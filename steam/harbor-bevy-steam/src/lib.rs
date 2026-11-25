//! Wave 4 Steam isolation until `bevy-steamworks` 0.18 and `steam-pressure`
//! 0.2.0 are available on crates.io. This stub links no Steam dependencies.

#[cfg(test)]
mod tests {
    #[test]
    fn builds_as_its_own_package() {
        assert_eq!(env!("CARGO_PKG_NAME"), "harbor-bevy-steam");
    }
}
