//! TLS 1.3 support for the OAAT control channel (RFC section 8).
//!
//! - Self-signed certificates generated at startup via `rcgen`
//! - TOFU (Trust On First Use) client config: accepts any server certificate
//! - SHA-256 fingerprint for certificate identification

use std::sync::Arc;

use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

/// Ensure the ring crypto provider is installed (idempotent).
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Generate a self-signed certificate for use by an OAAT endpoint.
///
/// Returns `(ServerConfig, certificate DER bytes, SHA-256 hex fingerprint)`.
pub fn generate_self_signed_cert() -> Result<
    (rustls::ServerConfig, Vec<u8>, String),
    Box<dyn std::error::Error + Send + Sync>,
> {
    ensure_crypto_provider();
    let mut params = CertificateParams::new(vec!["oaat-endpoint".into()])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String("OAAT Endpoint".into()),
    );

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    let cert_der = cert.der().to_vec();
    let fingerprint = cert_fingerprint(&cert_der);

    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
    let cert_chain = vec![CertificateDer::from(cert_der.clone())];

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key_der)?;

    Ok((server_config, cert_der, fingerprint))
}

/// Build a `rustls::ClientConfig` that accepts any server certificate (TOFU pattern).
///
/// On first connection the client logs the server's certificate fingerprint.
/// A production implementation would persist this fingerprint and verify on reconnection.
pub fn make_client_config_tofu() -> rustls::ClientConfig {
    ensure_crypto_provider();
    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TofuVerifier))
        .with_no_client_auth()
}

/// Compute the SHA-256 hex fingerprint of a DER-encoded certificate.
pub fn cert_fingerprint(cert_der: &[u8]) -> String {
    let hash = Sha256::digest(cert_der);
    hash.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A certificate verifier that accepts any certificate (TOFU).
#[derive(Debug)]
struct TofuVerifier;

impl rustls::client::danger::ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // TOFU: accept any certificate. Log the fingerprint for the caller.
        let fp = cert_fingerprint(end_entity.as_ref());
        // We cannot call tracing here because this is in oaat-core (no tracing dep),
        // but callers can read the fingerprint from the TLS connection after handshake.
        let _ = fp;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_cert_and_fingerprint() {
        let (config, cert_der, fingerprint) = generate_self_signed_cert().unwrap();
        assert!(!cert_der.is_empty());
        // SHA-256 fingerprint = 32 bytes = 32 hex pairs + 31 colons = 95 chars
        assert_eq!(fingerprint.len(), 95);
        assert!(fingerprint.contains(':'));
        // ServerConfig should be usable
        let _ = config;
    }

    #[test]
    fn fingerprint_deterministic() {
        let data = b"test certificate data";
        let fp1 = cert_fingerprint(data);
        let fp2 = cert_fingerprint(data);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn tofu_client_config_builds() {
        let config = make_client_config_tofu();
        let _ = config;
    }
}
