//! Falcon Player (FPP) control over its HTTP API, used by the show logic in
//! `logic.rs`.
//!
//! The helpers below queue a command and return immediately; `fpp_task` sends
//! them in order, one GET per connection. A command that can't reach FPP (name
//! not found over mDNS, connect or write failed) is retried until it gets
//! through, so commands issued at boot - before FPP has finished starting - still
//! land once it's up, and presses made while it's down replay in order. A
//! command FPP answers with an error status is logged and dropped: retrying
//! won't fix a misspelled name.
//!
//! Sequence and effect names are the file name without `.fseq`.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU8, Ordering};

use defmt::{info, warn};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack, tcp::TcpSocket};
use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, channel::Channel};
use embassy_time::{Duration, Timer, with_timeout};
use heapless::String;

use crate::config::MAX_HOSTNAME_LEN;
use crate::periphs::mdns;

// -------------------------------------------------------------------------
// Commands
// -------------------------------------------------------------------------

/// Plays a sequence as a one-item playlist, replacing whatever is playing.
pub fn start_sequence(name: &str, looping: bool) {
    queue(format_args!(
        "/api/command/Start%20Playlist/{}.fseq/{}/false",
        Encoded(name),
        looping,
    ));
}

/// Stops the playing sequence (running effects keep going).
#[allow(dead_code)] // available to logic.rs
pub fn stop_sequence() {
    queue(format_args!("/api/playlists/stop"));
}

/// Starts an effect on top of whatever sequence is playing.
pub fn start_effect(name: &str, looping: bool) {
    queue(format_args!(
        "/api/command/FSEQ%20Effect%20Start/{}/{}/0",
        Encoded(name),
        looping as u8,
    ));
}

#[allow(dead_code)] // available to logic.rs
pub fn stop_effect(name: &str) {
    queue(format_args!("/api/command/Effect%20Stop/{}", Encoded(name)));
}

pub fn stop_all_effects() {
    queue(format_args!("/api/command/Effects%20Stop"));
}

/// Holds back the commands queued after this one for `delay`, counted from
/// when FPP accepted the command before it.
pub fn wait(delay: Duration) {
    if COMMANDS.try_send(Command::Wait(delay)).is_err() {
        warn!("FPP: command queue full, wait dropped");
    }
}

// -------------------------------------------------------------------------
// Status (for the OLED / web page)
// -------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u8)]
pub enum FppStatus {
    /// Nothing sent yet.
    Idle,
    /// mDNS found nothing for the configured name; retrying.
    NoHost,
    /// Address known, but the connection failed; retrying.
    NoConn,
    /// Last command was accepted.
    Online,
    /// FPP answered the last command with an error (bad name?). Not retried.
    Rejected,
}

static STATUS: AtomicU8 = AtomicU8::new(FppStatus::Idle as u8);

fn set_status(status: FppStatus) {
    STATUS.store(status as u8, Ordering::Relaxed);
}

pub fn fpp_status() -> FppStatus {
    match STATUS.load(Ordering::Relaxed) {
        x if x == FppStatus::NoHost as u8 => FppStatus::NoHost,
        x if x == FppStatus::NoConn as u8 => FppStatus::NoConn,
        x if x == FppStatus::Online as u8 => FppStatus::Online,
        x if x == FppStatus::Rejected as u8 => FppStatus::Rejected,
        _ => FppStatus::Idle,
    }
}

// -------------------------------------------------------------------------
// Queue + sender
// -------------------------------------------------------------------------

const FPP_PORT: u16 = 80;
const RETRY_DELAY: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(3);

type Path = String<192>;

enum Command {
    Get(Path),
    Wait(Duration),
}

/// ThreadModeRawMutex, like mdns: every sender and the receiver run in thread
/// mode, and a critical section would mask the audio DMA interrupts.
static COMMANDS: Channel<ThreadModeRawMutex, Command, 16> = Channel::new();

fn queue(args: fmt::Arguments) {
    let mut path = Path::new();
    if path.write_fmt(args).is_err() {
        warn!("FPP: command path too long, dropped");
        return;
    }
    if COMMANDS.try_send(Command::Get(path)).is_err() {
        warn!("FPP: command queue full, dropped");
    }
}

#[embassy_executor::task]
pub async fn fpp_task(stack: Stack<'static>, host: String<MAX_HOSTNAME_LEN>) -> ! {
    let mut rx_buffer = [0u8; 512];
    let mut tx_buffer = [0u8; 512];

    // Looked up again after any connection failure, in case FPP came back at a
    // different link-local address.
    let mut ip: Option<Ipv4Address> = None;

    loop {
        let path = match COMMANDS.receive().await {
            Command::Get(path) => path,
            Command::Wait(delay) => {
                Timer::after(delay).await;
                continue;
            }
        };

        loop {
            let addr = match ip {
                Some(addr) => addr,
                None => match lookup(&host).await {
                    Some(addr) => *ip.insert(addr),
                    None => {
                        warn!("FPP: no answer for {}. Retrying...", host.as_str());
                        set_status(FppStatus::NoHost);
                        Timer::after(RETRY_DELAY).await;
                        continue;
                    }
                },
            };

            match get(stack, &mut rx_buffer, &mut tx_buffer, addr, &host, &path).await {
                Ok(code) if (200..300).contains(&code) => {
                    info!("FPP: {} -> {}", path.as_str(), code);
                    set_status(FppStatus::Online);
                    break;
                }
                Ok(code) => {
                    warn!("FPP: {} -> {} (rejected, not retrying)", path.as_str(), code);
                    set_status(FppStatus::Rejected);
                    break;
                }
                Err(()) => {
                    warn!("FPP: {} failed to reach {}. Retrying...", path.as_str(), addr);
                    set_status(FppStatus::NoConn);
                    ip = None;
                    Timer::after(RETRY_DELAY).await;
                }
            }
        }
    }
}

async fn lookup(host: &str) -> Option<Ipv4Address> {
    match host.parse::<Ipv4Address>() {
        Ok(ip) => Some(ip),
        Err(_) => mdns::resolve(host).await,
    }
}

/// One HTTP/1.0 GET. `Ok(status code)` if FPP answered, `Err` if it couldn't be
/// reached or the reply was unreadable.
async fn get(
    stack: Stack<'static>,
    rx_buffer: &mut [u8],
    tx_buffer: &mut [u8],
    ip: Ipv4Address,
    host: &str,
    path: &str,
) -> Result<u16, ()> {
    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(5)));

    let result = async {
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(ip), FPP_PORT);
        match with_timeout(CONNECT_TIMEOUT, socket.connect(endpoint)).await {
            Ok(Ok(())) => {}
            _ => return Err(()),
        }

        let mut request: String<320> = String::new();
        write!(request, "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n", path, host)
            .map_err(|_| ())?;

        let bytes = request.as_bytes();
        let mut sent = 0;
        while sent < bytes.len() {
            match with_timeout(IO_TIMEOUT, socket.write(&bytes[sent..])).await {
                Ok(Ok(n)) if n > 0 => sent += n,
                _ => return Err(()),
            }
        }

        // Only the status line matters: "HTTP/1.1 200 OK".
        let mut reply = [0u8; 16];
        let mut got = 0;
        while got < reply.len() {
            match with_timeout(IO_TIMEOUT, socket.read(&mut reply[got..])).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => got += n,
                _ => return Err(()),
            }
        }
        status_code(&reply[..got]).ok_or(())
    }
    .await;

    // The command has run by the time FPP replies, and the rest of the body is
    // of no interest, so just drop the connection.
    socket.abort();
    let _ = with_timeout(Duration::from_millis(100), socket.flush()).await;
    result
}

fn status_code(reply: &[u8]) -> Option<u16> {
    let line = core::str::from_utf8(reply).ok()?;
    let mut words = line.split(' ');
    if !words.next()?.starts_with("HTTP/") {
        return None;
    }
    words.next()?.get(..3)?.parse().ok()
}

/// Percent-encodes a path segment, so names with spaces or `/` stay one
/// segment.
struct Encoded<'a>(&'a str);

impl fmt::Display for Encoded<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                f.write_char(b as char)?;
            } else {
                write!(f, "%{:02X}", b)?;
            }
        }
        Ok(())
    }
}
