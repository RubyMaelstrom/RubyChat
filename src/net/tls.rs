//! TLS client configuration: rustls on aws-lc-rs, verifying servers against
//! Mozilla's roots with RFC 9525 hostname checks, and preferring the
//! X25519MLKEM768 hybrid key exchange (DESIGN.md §6.2). Prompts to trust
//! unverified certificates and pinning arrive with M3.

use std::sync::Arc;

use rustls::{ClientConfig, ClientConnection, RootCertStore};

pub fn client_config() -> Result<Arc<ClientConfig>, rustls::Error> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// What a completed handshake negotiated, for the server buffer:
/// "TLS 1.3, TLS13_AES_256_GCM_SHA384, X25519MLKEM768".
pub fn describe(connection: &ClientConnection) -> String {
    let version = match connection.protocol_version() {
        Some(rustls::ProtocolVersion::TLSv1_3) => String::from("TLS 1.3"),
        Some(rustls::ProtocolVersion::TLSv1_2) => String::from("TLS 1.2"),
        Some(other) => format!("{other:?}"),
        None => String::from("TLS"),
    };
    let mut parts = vec![version];
    if let Some(suite) = connection.negotiated_cipher_suite() {
        parts.push(format!("{:?}", suite.suite()));
    }
    if let Some(group) = connection.negotiated_key_exchange_group() {
        parts.push(format!("{:?}", group.name()));
    }
    parts.join(", ")
}
