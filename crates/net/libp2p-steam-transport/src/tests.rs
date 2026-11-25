//! Unit tests for libp2p-steam-transport internals (no Steam client required).

use std::collections::VecDeque;

use crate::stream::copy_from_deque;

#[test]
fn copy_from_deque_full() {
    let mut deque: VecDeque<u8> = VecDeque::from(vec![1, 2, 3, 4, 5]);
    let mut buf = [0u8; 5];
    let n = copy_from_deque(&mut deque, &mut buf);
    assert_eq!(n, 5);
    assert_eq!(buf, [1, 2, 3, 4, 5]);
    assert!(deque.is_empty());
}

#[test]
fn copy_from_deque_partial_buf_smaller() {
    let mut deque: VecDeque<u8> = VecDeque::from(vec![10, 20, 30, 40]);
    let mut buf = [0u8; 2];
    let n = copy_from_deque(&mut deque, &mut buf);
    assert_eq!(n, 2);
    assert_eq!(buf, [10, 20]);
    assert_eq!(deque.len(), 2);
    assert_eq!(deque[0], 30);
}

#[test]
fn copy_from_deque_partial_deque_smaller() {
    let mut deque: VecDeque<u8> = VecDeque::from(vec![42]);
    let mut buf = [0u8; 8];
    let n = copy_from_deque(&mut deque, &mut buf);
    assert_eq!(n, 1);
    assert_eq!(buf[0], 42);
    assert!(deque.is_empty());
}

#[test]
fn copy_from_deque_empty() {
    let mut deque: VecDeque<u8> = VecDeque::new();
    let mut buf = [0u8; 4];
    let n = copy_from_deque(&mut deque, &mut buf);
    assert_eq!(n, 0);
}

mod muxer_constants {
    use crate::muxer::{SIG_FIN, SIG_OPEN, SIG_RST};

    #[test]
    fn signal_bytes_are_distinct() {
        assert_ne!(SIG_OPEN, SIG_FIN);
        assert_ne!(SIG_OPEN, SIG_RST);
        assert_ne!(SIG_FIN, SIG_RST);
    }

    #[test]
    fn signal_bytes_are_single_bit() {
        assert_eq!(SIG_OPEN.count_ones(), 1, "OPEN should be a single bit");
        assert_eq!(SIG_FIN.count_ones(), 1, "FIN should be a single bit");
        assert_eq!(SIG_RST.count_ones(), 1, "RST should be a single bit");
    }

    #[test]
    fn signal_bytes_no_overlap() {
        // Signals should not share any bits (allows future bitfield use)
        assert_eq!(SIG_OPEN & SIG_FIN, 0);
        assert_eq!(SIG_OPEN & SIG_RST, 0);
        assert_eq!(SIG_FIN & SIG_RST, 0);
    }
}

mod peer_id_handshake_frame {
    use libp2p_identity::{Keypair, PeerId};

    use crate::muxer::{decode_peer_id_handshake_frame, encode_peer_id_handshake_frame};

    fn test_peer_id() -> PeerId {
        let key = Keypair::ed25519_from_bytes(&mut [42u8; 32]).unwrap();
        PeerId::from(key.public())
    }

    #[test]
    fn frame_roundtrips_peer_id_bytes() {
        let peer_id = test_peer_id();
        let frame = encode_peer_id_handshake_frame(&peer_id).unwrap();
        let decoded = decode_peer_id_handshake_frame(&frame).unwrap();
        assert_eq!(PeerId::from_bytes(&decoded).unwrap(), peer_id);
    }

    #[test]
    fn frame_rejects_bad_magic() {
        let peer_id = test_peer_id();
        let mut frame = encode_peer_id_handshake_frame(&peer_id).unwrap();
        frame[0] = b'X';
        let err = decode_peer_id_handshake_frame(&frame).unwrap_err();
        assert!(err.to_string().contains("bad magic"));
    }

    #[test]
    fn frame_rejects_unsupported_version() {
        let peer_id = test_peer_id();
        let mut frame = encode_peer_id_handshake_frame(&peer_id).unwrap();
        frame[4] = 1;
        let err = decode_peer_id_handshake_frame(&frame).unwrap_err();
        assert!(err.to_string().contains("unsupported version"));
    }

    #[test]
    fn frame_rejects_length_mismatch() {
        let peer_id = test_peer_id();
        let mut frame = encode_peer_id_handshake_frame(&peer_id).unwrap();
        frame.pop();
        let err = decode_peer_id_handshake_frame(&frame).unwrap_err();
        assert!(err.to_string().contains("length mismatch"));
    }
}

mod lane_config {
    use crate::lane::{DEFAULT_INITIAL_LANES, LaneConfig};

    #[test]
    fn default_initial_lanes_is_eight() {
        assert_eq!(DEFAULT_INITIAL_LANES, 8);
    }

    #[test]
    fn default_config_has_correct_lanes() {
        let config = LaneConfig::default();
        assert_eq!(config.num_lanes, DEFAULT_INITIAL_LANES);
    }

    #[test]
    fn default_config_uniform_priority() {
        let config = LaneConfig::default();
        assert_eq!(config.priorities.len(), config.num_lanes as usize);
        assert!(
            config.priorities.iter().all(|&p| p == 0),
            "all lanes should have priority 0"
        );
    }

    #[test]
    fn default_config_equal_weights() {
        let config = LaneConfig::default();
        assert_eq!(config.weights.len(), config.num_lanes as usize);
        assert!(
            config.weights.iter().all(|&w| w == 1),
            "all lanes should have weight 1"
        );
    }

    #[test]
    fn custom_config_preserves_values() {
        let config = LaneConfig {
            num_lanes: 4,
            priorities: vec![0, 1, 2, 3],
            weights: vec![1, 2, 4, 8],
        };
        assert_eq!(config.num_lanes, 4);
        assert_eq!(config.priorities, vec![0, 1, 2, 3]);
        assert_eq!(config.weights, vec![1, 2, 4, 8]);
    }
}

mod protocol {
    use crate::protocol::{parse_steam_id, steam_multiaddr};

    #[test]
    fn roundtrip_steam_id() {
        let id = 76561198012345678u64;
        let addr = steam_multiaddr(id);
        assert_eq!(parse_steam_id(&addr), Some(id));
    }

    #[test]
    fn roundtrip_zero() {
        let addr = steam_multiaddr(0);
        assert_eq!(parse_steam_id(&addr), Some(0));
    }

    #[test]
    fn parse_returns_none_for_tcp() {
        let addr: libp2p_core::Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        assert_eq!(parse_steam_id(&addr), None);
    }
}

mod copy_from_deque_edge_cases {
    use std::collections::VecDeque;

    use crate::stream::copy_from_deque;

    #[test]
    fn zero_length_buf() {
        let mut deque: VecDeque<u8> = VecDeque::from(vec![1, 2, 3]);
        let mut buf = [0u8; 0];
        let n = copy_from_deque(&mut deque, &mut buf);
        assert_eq!(n, 0);
        assert_eq!(deque.len(), 3, "deque should be untouched");
    }

    #[test]
    fn large_deque_small_buf() {
        let mut deque: VecDeque<u8> = (0..=255).cycle().take(1000).collect();
        let mut buf = [0u8; 16];
        let n = copy_from_deque(&mut deque, &mut buf);
        assert_eq!(n, 16);
        assert_eq!(deque.len(), 984);
        // First 16 bytes should be 0..16
        for (i, byte) in buf.iter().enumerate() {
            assert_eq!(*byte, i as u8);
        }
    }
}

mod parity {
    use std::collections::{BTreeSet, VecDeque};

    /// Compute the free lane set for a given parity (mirrors muxer.rs:294-296).
    fn lanes_for_parity(initial_lanes: i32, parity: i32) -> VecDeque<i32> {
        (0..initial_lanes).filter(|i| i % 2 == parity).collect()
    }

    #[test]
    fn initiator_gets_even_lanes() {
        let lanes = lanes_for_parity(8, 0);
        assert_eq!(lanes, VecDeque::from(vec![0, 2, 4, 6]));
    }

    #[test]
    fn responder_gets_odd_lanes() {
        let lanes = lanes_for_parity(8, 1);
        assert_eq!(lanes, VecDeque::from(vec![1, 3, 5, 7]));
    }

    #[test]
    fn odd_initial_lanes_handled() {
        let initiator = lanes_for_parity(7, 0);
        let responder = lanes_for_parity(7, 1);
        assert_eq!(initiator, VecDeque::from(vec![0, 2, 4, 6]));
        assert_eq!(responder, VecDeque::from(vec![1, 3, 5]));
    }

    #[test]
    fn no_overlap_between_sides() {
        for n in [2, 4, 7, 8, 15, 16, 32] {
            let init: BTreeSet<i32> = lanes_for_parity(n, 0).into_iter().collect();
            let resp: BTreeSet<i32> = lanes_for_parity(n, 1).into_iter().collect();
            assert!(
                init.is_disjoint(&resp),
                "parity sets should be disjoint for initial_lanes={n}"
            );
            // Together they should cover 0..n
            let combined: BTreeSet<i32> = init.union(&resp).copied().collect();
            let expected: BTreeSet<i32> = (0..n).collect();
            assert_eq!(
                combined, expected,
                "combined should cover all lanes for n={n}"
            );
        }
    }

    #[test]
    fn small_initial_lanes() {
        assert_eq!(lanes_for_parity(2, 0), VecDeque::from(vec![0]));
        assert_eq!(lanes_for_parity(2, 1), VecDeque::from(vec![1]));
        assert_eq!(lanes_for_parity(1, 0), VecDeque::from(vec![0]));
        assert!(lanes_for_parity(1, 1).is_empty());
    }
}

mod transport_validation {
    use libp2p_core::Multiaddr;

    use crate::transport::{validate_dial_addr, validate_listen_addr};

    #[test]
    fn listen_addr_accepts_memory_protocol() {
        let addr: Multiaddr = "/memory/76561198012345678".parse().unwrap();
        assert!(validate_listen_addr(&addr).is_ok());
    }

    #[test]
    fn listen_addr_accepts_zero() {
        let addr: Multiaddr = "/memory/0".parse().unwrap();
        assert!(validate_listen_addr(&addr).is_ok());
    }

    #[test]
    fn listen_addr_rejects_tcp() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        assert!(validate_listen_addr(&addr).is_err());
    }

    #[test]
    fn dial_addr_accepts_valid_steam_id() {
        let addr: Multiaddr = "/memory/76561198012345678".parse().unwrap();
        let id = validate_dial_addr(&addr).unwrap();
        assert_eq!(id, 76561198012345678);
    }

    #[test]
    fn dial_addr_rejects_wildcard_zero() {
        let addr: Multiaddr = "/memory/0".parse().unwrap();
        assert!(validate_dial_addr(&addr).is_err());
    }

    #[test]
    fn dial_addr_rejects_tcp() {
        let addr: Multiaddr = "/ip4/0.0.0.0/tcp/0".parse().unwrap();
        assert!(validate_dial_addr(&addr).is_err());
    }
}
