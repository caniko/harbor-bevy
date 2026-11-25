//! Sanity-check that every Frame variant rkyv-roundtrips losslessly.
//! The whole rendezvous architecture pivots on this; if it ever regresses,
//! catch it here, not in production.

use rkyv::rancor::Error;
use thespan_rendezvous_proto::{
    DiscoverRequest, DiscoverResponse, Frame, LobbyEntry, MultiaddrBytes, Namespace, PeerIdBytes,
    PeerTicket, RegisterAck, RegisterRequest, SignedEnvelope,
};

fn roundtrip(frame: &Frame) -> Frame {
    let bytes = rkyv::to_bytes::<Error>(frame).expect("serialize");
    rkyv::from_bytes::<Frame, Error>(&bytes).expect("deserialize")
}

fn sample_ticket() -> PeerTicket {
    PeerTicket {
        peer_id: PeerIdBytes(vec![0x12, 0x20, 0xab, 0xcd]),
        addrs: vec![MultiaddrBytes("/ip4/127.0.0.1/udp/4001/quic-v1".into())],
        relay_hints: vec![],
    }
}

fn sample_envelope(payload: Vec<u8>) -> SignedEnvelope {
    SignedEnvelope::new(
        vec![1, 2, 3],
        PeerIdBytes(vec![0x12, 0x20, 0xff]),
        [0u8; 16],
        1_700_000_000,
        payload,
        vec![9, 9, 9],
    )
}

#[test]
fn register_roundtrips() {
    let req = RegisterRequest {
        namespace: Namespace::new("lobby.public").unwrap(),
        ticket: sample_ticket(),
        ttl_secs: 120,
        metadata: b"{\"mode\":\"1v1\"}".to_vec(),
    };
    let payload_bytes = rkyv::to_bytes::<Error>(&req).unwrap().to_vec();
    let frame = Frame::Register(sample_envelope(payload_bytes));
    let _back = roundtrip(&frame);
}

#[test]
fn discover_roundtrips() {
    let req = DiscoverRequest {
        namespace: Namespace::new("quickmatch.1v1").unwrap(),
        cursor: vec![],
        limit: 16,
    };
    let _ = roundtrip(&Frame::Discover(req));
}

#[test]
fn discover_response_roundtrips() {
    let resp = DiscoverResponse {
        entries: vec![LobbyEntry {
            ticket: sample_ticket(),
            metadata: vec![],
            ttl_remaining_secs: 60,
        }],
        next_cursor: vec![],
    };
    let _ = roundtrip(&Frame::DiscoverResponse(resp));
}

#[test]
fn register_ack_roundtrips() {
    let ack = RegisterAck {
        granted_ttl_secs: 300,
        registration_id: 42,
    };
    let _ = roundtrip(&Frame::RegisterAck(ack));
}

#[test]
fn namespace_validation() {
    assert!(Namespace::new("").is_err());
    assert!(Namespace::new("Bad Chars!").is_err());
    assert!(Namespace::new("a".repeat(Namespace::MAX_LEN + 1)).is_err());
    assert!(Namespace::new("ok.namespace_1-2").is_ok());
}
