//! Every reqwest client starts here, so the whole build has one TLS stack:
//! rustls on ring.

/// A reqwest client builder on rustls with the ring provider. Desktop and
/// servers verify against the platform trust store. Android trusts only the
/// bundled Mozilla roots, because its platform verifier needs JNI setup, so a
/// server chained to an OEM or user-installed root fails there.
pub fn builder() -> reqwest::ClientBuilder {
    // Err only means a provider is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = reqwest::Client::builder().tls_backend_rustls();
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|cert| reqwest::Certificate::from_der(cert).ok()),
    );
    builder
}

/// An error with its causes. reqwest's own text for a failed build is only
/// "builder error"; the reason, such as no readable CA store, is a source.
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    std::iter::successors(Some(err), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds_without_a_preinstalled_provider() {
        super::builder().build().expect("client builds");
    }

    #[test]
    fn error_chain_keeps_the_cause() {
        let err = super::builder()
            .tls_version_min(reqwest::tls::Version::TLS_1_3)
            .tls_version_max(reqwest::tls::Version::TLS_1_2)
            .build()
            .unwrap_err();
        assert!(super::error_chain(&err).contains("empty supported tls versions"));
    }
}
