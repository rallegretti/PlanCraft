#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! PlanCraft's own mDNS (`plancraft_server::mdns`): packets come from anyone on the network, so
//! whatever they hold, reading them never panics, and what a listener keeps stays bounded.

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use plancraft_server::mdns::{self, Data, Heard, Message, Name, Record};
use proptest::prelude::*;

fn service() -> Name {
    Name::dotted("_plancraft._tcp.local.")
}

/// A packet that looks like mDNS on the wire: a real header, then names made of labels and
/// pointers (forwards, backwards, to themselves), types PlanCraft reads, and lengths that may lie.
fn packetish() -> impl Strategy<Value = Vec<u8>> {
    let label = prop_oneof![
        Just(b"_plancraft".to_vec()),
        Just(b"_tcp".to_vec()),
        Just(b"local".to_vec()),
        proptest::collection::vec(any::<u8>(), 0..70),
    ];
    let piece = prop_oneof![
        label.prop_map(|l| {
            let mut v = vec![l.len().min(255) as u8];
            v.extend(l);
            v
        }),
        any::<u16>().prop_map(|p| (0xC000 | (p & 0x3FFF)).to_be_bytes().to_vec()),
        Just(vec![0u8]),
        proptest::collection::vec(any::<u8>(), 0..12),
    ];
    let types = prop_oneof![Just(1u16), Just(12), Just(16), Just(28), Just(33), Just(255), any::<u16>()];
    let record = (
        proptest::collection::vec(piece, 0..6),
        types,
        any::<u16>(),
        any::<u32>(),
        proptest::collection::vec(any::<u8>(), 0..40),
        any::<u16>(),
    )
        .prop_map(|(name, t, class, ttl, rdata, lie)| {
            let mut v: Vec<u8> = name.concat();
            v.extend_from_slice(&t.to_be_bytes());
            v.extend_from_slice(&(if class % 3 == 0 { class } else { 1 | (class & 0x8000) }).to_be_bytes());
            v.extend_from_slice(&ttl.to_be_bytes());
            let len = if lie % 4 == 0 { lie } else { rdata.len() as u16 };
            v.extend_from_slice(&len.to_be_bytes());
            v.extend(rdata);
            v
        });
    (any::<u16>(), any::<bool>(), any::<[u16; 4]>(), proptest::collection::vec(record, 0..20)).prop_map(
        |(id, response, counts, records)| {
            let mut p = id.to_be_bytes().to_vec();
            p.extend_from_slice(&(if response { 0x8400u16 } else { 0 }).to_be_bytes());
            for c in counts {
                p.extend_from_slice(&(c % 40).to_be_bytes());
            }
            p.extend(records.concat());
            p
        },
    )
}

/// Records about PlanCraft servers, as a hostile network would send them: names from a small
/// pool (the service, a few servers, a few hosts, the real ones), any kind, lifetime, cache-flush.
fn plancraft_records() -> impl Strategy<Value = Vec<Record>> {
    let svc = service();
    let instance = (0u8..12).prop_map(|i| Name::child(format!("Server {i}").as_bytes(), &service()));
    let host = (0u8..6).prop_map(|i| Name::dotted(&format!("h{i}-plancraft.local.")));
    let ttl = prop_oneof![Just(0u32), Just(120), Just(4500), Just(u32::MAX), any::<u32>()];
    let record = (0u8..4, instance, host, ttl, any::<bool>(), any::<u16>(), any::<[u8; 4]>()).prop_map(
        move |(kind, inst, host, ttl, flush, port, ip)| match kind {
            0 => Record { name: svc.clone(), rtype: 12, ttl, flush, data: Data::Ptr(inst) },
            1 => Record { name: inst, rtype: 33, ttl, flush, data: Data::Srv { port, target: host } },
            2 => Record { name: host, rtype: 1, ttl, flush, data: Data::A(Ipv4Addr::from(ip)) },
            _ => Record { name: inst, rtype: 16, ttl, flush, data: Data::Other },
        },
    );
    proptest::collection::vec(record, 0..40)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    /// Random bytes never make reading a packet panic.
    #[test]
    fn any_bytes_read_safely(bytes in proptest::collection::vec(any::<u8>(), 0..1500)) {
        let _ = mdns::parse(&bytes);
        let _ = mdns::parse_part(&bytes, mdns::Part::Questions);
        let _ = mdns::parse_part(&bytes, mdns::Part::Records);
    }

    /// Packets that look like mDNS read safely, within the limits on what one packet holds.
    #[test]
    fn any_packet_reads_within_limits(packets in proptest::collection::vec(packetish(), 1..30)) {
        let mut heard = Heard::new(service());
        let now = Instant::now();
        for (n, p) in packets.iter().enumerate() {
            if let Some(m) = mdns::parse(p) {
                prop_assert!(m.questions.len() <= 64 && m.records.len() <= 256);
                heard.take(&m, Ipv4Addr::new(10, 0, 0, n as u8), now);
            }
        }
        heard.expire(now + mdns::MAX_KEPT + Duration::from_secs(1));
        prop_assert_eq!(heard.size(), 0);
    }

    /// Whatever several computers say about PlanCraft servers, in any order, a listener keeps
    /// little, each computer's at most its share, never someone else's server changed, and
    /// forgets it all in time.
    #[test]
    fn what_a_listener_keeps_stays_bounded(packets in proptest::collection::vec((0u8..6, plancraft_records()), 1..40)) {
        let mut heard = Heard::new(service());
        let now = Instant::now();
        for (n, (sender, records)) in packets.iter().enumerate() {
            let from = Ipv4Addr::new(192, 168, 1, 10 + sender);
            let m = Message { response: true, records: records.clone(), ..Message::default() };
            heard.take(&m, from, now + Duration::from_millis(n as u64 * 10));
        }
        prop_assert!(heard.size() <= 6 * mdns::MAX_PER_SENDER * (2 + mdns::MAX_ADDRESSES));
        for r in heard.resolved() {
            prop_assert!(!r.addresses.is_empty() && r.addresses.len() <= mdns::MAX_ADDRESSES);
        }
        heard.expire(now + mdns::MAX_KEPT + Duration::from_secs(5));
        prop_assert_eq!(heard.size(), 0);
    }

    /// A server's name reads back as it was announced (dots and backslashes in it too).
    #[test]
    fn a_name_reads_back(label in "[A-Za-z0-9 ._\\\\-]{1,40}") {
        let fullname = format!("{}.{}", mdns::escape_label(label.as_bytes()), plancraft_server::discovery::SERVICE);
        let lan = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))];
        let found = plancraft_server::discovery::found_from(&fullname, 47340, &lan).unwrap();
        let expected = label.trim();
        prop_assert_eq!(found.name, if expected.is_empty() { "192.168.1.20".to_string() } else { expected.to_string() });
    }
}
