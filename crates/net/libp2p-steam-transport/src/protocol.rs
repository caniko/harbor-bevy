use libp2p_core::Multiaddr;
use libp2p_core::multiaddr::Protocol;

/// Build a multiaddr encoding a Steam ID: `/memory/<steam_id>`.
///
/// We reuse the existing `Protocol::Memory(u64)` because Steam IDs are `u64`
/// and the memory protocol is already in the multiaddr spec.
pub fn steam_multiaddr(steam_id: u64) -> Multiaddr {
    Multiaddr::empty().with(Protocol::Memory(steam_id))
}

/// Extract a Steam ID from a `/memory/<id>` multiaddr.
///
/// Returns `None` if the address doesn't match the expected format.
pub fn parse_steam_id(addr: &Multiaddr) -> Option<u64> {
    let mut iter = addr.iter();
    match iter.next() {
        Some(Protocol::Memory(id)) if iter.next().is_none() => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let steam_id: u64 = 76561198012345678;
        let addr = steam_multiaddr(steam_id);
        assert_eq!(parse_steam_id(&addr), Some(steam_id));
    }

    #[test]
    fn parse_zero() {
        let addr = steam_multiaddr(0);
        assert_eq!(parse_steam_id(&addr), Some(0));
    }

    #[test]
    fn rejects_wrong_protocol() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        assert_eq!(parse_steam_id(&addr), None);
    }

    #[test]
    fn rejects_extra_components() {
        let addr: Multiaddr = "/memory/123/tcp/80".parse().unwrap();
        assert_eq!(parse_steam_id(&addr), None);
    }
}
