//! Minimal mDNS (RFC 6762) / DNS-SD (RFC 6763), both directions:
//!
//! - Responder: advertises "<hostname>.local" (A record) and a `_pdmx._tcp`
//!   service named "PDMX Controller XXXX" pointing at the future web interface.
//! - Resolver: `resolve("fpp.local")` for the FPP command task.
//!
//! Both share one socket on purpose. smoltcp hands each datagram to a single
//! socket per port, so a second socket on 5353 would never see the replies to
//! our own queries.

use core::cell::RefCell;
use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_futures::select::{Either3, select3};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, ThreadModeRawMutex};
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use heapless::{String, Vec};
use static_cell::StaticCell;

use crate::periphs::eth::{NET_IDENTITY, NetIdentity};

const MDNS_PORT: u16 = 5353;
const MDNS_GROUP: Ipv4Address = Ipv4Address::new(224, 0, 0, 251);

const SERVICE_TYPE: &str = "_pdmx._tcp.local";
const META_QUERY: &str = "_services._dns-sd._udp.local";
const SERVICE_PORT: u16 = crate::periphs::http::HTTP_PORT;

/// Longest DNS name we bother to decode; anything longer can't be one of ours.
const MAX_NAME: usize = 128;

// RFC 6762 recommends 120s for records tied to a host address, 75 minutes for the rest.
const TTL_HOST: u32 = 120;
const TTL_OTHER: u32 = 4500;

const QUERY_ATTEMPTS: u8 = 3;
const QUERY_INTERVAL: Duration = Duration::from_secs(1);

/// The Falcon Player host to watch for; the web UI shows whether it's answering.
pub const FPP_HOST: &str = "fpp.local";
/// The tail of every instance name: "<instance>._pdmx._tcp.local".
const SERVICE_SUFFIX: &str = "._pdmx._tcp.local";
/// How often to ask who's out there.
const BROWSE_INTERVAL: Duration = Duration::from_secs(5);
/// Until FPP has answered, ask this often instead: the show logic waits on
/// FPP being found, and a missed first query would otherwise cost 5s.
const BROWSE_INTERVAL_NO_FPP: Duration = Duration::from_secs(1);
/// A peer (or FPP) not heard from for this long, i.e. a few missed browses, is
/// gone. We send no goodbye packets, so this is the only way one disappears.
const SEEN_TTL: Duration = Duration::from_secs(20);
pub const MAX_PEERS: usize = 8;
const PEER_NAME_LEN: usize = 32;

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_AAAA: u16 = 28;
const TYPE_SRV: u16 = 33;
const TYPE_NSEC: u16 = 47;
const TYPE_ANY: u16 = 255;

const CLASS_IN: u16 = 1;
/// Top bit of a question's class: "unicast response requested".
const QU: u16 = 0x8000;
/// Set on records that only this host answers for (everything but the shared PTRs).
const CACHE_FLUSH: u16 = 0x8000;

// Records we can emit, as a bitmask so a query can accumulate its answer set.
const REC_A: u8 = 1 << 0;
const REC_PTR: u8 = 1 << 1;
const REC_META_PTR: u8 = 1 << 2;
const REC_SRV: u8 = 1 << 3;
const REC_TXT: u8 = 1 << 4;
const REC_NSEC: u8 = 1 << 5;
// Emission order, so `build_response` can count and write records in one pass.
const REC_ORDER: [u8; 6] = [REC_A, REC_PTR, REC_META_PTR, REC_SRV, REC_TXT, REC_NSEC];

// -------------------------------------------------------------------------
// Resolver API
// -------------------------------------------------------------------------

static RESOLVE_LOCK: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());
static RESOLVE_REQ: Signal<CriticalSectionRawMutex, String<MAX_NAME>> = Signal::new();
static RESOLVE_RESP: Signal<CriticalSectionRawMutex, Option<Ipv4Address>> = Signal::new();

/// Looks up the IPv4 address of a `.local` name. Always sends a fresh query (no
/// cache), so a host that changed address is found again. `None` if nothing
/// answered within ~3 seconds.
pub async fn resolve(host: &str) -> Option<Ipv4Address> {
    let mut name: String<MAX_NAME> = String::new();
    name.push_str(host.trim_end_matches('.')).ok()?;

    // One lookup at a time: there's a single request/response slot.
    let _guard = RESOLVE_LOCK.lock().await;
    RESOLVE_RESP.reset();
    RESOLVE_REQ.signal(name);

    // The task always answers within QUERY_ATTEMPTS * QUERY_INTERVAL; the extra
    // time only matters if the task isn't running.
    let give_up = QUERY_INTERVAL * (QUERY_ATTEMPTS as u32 + 2);
    with_timeout(give_up, RESOLVE_RESP.wait()).await.ok().flatten()
}

// -------------------------------------------------------------------------
// Discovery API (other PDMX controllers + FPP, for the web UI)
// -------------------------------------------------------------------------

// ThreadModeRawMutex rather than a critical section: both users (this task and
// the web tasks) run in thread mode, and a critical section would mask the
// interrupts the audio DMA feed depends on. It panics if ever locked from an
// interrupt, which is the right failure for a mistake like that.
static DISCOVERY: BlockingMutex<ThreadModeRawMutex, RefCell<Discovery>> =
    BlockingMutex::new(RefCell::new(Discovery::new()));

/// Other PDMX controllers heard from recently.
pub fn peers() -> Vec<Peer, MAX_PEERS> {
    let now = Instant::now();
    DISCOVERY.lock(|d| {
        d.borrow().peers.iter().filter(|p| now - p.last_seen < SEEN_TTL).cloned().collect()
    })
}

/// FPP's address, if it has answered recently.
pub fn fpp_ip() -> Option<Ipv4Address> {
    let now = Instant::now();
    DISCOVERY.lock(|d| {
        let d = d.borrow();
        d.fpp_seen.filter(|&seen| now - seen < SEEN_TTL).and(d.fpp_ip)
    })
}

impl Discovery {
    /// Drops what's gone quiet so its slot can be reused.
    fn prune(&mut self, now: Instant) {
        self.peers.retain(|p| now - p.last_seen < SEEN_TTL);
        if self.fpp_seen.is_some_and(|seen| now - seen >= SEEN_TTL) {
            self.fpp_seen = None;
            self.fpp_ip = None;
        }
    }
}

// -------------------------------------------------------------------------
// Task
// -------------------------------------------------------------------------

struct Names {
    /// "pdmx-a1b2.local"
    host: String<24>,
    /// "PDMX Controller A1B2._pdmx._tcp.local"
    instance: String<48>,
}

impl Names {
    fn new(ident: &NetIdentity) -> Self {
        let mut host = String::new();
        let mut instance = String::new();
        write!(host, "{}.local", ident.hostname).unwrap();
        write!(instance, "{}.{}", ident.instance, SERVICE_TYPE).unwrap();
        Self { host, instance }
    }
}

struct Pending {
    name: String<MAX_NAME>,
    sent: u8,
    next_at: Instant,
}

#[embassy_executor::task]
pub async fn mdns_task(stack: Stack<'static>) -> ! {
    static RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
    static TX_META: StaticCell<[PacketMetadata; 2]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 3072]> = StaticCell::new();
    static TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();

    let ident = NET_IDENTITY.get().await;
    let names = Names::new(ident);

    let mut socket = UdpSocket::new(
        stack,
        RX_META.init([PacketMetadata::EMPTY; 4]),
        RX_BUF.init([0u8; 3072]),
        TX_META.init([PacketMetadata::EMPTY; 2]),
        TX_BUF.init([0u8; 1024]),
    );
    socket.bind(MDNS_PORT).unwrap();
    // RFC 6762 §11: mDNS packets are sent with TTL 255 so receivers can tell
    // they didn't cross a router.
    socket.set_hop_limit(Some(255));

    // Same reasoning as sACN: with no router there's no IGMP querier to ask
    // again if the join report is lost, so wait for link first.
    stack.wait_link_up().await;
    match stack.join_multicast_group(MDNS_GROUP) {
        Ok(()) => info!("mDNS: advertising {}", names.host.as_str()),
        Err(e) => warn!("mDNS: failed to join 224.0.0.251: {:?}", e),
    }

    let dest = IpEndpoint::new(IpAddress::Ipv4(MDNS_GROUP), MDNS_PORT);
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 1500];

    // Unsolicited announcement so browsers see us without having to ask.
    for _ in 0..2 {
        let all = REC_A | REC_PTR | REC_SRV | REC_TXT | REC_NSEC;
        if let Some(len) = build_response(&mut tx, &names, ident.ip, all, 0) {
            send(&socket, &tx[..len], dest).await;
        }
        Timer::after_secs(1).await;
    }

    let mut pending: Option<Pending> = None;
    let mut next_browse = Instant::now();

    loop {
        let wake = pending
            .as_ref()
            .map_or(next_browse, |p| p.next_at.min(next_browse));

        match select3(socket.recv_from(&mut rx), RESOLVE_REQ.wait(), Timer::at(wake)).await {
            Either3::First(Ok((n, _))) => {
                let packet = &rx[..n];

                let now = Instant::now();
                DISCOVERY.lock(|d| apply_response(packet, &ident.instance, &mut d.borrow_mut(), now));

                if let Some(p) = &pending {
                    if let Some(ip) = find_a_record(packet, &p.name) {
                        RESOLVE_RESP.signal(Some(ip));
                        pending = None;
                        continue;
                    }
                }

                if let Some(len) = answer_query(packet, &names, ident.ip, &mut tx) {
                    send(&socket, &tx[..len], dest).await;
                }
            }
            // A datagram bigger than `rx`; nothing useful to do with it.
            Either3::First(Err(_)) => {}
            Either3::Second(name) => {
                pending = Some(Pending { name, sent: 0, next_at: Instant::now() });
            }
            Either3::Third(()) => {
                let now = Instant::now();

                if let Some(p) = pending.as_mut().filter(|p| p.next_at <= now) {
                    if p.sent >= QUERY_ATTEMPTS {
                        RESOLVE_RESP.signal(None);
                        pending = None;
                    } else {
                        if let Some(len) = build_query(&mut tx, &[(p.name.as_str(), TYPE_A)], false) {
                            send(&socket, &tx[..len], dest).await;
                        }
                        p.sent += 1;
                        p.next_at = now + QUERY_INTERVAL;
                    }
                }

                if next_browse <= now {
                    // One packet asks for both: other controllers and FPP.
                    let questions = [(SERVICE_TYPE, TYPE_PTR), (FPP_HOST, TYPE_A)];
                    // Until FPP has answered, ask for direct (unicast)
                    // replies: the show waits on finding FPP, and RFC 6762
                    // recommends QU for a host's first queries anyway.
                    let fpp_known = fpp_ip().is_some();
                    if let Some(len) = build_query(&mut tx, &questions, !fpp_known) {
                        send(&socket, &tx[..len], dest).await;
                    }
                    DISCOVERY.lock(|d| d.borrow_mut().prune(now));
                    let interval = if fpp_ip().is_some() { BROWSE_INTERVAL } else { BROWSE_INTERVAL_NO_FPP };
                    next_browse = now + interval;
                }
            }
        }
    }
}

async fn send(socket: &UdpSocket<'_>, packet: &[u8], dest: IpEndpoint) {
    if let Err(e) = socket.send_to(packet, dest).await {
        warn!("mDNS: send failed: {:?}", e);
    }
}

// -------------------------------------------------------------------------
// Responder: incoming query -> response
// -------------------------------------------------------------------------

/// Builds the response to a query packet, or `None` if it isn't a query, has
/// nothing for us, or the answer doesn't fit.
fn answer_query(packet: &[u8], names: &Names, ip: Ipv4Address, out: &mut [u8]) -> Option<usize> {
    // QR bit set = response; opcode must be 0 (standard query).
    if packet.len() < 12 || packet[2] & 0x80 != 0 || packet[2] & 0x78 != 0 {
        return None;
    }

    let question_count = u16::from_be_bytes([packet[4], packet[5]]);
    let mut pos = 12;
    let mut name: String<MAX_NAME> = String::new();
    let mut answers = 0u8;
    let mut additional = 0u8;

    for _ in 0..question_count {
        pos = read_name(packet, pos, &mut name)?;
        let qtype = u16::from_be_bytes([*packet.get(pos)?, *packet.get(pos + 1)?]);
        // Skip type + class; the QU (unicast-response) bit is ignored, we always
        // multicast.
        pos += 4;

        let any = qtype == TYPE_ANY;

        if name.eq_ignore_ascii_case(&names.host) {
            if qtype == TYPE_A || any {
                answers |= REC_A;
                additional |= REC_NSEC;
            } else if qtype == TYPE_AAAA {
                // We have no IPv6. Say so explicitly (NSEC) so resolvers that
                // asked for A and AAAA together don't wait for the AAAA.
                answers |= REC_NSEC;
            }
        } else if name.eq_ignore_ascii_case(SERVICE_TYPE) {
            if qtype == TYPE_PTR || any {
                answers |= REC_PTR;
                additional |= REC_SRV | REC_TXT | REC_A | REC_NSEC;
            }
        } else if name.eq_ignore_ascii_case(META_QUERY) {
            if qtype == TYPE_PTR || any {
                answers |= REC_META_PTR;
            }
        } else if name.eq_ignore_ascii_case(&names.instance) {
            if qtype == TYPE_SRV || any {
                answers |= REC_SRV;
                additional |= REC_A | REC_NSEC;
            }
            if qtype == TYPE_TXT || any {
                answers |= REC_TXT;
            }
        }
    }

    if answers == 0 {
        return None;
    }
    build_response(out, names, ip, answers, additional & !answers)
}

fn build_response(out: &mut [u8], names: &Names, ip: Ipv4Address, answers: u8, additional: u8) -> Option<usize> {
    let mut w = Writer { buf: out, pos: 0 };

    // id 0, flags = response + authoritative, no questions.
    w.u16(0)?;
    w.u16(0x8400)?;
    w.u16(0)?;
    w.u16(answers.count_ones() as u16)?;
    w.u16(0)?;
    w.u16(additional.count_ones() as u16)?;

    for set in [answers, additional] {
        for rec in REC_ORDER {
            if set & rec != 0 {
                write_record(&mut w, rec, names, ip)?;
            }
        }
    }
    Some(w.pos)
}

fn write_record(w: &mut Writer, rec: u8, names: &Names, ip: Ipv4Address) -> Option<()> {
    match rec {
        REC_A => {
            let at = w.record_start(&names.host, TYPE_A, CLASS_IN | CACHE_FLUSH, TTL_HOST)?;
            w.bytes(&ip.octets())?;
            w.record_end(at);
        }
        REC_PTR => {
            let at = w.record_start(SERVICE_TYPE, TYPE_PTR, CLASS_IN, TTL_OTHER)?;
            w.name(&names.instance)?;
            w.record_end(at);
        }
        REC_META_PTR => {
            let at = w.record_start(META_QUERY, TYPE_PTR, CLASS_IN, TTL_OTHER)?;
            w.name(SERVICE_TYPE)?;
            w.record_end(at);
        }
        REC_SRV => {
            let at = w.record_start(&names.instance, TYPE_SRV, CLASS_IN | CACHE_FLUSH, TTL_HOST)?;
            w.u16(0)?; // priority
            w.u16(0)?; // weight
            w.u16(SERVICE_PORT)?;
            w.name(&names.host)?;
            w.record_end(at);
        }
        REC_TXT => {
            let at = w.record_start(&names.instance, TYPE_TXT, CLASS_IN | CACHE_FLUSH, TTL_OTHER)?;
            w.bytes(&[9])?;
            w.bytes(b"txtvers=1")?;
            w.record_end(at);
        }
        REC_NSEC => {
            // "This name has an A record and nothing else" (type bitmap: window
            // 0, 1 byte, bit for type 1).
            let at = w.record_start(&names.host, TYPE_NSEC, CLASS_IN | CACHE_FLUSH, TTL_HOST)?;
            w.name(&names.host)?;
            w.bytes(&[0, 1, 0x40])?;
            w.record_end(at);
        }
        _ => {}
    }
    Some(())
}

// -------------------------------------------------------------------------
// Resolver: query out, response in
// -------------------------------------------------------------------------

/// `unicast`: set the QU bit (RFC 6762 §5.4) so responders answer straight to
/// us instead of multicasting.
fn build_query(out: &mut [u8], questions: &[(&str, u16)], unicast: bool) -> Option<usize> {
    let mut w = Writer { buf: out, pos: 0 };
    w.u16(0)?; // id
    w.u16(0)?; // flags: standard query
    w.u16(questions.len() as u16)?;
    w.bytes(&[0; 6])?; // no answer/authority/additional records
    for (name, qtype) in questions {
        w.name(name)?;
        w.u16(*qtype)?;
        w.u16(if unicast { CLASS_IN | QU } else { CLASS_IN })?;
    }
    Some(w.pos)
}

/// The IPv4 address carried by an A record for `wanted` anywhere in a response
/// packet (answer, authority or additional section).
fn find_a_record(packet: &[u8], wanted: &str) -> Option<Ipv4Address> {
    let mut found = None;
    // A malformed tail doesn't invalidate a match that came before it.
    let _ = for_each_record(packet, |name, rtype, at, len| {
        if found.is_none() && rtype == TYPE_A && len == 4 && name.eq_ignore_ascii_case(wanted) {
            found = a_record(packet, at);
        }
    });
    found
}

fn a_record(packet: &[u8], at: usize) -> Option<Ipv4Address> {
    let octets = packet.get(at..at + 4)?;
    Some(Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]))
}

/// Calls `f(name, type, rdata_offset, rdata_len)` for every record in a response
/// packet, in any section. `None` if it isn't a response or is malformed; records
/// before the malformed part have already been visited.
fn for_each_record(packet: &[u8], mut f: impl FnMut(&str, u16, usize, usize)) -> Option<()> {
    if packet.len() < 12 || packet[2] & 0x80 == 0 {
        return None;
    }

    let question_count = u16::from_be_bytes([packet[4], packet[5]]);
    let record_count = u16::from_be_bytes([packet[6], packet[7]]) as usize
        + u16::from_be_bytes([packet[8], packet[9]]) as usize
        + u16::from_be_bytes([packet[10], packet[11]]) as usize;

    let mut pos = 12;
    let mut name: String<MAX_NAME> = String::new();

    for _ in 0..question_count {
        pos = read_name(packet, pos, &mut name)? + 4;
    }

    for _ in 0..record_count {
        pos = read_name(packet, pos, &mut name)?;
        let fixed = packet.get(pos..pos + 10)?; // type, class, ttl, rdlength
        let rtype = u16::from_be_bytes([fixed[0], fixed[1]]);
        let rdlen = u16::from_be_bytes([fixed[8], fixed[9]]) as usize;
        pos += 10;
        packet.get(pos..pos + rdlen)?;
        f(&name, rtype, pos, rdlen);
        pos += rdlen;
    }
    Some(())
}

// -------------------------------------------------------------------------
// Discovery: what other controllers / FPP are saying
// -------------------------------------------------------------------------

/// Another PDMX controller on the network.
#[derive(Clone)]
pub struct Peer {
    /// "PDMX Controller A1B2"
    pub instance: String<PEER_NAME_LEN>,
    /// "pdmx-a1b2.local"; empty until its SRV record has been seen.
    pub host: String<PEER_NAME_LEN>,
    pub ip: Option<Ipv4Address>,
    last_seen: Instant,
}

struct Discovery {
    peers: Vec<Peer, MAX_PEERS>,
    fpp_ip: Option<Ipv4Address>,
    fpp_seen: Option<Instant>,
}

impl Discovery {
    const fn new() -> Self {
        Self { peers: Vec::new(), fpp_ip: None, fpp_seen: None }
    }

    /// The peer called `label`, added if there's room, and marked as just seen.
    fn touch(&mut self, label: &str, now: Instant) -> Option<&mut Peer> {
        let index = match self.peers.iter().position(|p| p.instance.as_str() == label) {
            Some(index) => index,
            None => {
                let mut instance = String::new();
                instance.push_str(label).ok()?;
                let peer = Peer { instance, host: String::new(), ip: None, last_seen: now };
                self.peers.push(peer).ok()?;
                self.peers.len() - 1
            }
        };
        let peer = &mut self.peers[index];
        peer.last_seen = now;
        Some(peer)
    }
}

/// "PDMX Controller A1B2" out of "PDMX Controller A1B2._pdmx._tcp.local".
fn instance_label(fqdn: &str) -> Option<&str> {
    let cut = fqdn.len().checked_sub(SERVICE_SUFFIX.len())?;
    let (label, suffix) = (fqdn.get(..cut)?, fqdn.get(cut..)?);
    (!label.is_empty() && suffix.eq_ignore_ascii_case(SERVICE_SUFFIX)).then_some(label)
}

/// Learns from any response on the wire, ours or someone else's: which PDMX
/// controllers exist, where they live, and whether FPP is answering. Everything
/// in `packet` is untrusted, so every lookup is bounds-checked and a bad packet
/// just teaches us nothing.
fn apply_response(packet: &[u8], own_instance: &str, d: &mut Discovery, now: Instant) {
    // Two passes so it doesn't matter what order the sender listed records in:
    // first who exists and their host names, then host addresses.
    let _ = for_each_record(packet, |name, rtype, at, len| {
        let mut target: String<MAX_NAME> = String::new();
        match rtype {
            TYPE_PTR if name.eq_ignore_ascii_case(SERVICE_TYPE) => {
                if read_name(packet, at, &mut target).is_none() {
                    return;
                }
                if let Some(label) = instance_label(&target).filter(|l| *l != own_instance) {
                    d.touch(label, now);
                }
            }
            TYPE_SRV if len > 6 => {
                let Some(label) = instance_label(name).filter(|l| *l != own_instance) else {
                    return;
                };
                // SRV rdata: priority, weight, port, then the target host name.
                if read_name(packet, at + 6, &mut target).is_none() {
                    return;
                }
                if let Some(peer) = d.touch(label, now) {
                    peer.host.clear();
                    // Too long for the field just leaves it empty.
                    let _ = peer.host.push_str(&target);
                }
            }
            _ => {}
        }
    });

    let _ = for_each_record(packet, |name, rtype, at, len| {
        if rtype != TYPE_A || len != 4 {
            return;
        }
        let Some(ip) = a_record(packet, at) else {
            return;
        };

        if name.eq_ignore_ascii_case(FPP_HOST) {
            d.fpp_ip = Some(ip);
            d.fpp_seen = Some(now);
        }
        for peer in d.peers.iter_mut() {
            if peer.host.eq_ignore_ascii_case(name) {
                peer.ip = Some(ip);
            }
        }
    });
}

// -------------------------------------------------------------------------
// Wire format helpers
// -------------------------------------------------------------------------

/// Decodes the (possibly compressed) name at `start` into dotted form, without
/// a trailing dot. Returns the offset just past the name in the original
/// stream. A name too long for `out` decodes to an empty string, which never
/// matches anything, but is still skipped correctly.
fn read_name(packet: &[u8], start: usize, out: &mut String<MAX_NAME>) -> Option<usize> {
    out.clear();

    let mut pos = start;
    let mut end = None;
    let mut jumps = 0;
    let mut overflow = false;

    loop {
        let len = *packet.get(pos)? as usize;

        if len == 0 {
            pos += 1;
            break;
        }

        if len & 0xC0 == 0xC0 {
            let low = *packet.get(pos + 1)? as usize;
            end.get_or_insert(pos + 2);
            jumps += 1;
            if jumps > 8 {
                return None;
            }
            pos = ((len & 0x3F) << 8) | low;
            continue;
        }

        if len & 0xC0 != 0 {
            return None;
        }

        let label = packet.get(pos + 1..pos + 1 + len)?;
        if !overflow {
            let fits = (out.is_empty() || out.push('.').is_ok())
                && core::str::from_utf8(label).is_ok_and(|s| out.push_str(s).is_ok());
            overflow = !fits;
        }
        pos += 1 + len;
    }

    if overflow {
        out.clear();
    }
    Some(end.unwrap_or(pos))
}

struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl Writer<'_> {
    fn bytes(&mut self, data: &[u8]) -> Option<()> {
        let end = self.pos.checked_add(data.len())?;
        self.buf.get_mut(self.pos..end)?.copy_from_slice(data);
        self.pos = end;
        Some(())
    }

    fn u16(&mut self, v: u16) -> Option<()> {
        self.bytes(&v.to_be_bytes())
    }

    fn u32(&mut self, v: u32) -> Option<()> {
        self.bytes(&v.to_be_bytes())
    }

    /// Writes a dotted name uncompressed (packets are small; compression isn't
    /// worth the code).
    fn name(&mut self, name: &str) -> Option<()> {
        for label in name.split('.') {
            if label.is_empty() || label.len() > 63 {
                return None;
            }
            self.bytes(&[label.len() as u8])?;
            self.bytes(label.as_bytes())?;
        }
        self.bytes(&[0])
    }

    /// Writes name/type/class/ttl and a placeholder rdlength. Returns where the
    /// rdlength lives, for `record_end`.
    fn record_start(&mut self, name: &str, rtype: u16, class: u16, ttl: u32) -> Option<usize> {
        self.name(name)?;
        self.u16(rtype)?;
        self.u16(class)?;
        self.u32(ttl)?;
        let len_at = self.pos;
        self.u16(0)?;
        Some(len_at)
    }

    fn record_end(&mut self, len_at: usize) {
        let len = (self.pos - len_at - 2) as u16;
        self.buf[len_at..len_at + 2].copy_from_slice(&len.to_be_bytes());
    }
}
