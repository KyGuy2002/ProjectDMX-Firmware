//! Minimal mDNS (RFC 6762) / DNS-SD (RFC 6763), both directions:
//!
//! - Responder: advertises "<hostname>.local" (A record) and a `_pdmx._tcp`
//!   service named "PDMX Controller XXXX" pointing at the future web interface.
//! - Resolver: `resolve("fpp.local")` for the TCP command task.
//!
//! Both share one socket on purpose. smoltcp hands each datagram to a single
//! socket per port, so a second socket on 5353 would never see the replies to
//! our own queries.

use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_futures::select::{Either3, select3};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use heapless::String;
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

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_AAAA: u16 = 28;
const TYPE_SRV: u16 = 33;
const TYPE_NSEC: u16 = 47;
const TYPE_ANY: u16 = 255;

const CLASS_IN: u16 = 1;
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

    loop {
        let wake = pending
            .as_ref()
            .map_or(Instant::now() + Duration::from_secs(3600), |p| p.next_at);

        match select3(socket.recv_from(&mut rx), RESOLVE_REQ.wait(), Timer::at(wake)).await {
            Either3::First(Ok((n, _))) => {
                let packet = &rx[..n];

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
                if let Some(p) = pending.as_mut() {
                    if p.sent >= QUERY_ATTEMPTS {
                        RESOLVE_RESP.signal(None);
                        pending = None;
                    } else {
                        if let Some(len) = build_query(&mut tx, &p.name) {
                            send(&socket, &tx[..len], dest).await;
                        }
                        p.sent += 1;
                        p.next_at = Instant::now() + QUERY_INTERVAL;
                    }
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

fn build_query(out: &mut [u8], name: &str) -> Option<usize> {
    let mut w = Writer { buf: out, pos: 0 };
    w.u16(0)?; // id
    w.u16(0)?; // flags: standard query
    w.u16(1)?; // one question
    w.bytes(&[0; 6])?; // no answer/authority/additional records
    w.name(name)?;
    w.u16(TYPE_A)?;
    w.u16(CLASS_IN)?;
    Some(w.pos)
}

/// The IPv4 address carried by an A record for `wanted` anywhere in a response
/// packet (answer, authority or additional section).
fn find_a_record(packet: &[u8], wanted: &str) -> Option<Ipv4Address> {
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
        let rdata = packet.get(pos..pos + rdlen)?;
        pos += rdlen;

        if rtype == TYPE_A && rdlen == 4 && name.eq_ignore_ascii_case(wanted) {
            return Some(Ipv4Address::new(rdata[0], rdata[1], rdata[2], rdata[3]));
        }
    }
    None
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
