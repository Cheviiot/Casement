//! TLS for QUIC: certificates made from the device key and verifiers that trust keys,
//! not certificate authorities.
//!
//! Every device presents a self-signed certificate for its Ed25519 key. The client pins
//! the host by fingerprint (or records it for the user to confirm on first use); the
//! host accepts any well-formed device certificate during the handshake and decides
//! trust afterwards, at login. Only TLS 1.3 and Ed25519 are accepted.

use std::fmt;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::identity::{Fingerprint, Identity};

pub const ALPN: &[u8] = b"casement/1";
/// The name in every certificate; identity comes from the key, never from this name.
pub const SERVER_NAME: &str = "casement";

/// DER prefix of a PKCS#8 v1 Ed25519 private key (RFC 8410); the 32-byte seed follows.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A self-signed certificate for the device key, with its private key.
pub fn device_certificate(
    identity: &Identity,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), rcgen::Error> {
    let mut pkcs8 = ED25519_PKCS8_PREFIX.to_vec();
    pkcs8.extend_from_slice(identity.signing_key().as_bytes());
    let pkcs8 = PrivatePkcs8KeyDer::from(pkcs8);
    let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &rcgen::PKCS_ED25519)?;
    let mut params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()])?;
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2120, 1, 1);
    let cert = params.self_signed(&key_pair)?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(pkcs8)))
}

/// Fingerprint of the Ed25519 key in a certificate; None for anything else.
pub fn certificate_fingerprint(cert: &CertificateDer<'_>) -> Option<Fingerprint> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).ok()?;
    let spki = parsed.tbs_certificate.subject_pki.raw;
    if spki.len() != 44 || spki[..12] != ED25519_SPKI_PREFIX {
        return None;
    }
    Some(Fingerprint(Sha256::digest(spki).into()))
}

fn bad_certificate() -> rustls::Error {
    rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
}

fn tls13_signature(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::NoSignatureSchemesInCommon,
        ));
    }
    verify_tls13_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

/// What the client saw of the host's key during the handshake.
#[derive(Clone, Default)]
pub struct SeenKey(Arc<Mutex<Option<Fingerprint>>>);

impl SeenKey {
    pub fn get(&self) -> Option<Fingerprint> {
        *self.0.lock().unwrap()
    }
}

/// Client side: accept the host whose key matches `expected`, or any Ed25519 key when
/// nothing is expected yet (trust on first use, confirmed by the user before login).
pub struct HostKeyVerifier {
    expected: Option<Fingerprint>,
    seen: SeenKey,
    provider: Arc<CryptoProvider>,
}

impl HostKeyVerifier {
    pub fn new(expected: Option<Fingerprint>, seen: SeenKey) -> Self {
        Self {
            expected,
            seen,
            provider: provider(),
        }
    }
}

impl fmt::Debug for HostKeyVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostKeyVerifier")
            .field("expected", &self.expected)
            .finish()
    }
}

/// Text of the error the verifier returns on a key mismatch; `client` maps it back.
pub const KEY_MISMATCH: &str = "casement: host key does not match the trusted fingerprint";

impl ServerCertVerifier for HostKeyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint = certificate_fingerprint(end_entity).ok_or_else(bad_certificate)?;
        *self.seen.0.lock().unwrap() = Some(fingerprint);
        match self.expected {
            Some(expected) if expected != fingerprint => {
                Err(rustls::Error::General(KEY_MISMATCH.into()))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls13RequiredForQuic,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls13_signature(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Host side: require a device certificate and check its signature; who may log in is
/// decided by the login step, which reads the fingerprint from the connection.
pub struct DeviceKeyVerifier {
    provider: Arc<CryptoProvider>,
}

impl DeviceKeyVerifier {
    pub fn new() -> Self {
        Self {
            provider: provider(),
        }
    }
}

impl Default for DeviceKeyVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for DeviceKeyVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceKeyVerifier")
    }
}

impl ClientCertVerifier for DeviceKeyVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        certificate_fingerprint(end_entity).ok_or_else(bad_certificate)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls13RequiredForQuic,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls13_signature(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

pub fn client_config(
    identity: &Identity,
    verifier: HostKeyVerifier,
) -> Result<rustls::ClientConfig, crate::NetError> {
    let (cert, key) = device_certificate(identity)?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(vec![cert], key)?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(config)
}

pub fn server_config(identity: &Identity) -> Result<rustls::ServerConfig, crate::NetError> {
    let (cert, key) = device_certificate(identity)?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(DeviceKeyVerifier::new()))
        .with_single_cert(vec![cert], key)?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_carries_the_device_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let (cert, _) = device_certificate(&identity).unwrap();
        assert_eq!(certificate_fingerprint(&cert), Some(identity.fingerprint()));
    }

    #[test]
    fn garbage_is_not_a_device_certificate() {
        assert_eq!(
            certificate_fingerprint(&CertificateDer::from(vec![0x30, 0x00])),
            None
        );
    }
}
