#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! PlanCraft's own mDNS (`plancraft_server::mdns`): packets come from anyone on the network, so
//! whatever they hold, reading them never panics, and what a listener keeps stays bounded.

use std::time::{Duration, Instant};

use plancraft_server::mdns::{self, Heard, Name};
use proptest::prelude::*;

fn service() -> Name {
    Name::dotted("_plancraft._tcp.local.")
}

/// A packet that looks like mDNS: a real header, then names made of labels and pointers
/// (forwards, backwards, to themselves), types PlanCraft reads, and lengths that may lie.
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
    let record = (proptest::collection::vec(piece, 0..6), types, any::<u32>(), proptest::collection::vec(any::<u8>(), 0..40), any::<u16>())
        .prop_map(|(name, t, ttl, rdata, lie)| {
            let mut v: Vec<u8> = name.concat();
            v.extend_from_slice(&t.to_be_bytes());
            v.extend_from_slice(&1u16.to_be_bytes());
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    /// Random bytes never make reading a packet panic.
    #[test]
    fn any_bytes_read_safely(bytes in proptest::collection::vec(any::<u8>(), 0..1500)) {
        let _ = mdns::parse(&bytes);
    }

    /// Packets that look like mDNS read safely, and whatever they say, a listener keeps little
    /// and forgets it in time.
    #[test]
    fn any_packet_is_kept_bounded(packets in proptest::collection::vec(packetish(), 1..30)) {
        let mut heard = Heard::new(service());
        let now = Instant::now();
        for p in &packets {
            if let Some(m) = mdns::parse(p) {
                prop_assert!(m.questions.len() <= 64 && m.records.len() <= 256);
                heard.take(&m, now);
            }
        }
        prop_assert!(heard.size() <= mdns::MAX_INSTANCES + mdns::MAX_HOSTS * mdns::MAX_ADDRESSES);
        heard.expire(now + mdns::MAX_TTL + Duration::from_secs(1));
        prop_assert_eq!(heard.size(), 0);
    }
}
