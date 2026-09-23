//! Bare-bones status page on port 80 - just enough to prove the board is
//! reachable by name (`http://pdmx-xxxx.local/`). One request per connection,
//! HTTP/1.0 with `Connection: close`, nothing parsed beyond the request line.
//!
//!   GET /        static page; a few lines of JS poll /status once a second
//!   GET /status  live values as JSON
//!
//! Kept cheap on purpose: every await here is network I/O with a timeout, the
//! idle cost is a listening socket, and a request formats a few hundred bytes
//! from atomics, so it can't starve the audio/LED/DMX tasks sharing the
//! executor. Each open browser tab costs one short connection per second.

use core::fmt::{self, Write};

use embassy_net::{Stack, tcp::{State, TcpSocket}};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use heapless::String;

use crate::config::InputProtocol;
use crate::periphs::eth::{NET_IDENTITY, NetIdentity};
use crate::periphs::mdns;
use crate::periphs::sensors::*;
use crate::periphs::fpp::{FppStatus, fpp_status};

/// Also the port advertised over mDNS.
pub const HTTP_PORT: u16 = 80;

const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// Whole HTTP response (headers + body) is built in one buffer.
type Response = String<2048>;

/// Stop adding peers to /status when this little room is left, so a pile of
/// long or escape-heavy names truncates the list instead of losing the reply.
const PEER_ROOM: usize = 256;

/// Polls /status once a second. A `setTimeout` chain instead of `setInterval`
/// so a slow response can never stack up overlapping requests.
///
/// Each request is aborted after 1.5s (a browser will otherwise wait a minute or
/// more on an unreachable host), and a separate 0.5s watchdog flips the page to
/// OFFLINE once nothing has come back for 2s, whether or not a request is
/// still hanging. Stale values are dimmed rather than cleared.
///
/// Peer names come off the network, so they only ever go in via `textContent` /
/// `append` of a string, never as HTML.
const PAGE_SCRIPT: &str = r#"<script>
let last=Date.now();
function A(ip,text){let a=document.createElement('a');a.href='http://'+ip+'/';a.textContent=text;return a}
function off(){let d=Date.now()-last,o=d>2000;
m.style.opacity=o?.35:1;e.textContent=o?'OFFLINE - no response for '+Math.floor(d/1000)+'s':''}
async function t(){let ctl=new AbortController(),to=setTimeout(()=>ctl.abort(),1500);try{
let s=await(await fetch('/status',{cache:'no-store',signal:ctl.signal})).json();
u.textContent=s.up;i.textContent=s.input;g.textContent=s.state;c.textContent=s.fppcmd;
b.textContent=s.buttons.map((x,n)=>(n+1)+(x?'●':'○')).join('  ');
if(s.fpp)f.replaceChildren(A(s.fpp,'online ('+s.fpp+')'));else f.textContent='offline';
l.replaceChildren(...s.peers.map(q=>{let li=document.createElement('li');li.append(q.n+' - ');
li.append(q.ip?A(q.ip,q.h+' ('+q.ip+')'):q.h);return li}));
if(!s.peers.length)l.textContent='none found';
last=Date.now()
}catch(_){}
clearTimeout(to);off();setTimeout(t,1000)}
setInterval(off,500);t()
</script>"#;

/// Two listeners: browsers open a second, idle speculative connection alongside
/// the real one, and with a single listener that idle one can hold up the page
/// for a full read timeout.
#[embassy_executor::task(pool_size = 2)]
pub async fn http_task(stack: Stack<'static>) -> ! {
    let mut rx_buffer = [0u8; 1024];
    let mut tx_buffer = [0u8; 1024];

    let ident = NET_IDENTITY.get().await;

    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        // Aborts the connection if our response sits unacknowledged this long.
        socket.set_timeout(Some(Duration::from_secs(5)));

        if socket.accept(HTTP_PORT).await.is_err() {
            Timer::after_millis(100).await;
            continue;
        }

        serve(&mut socket, ident).await;
        finish(&mut socket).await;
    }
}

async fn serve(socket: &mut TcpSocket<'_>, ident: &NetIdentity) {
    // Only the request line matters; the rest of the headers stay in the
    // receive buffer unread.
    let mut request = [0u8; 128];
    let n = match with_timeout(IO_TIMEOUT, socket.read(&mut request)).await {
        Ok(Ok(n)) if n > 0 => n,
        _ => return,
    };
    let request = &request[..n];

    let mut response = Response::new();
    let built = if request.starts_with(b"GET / ") {
        write_page(&mut response, ident)
    } else if request.starts_with(b"GET /status ") {
        write_status(&mut response)
    } else {
        // Keeps favicon.ico and friends from getting the full page.
        push(&mut response, "HTTP/1.0 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
    };
    if built.is_err() {
        return;
    }

    let bytes = response.as_bytes();
    let mut sent = 0;
    while sent < bytes.len() {
        match with_timeout(IO_TIMEOUT, socket.write(&bytes[sent..])).await {
            Ok(Ok(n)) if n > 0 => sent += n,
            _ => return,
        }
    }
}

/// Closes gracefully. Dropping or aborting straight after the write can make the
/// peer see a reset before it has read the reply (Windows discards unread data on
/// RST), so wait for the FIN exchange and only abort if the peer never answers.
async fn finish(socket: &mut TcpSocket<'_>) {
    let _ = with_timeout(IO_TIMEOUT, socket.flush()).await;
    socket.close();

    let _ = with_timeout(Duration::from_secs(2), async {
        while !matches!(socket.state(), State::TimeWait | State::Closed) {
            Timer::after_millis(10).await;
        }
    })
    .await;

    socket.abort();
}

fn push(out: &mut Response, s: &str) -> fmt::Result {
    out.push_str(s).map_err(|_| fmt::Error)
}

fn write_page(out: &mut Response, ident: &NetIdentity) -> fmt::Result {
    push(out, "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n")?;
    write!(
        out,
        "<!doctype html><meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>{instance}</title>\
         <body style=\"font-family:sans-serif;margin:2em\">\
         <p id=e style=\"color:#b00;font-weight:bold\"></p>\
         <div id=m><h1>{instance}</h1>\
         <p>{host}.local &middot; {ip}</p>\
         <p>Uptime: <span id=u></span>s</p>\
         <p>Input: <span id=i></span></p>\
         <p>Show state: <span id=g></span></p>\
         <p>FPP commands: <span id=c></span></p>\
         <p>Buttons: <span id=b style=\"white-space:pre\"></span></p>\
         <p>FPP ({fpp}): <span id=f></span></p>\
         <h3>Other controllers</h3><ul id=l></ul></div>",
        instance = ident.instance,
        host = ident.hostname,
        ip = ident.ip,
        fpp = mdns::FPP_HOST,
    )?;
    push(out, PAGE_SCRIPT)
}

fn write_status(out: &mut Response) -> fmt::Result {
    let source = crate::CONFIG.try_get().map(|c| c.input.source);
    let input = match source {
        Some(InputProtocol::Dmx) => "DMX",
        Some(InputProtocol::Artnet) => "Art-Net",
        Some(InputProtocol::sACN) => "sACN",
        Some(InputProtocol::Sd) => "SD card",
        None => "unknown",
    };
    let data = match source {
        Some(InputProtocol::Artnet | InputProtocol::sACN) if crate::input_active() => ", receiving data",
        Some(InputProtocol::Artnet | InputProtocol::sACN) => ", no data",
        _ => "",
    };
    let fpp_cmd = match fpp_status() {
        FppStatus::Idle => "none sent yet",
        FppStatus::NoHost => "host not found, retrying",
        FppStatus::NoConn => "cannot connect, retrying",
        FppStatus::Online => "ok",
        FppStatus::Rejected => "last command rejected",
    };

    push(
        out,
        "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
    )?;
    write!(
        out,
        "{{\"up\":{},\"input\":\"{}{}\",\"fppcmd\":\"{}\",\"state\":\"",
        Instant::now().as_secs(),
        input,
        data,
        fpp_cmd,
    )?;
    match logic_state() {
        Some(state) => write!(out, "{:?}", state)?,
        None => push(out, "starting")?,
    }
    push(out, "\",\"buttons\":[")?;
    for n in 1..=6 {
        let sep = if n == 1 { "" } else { "," };
        write!(out, "{}{}", sep, button_active(n))?;
    }

    push(out, "],\"fpp\":")?;
    match mdns::fpp_ip() {
        Some(ip) => write!(out, "\"{}\"", ip)?,
        None => push(out, "null")?,
    }

    push(out, ",\"peers\":[")?;
    for (n, peer) in mdns::peers().iter().enumerate() {
        if out.capacity() - out.len() < PEER_ROOM {
            break;
        }
        if n > 0 {
            push(out, ",")?;
        }
        push(out, "{\"n\":")?;
        write_json_str(out, &peer.instance)?;
        push(out, ",\"h\":")?;
        write_json_str(out, &peer.host)?;
        match peer.ip {
            Some(ip) => write!(out, ",\"ip\":\"{}\"}}", ip)?,
            None => push(out, ",\"ip\":null}")?,
        }
    }
    push(out, "]}")
}

/// Writes `s` as a JSON string. Peer names are attacker-controlled network
/// data, so quotes, backslashes and control characters must be escaped.
fn write_json_str(out: &mut Response, s: &str) -> fmt::Result {
    push(out, "\"")?;
    for c in s.chars() {
        match c {
            '"' => push(out, "\\\"")?,
            '\\' => push(out, "\\\\")?,
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32)?,
            c => out.push(c).map_err(|_| fmt::Error)?,
        }
    }
    push(out, "\"")
}
