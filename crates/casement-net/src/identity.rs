//! The long-term key of this device.
//!
//! Every Casement device owns one Ed25519 key, created on first start and kept in
//! `device.key` (the 32-byte seed, readable only by the owner). Peers recognise the
//! device by the SHA-256 of its SubjectPublicKeyInfo, the same bytes a TLS
//! certificate built from this key carries, so the fingerprint shown to people is
//! exactly what the transport will pin.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand_core::OsRng;
use sha2::{Digest, Sha256};

const KEY_FILE: &str = "device.key";
const SEED_LEN: usize = 32;

/// DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410); the 32 key bytes follow.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("cannot use the key directory {path}: {source}")]
    Directory { path: PathBuf, source: io::Error },
    #[error("cannot read the device key {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot save the device key {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("the device key {path} is damaged: expected {SEED_LEN} bytes, found {found}")]
    Damaged { path: PathBuf, found: usize },
}

/// SHA-256 of a device's SubjectPublicKeyInfo.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(key: &VerifyingKey) -> Self {
        let mut spki = [0u8; 44];
        spki[..12].copy_from_slice(&ED25519_SPKI_PREFIX);
        spki[12..].copy_from_slice(key.as_bytes());
        Self(Sha256::digest(spki).into())
    }

    /// Full lower-case hex, for storage and mDNS.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// What people compare by eye: the first 128 bits as eight groups, `3F2A-91C4-…`.
    pub fn display(&self) -> String {
        self.0[..16]
            .chunks(2)
            .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
            .collect::<Vec<_>>()
            .join("-")
    }
}

pub struct Identity {
    key: SigningKey,
    path: PathBuf,
}

impl Identity {
    /// Loads the key from `dir`, creating the directory and a new key if needed.
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        crate::files::create_private_dir(dir).map_err(|source| IdentityError::Directory {
            path: dir.to_owned(),
            source,
        })?;
        let path = dir.join(KEY_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let seed: [u8; SEED_LEN] =
                    bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| IdentityError::Damaged {
                            path: path.clone(),
                            found: bytes.len(),
                        })?;
                Ok(Self {
                    key: SigningKey::from_bytes(&seed),
                    path,
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let key = SigningKey::generate(&mut OsRng);
                crate::files::write_private(&path, key.as_bytes()).map_err(|source| {
                    IdentityError::Write {
                        path: path.clone(),
                        source,
                    }
                })?;
                Ok(Self { key, path })
            }
            Err(source) => Err(IdentityError::Read { path, source }),
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.key.verifying_key())
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reloads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(dir.path()).unwrap();
        let second = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[cfg(unix)]
    #[test]
    fn key_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("casement");
        let identity = Identity::load_or_create(&nested).unwrap();
        let mode = fs::metadata(identity.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = fs::metadata(&nested).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }

    #[test]
    fn damaged_key_is_reported_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(KEY_FILE), b"short").unwrap();
        let err = Identity::load_or_create(dir.path()).err().unwrap();
        assert!(matches!(err, IdentityError::Damaged { found: 5, .. }));
        assert_eq!(fs::read(dir.path().join(KEY_FILE)).unwrap(), b"short");
    }

    #[test]
    fn fingerprint_matches_the_spki_hash() {
        // RFC 8032 test 1 public key. Expected value: OpenSSL re-encoded this key as
        // SPKI DER (`openssl pkey -pubin -inform DER -outform DER`) and hashed it.
        let public: [u8; 32] = [
            0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64,
            0x07, 0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68,
            0xf7, 0x07, 0x51, 0x1a,
        ];
        let key = VerifyingKey::from_bytes(&public).unwrap();
        assert_eq!(
            Fingerprint::of(&key).to_hex(),
            "06e3fd8fda29bb60ab59557de61edb0aecdb231134be30e75b455f8e1b792fa9"
        );
    }

    #[test]
    fn fingerprint_text_forms() {
        let fp = Fingerprint([0xab; 32]);
        assert_eq!(fp.display(), "ABAB-ABAB-ABAB-ABAB-ABAB-ABAB-ABAB-ABAB");
        assert_eq!(fp.to_hex().len(), 64);
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()), Some(fp));
        assert_eq!(Fingerprint::from_hex("zz"), None);
        assert_eq!(Fingerprint::from_hex(&"g".repeat(64)), None);
    }
}
