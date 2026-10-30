//! TLS client configuration (DESIGN.md §6.2): rustls on aws-lc-rs,
//! preferring the X25519MLKEM768 hybrid key exchange. Each server's policy
//! verifies against Mozilla's roots with RFC 9525 name checks, or accepts
//! only its pinned certificate, or accepts any; a pinned or trusted-once
//! fingerprint always passes. Handshake signatures are checked whatever
//! the policy (RFC 8446 §4.4.3). A client certificate serves CertFP and
//! SASL EXTERNAL.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore,
    SignatureScheme,
};

use crate::config::known_hosts::Fingerprint;
use crate::config::networks::CertificatePolicy;

/// What every connection shares, built once: the provider and Mozilla's
/// roots in a WebPKI verifier.
pub struct Shared {
    provider: Arc<CryptoProvider>,
    webpki: Arc<WebPkiServerVerifier>,
}

pub fn shared() -> Result<Shared, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|error| error.to_string())?;
    Ok(Shared { provider, webpki })
}

/// The SHA-256 of a certificate's DER encoding, as `known_hosts` keeps it.
pub fn fingerprint(der: &[u8]) -> Fingerprint {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, der);
    let mut fingerprint = [0u8; 32];
    fingerprint.copy_from_slice(digest.as_ref());
    fingerprint
}

/// A certificate a policy refused, for the trust prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub der: Vec<u8>,
    pub fingerprint: Fingerprint,
    pub reason: String,
    /// The server has a pin, and this isn't it: its certificate changed.
    pub changed: bool,
}

/// One connection's verifier. Only the network thread calls it.
#[derive(Debug)]
pub struct Verifier {
    webpki: Arc<WebPkiServerVerifier>,
    policy: CertificatePolicy,
    pins: Vec<Fingerprint>,
    refused: Mutex<Option<Refusal>>,
}

impl Verifier {
    /// The certificate refused during the handshake, if one was.
    pub fn refusal(&self) -> Option<Refusal> {
        self.refused.lock().ok()?.take()
    }

    fn refuse(&self, der: &[u8], fingerprint: Fingerprint, reason: String) {
        if let Ok(mut refused) = self.refused.lock() {
            *refused = Some(Refusal {
                der: der.to_vec(),
                fingerprint,
                reason,
                changed: !self.pins.is_empty(),
            });
        }
    }
}

/// Why WebPKI refused a certificate, as a person would say it.
fn explain(error: &rustls::Error) -> String {
    match error {
        rustls::Error::InvalidCertificate(reason) => match reason {
            CertificateError::UnknownIssuer => String::from(
                "it was issued by an authority RubyChat doesn't know, or by the server itself",
            ),
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                String::from("it has expired")
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                String::from("it isn't valid yet")
            }
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                String::from("it was issued for a different name")
            }
            CertificateError::Revoked => String::from("it has been revoked"),
            CertificateError::BadEncoding | CertificateError::BadSignature => {
                String::from("it is malformed")
            }
            other => format!("{other:?}"),
        },
        other => other.to_string(),
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint = fingerprint(end_entity.as_ref());
        if self.pins.contains(&fingerprint) {
            return Ok(ServerCertVerified::assertion());
        }
        match self.policy {
            CertificatePolicy::None => Ok(ServerCertVerified::assertion()),
            CertificatePolicy::Pinned => {
                let reason = if self.pins.is_empty() {
                    String::from("no certificate is pinned for this server")
                } else {
                    String::from("it isn't the pinned certificate")
                };
                self.refuse(end_entity.as_ref(), fingerprint, reason);
                Err(rustls::Error::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ))
            }
            CertificatePolicy::Verify => {
                let verified = self.webpki.verify_server_cert(
                    end_entity,
                    intermediates,
                    server_name,
                    ocsp_response,
                    now,
                );
                if let Err(error) = &verified {
                    // Most certificates refused on IRC signed themselves;
                    // WebPKI's reasons for those vary.
                    let reason = if super::x509::parse(end_entity.as_ref())
                        .is_some_and(|facts| facts.self_signed())
                    {
                        String::from("it signed itself, so no certificate authority vouches for it")
                    } else {
                        explain(error)
                    };
                    self.refuse(end_entity.as_ref(), fingerprint, reason);
                }
                verified
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

/// One connection's configuration and its verifier. A client certificate
/// file holds the chain and its key, in PEM.
pub fn client_config(
    shared: &Shared,
    policy: CertificatePolicy,
    pins: &[Fingerprint],
    client_certificate: Option<&Path>,
) -> Result<(Arc<ClientConfig>, Arc<Verifier>), String> {
    let verifier = Arc::new(Verifier {
        webpki: shared.webpki.clone(),
        policy,
        pins: pins.to_vec(),
        refused: Mutex::new(None),
    });
    let builder = ClientConfig::builder_with_provider(shared.provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone());
    let config = match client_certificate {
        Some(path) => {
            let read = |error: rustls::pki_types::pem::Error| {
                format!("client certificate {}: {error}", path.display())
            };
            let chain = CertificateDer::pem_file_iter(path)
                .map_err(read)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(read)?;
            if chain.is_empty() {
                return Err(format!(
                    "client certificate {}: no certificate in it",
                    path.display()
                ));
            }
            let key = PrivateKeyDer::from_pem_file(path).map_err(read)?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|error| format!("client certificate {}: {error}", path.display()))?
        }
        None => builder.with_no_client_auth(),
    };
    Ok((Arc::new(config), verifier))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_match_openssl() {
        // `openssl x509 -in testdata/server.pem -noout -fingerprint -sha256`.
        let der = CertificateDer::from_pem_slice(include_bytes!("testdata/server.pem")).unwrap();
        let expected = "73:DE:04:6D:26:5B:29:F5:11:E9:C6:E1:21:00:65:37:19:F4:28:D9:84:99:E1:0A:F1:B8:E9:D8:FC:F8:6F:ED";
        assert_eq!(
            crate::config::known_hosts::display(&fingerprint(der.as_ref())),
            expected
        );
    }

    #[test]
    fn client_certificates_load_and_missing_ones_are_explained() {
        let shared = shared().unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/net/testdata/client.pem");
        assert!(client_config(&shared, CertificatePolicy::Verify, &[], Some(&path)).is_ok());
        let missing = client_config(
            &shared,
            CertificatePolicy::Verify,
            &[],
            Some(Path::new("/nonexistent/cert.pem")),
        );
        assert!(missing.is_err_and(|error| error.contains("/nonexistent/cert.pem")));
    }
}
