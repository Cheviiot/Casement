//! Who may log in to a host: the access password, the list of trusted devices, and a
//! brake on guessing.
//!
//! Password login is OPAQUE (an augmented PAKE, RFC 9807) with Argon2id as its key
//! stretching function. The host keeps only an OPAQUE "password file": it reveals nothing
//! that lets anyone log in, and guessing the password from it costs an Argon2id run per
//! guess. The password never travels, a recorded session gives nothing to guess offline,
//! and the TLS exporter is the OPAQUE context, so a login cannot be relayed into another
//! connection. The host is authenticated by OPAQUE too (on top of its pinned key).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use opaque_ke::{
    CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, RegistrationRequest, RegistrationResponse, RegistrationUpload, ServerLogin,
    ServerLoginParameters, ServerRegistration, ServerSetup,
};
use rand_core::OsRng;

use crate::identity::Fingerprint;

const PASSWORD_FILE: &str = "host-password";
const TRUST_FILE: &str = "trusted-devices";
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-casement-login";
/// The one account a Casement host has: OPAQUE wants a credential identifier.
const ACCOUNT: &[u8] = b"casement";
const FILE_HEADER: &str = "opaque-v1";

pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_BYTES: usize = 256;

/// Ristretto255 everywhere, Argon2id (RFC 9106 defaults) to stretch the password.
pub struct Suite;

impl CipherSuite for Suite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2::Sha512>;
    type Ksf = argon2::Argon2<'static>;
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the password must have at least {MIN_PASSWORD_CHARS} characters")]
    PasswordTooShort,
    #[error("the password is too long")]
    PasswordTooLong,
    #[error("cannot use {path}: {source}")]
    File { path: PathBuf, source: io::Error },
    #[error("{path} is damaged")]
    Damaged { path: PathBuf },
    #[error("wrong password")]
    WrongPassword,
    #[error("malformed login message")]
    Malformed,
}

fn check_length(password: &str) -> Result<(), AuthError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(AuthError::PasswordTooLong);
    }
    Ok(())
}

/// The host's access password as stored: the OPAQUE server keys and password file.
#[derive(Clone)]
pub struct StoredPassword {
    setup: ServerSetup<Suite>,
    file: ServerRegistration<Suite>,
}

impl StoredPassword {
    /// Registers `password` (both OPAQUE roles run here, on the host). Slow on purpose.
    pub fn create(password: &str) -> Result<Self, AuthError> {
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(AuthError::PasswordTooShort);
        }
        check_length(password)?;
        let mut rng = OsRng;
        let setup = ServerSetup::<Suite>::new(&mut rng);
        let start = ClientRegistration::<Suite>::start(&mut rng, password.as_bytes())
            .map_err(|_| AuthError::Malformed)?;
        let request = RegistrationRequest::deserialize(&start.message.serialize())
            .map_err(|_| AuthError::Malformed)?;
        let response = ServerRegistration::<Suite>::start(&setup, request, ACCOUNT)
            .map_err(|_| AuthError::Malformed)?;
        let finish = start
            .state
            .finish(
                &mut rng,
                password.as_bytes(),
                RegistrationResponse::deserialize(&response.message.serialize())
                    .map_err(|_| AuthError::Malformed)?,
                ClientRegistrationFinishParameters::default(),
            )
            .map_err(|_| AuthError::Malformed)?;
        let upload = RegistrationUpload::<Suite>::deserialize(&finish.message.serialize())
            .map_err(|_| AuthError::Malformed)?;
        Ok(Self {
            setup,
            file: ServerRegistration::finish(upload),
        })
    }

    /// None when no password is set. A file from an older Casement (before OPAQUE) counts
    /// as "no password": the owner sets it again.
    pub fn load(dir: &Path) -> Result<Option<Self>, AuthError> {
        let path = dir.join(PASSWORD_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(AuthError::File { path, source }),
        };
        let damaged = || AuthError::Damaged { path: path.clone() };
        let fields: Vec<&str> = text.split_whitespace().collect();
        match fields.as_slice() {
            [header, setup, file] if *header == FILE_HEADER => {
                let setup = from_hex(setup)
                    .and_then(|b| ServerSetup::<Suite>::deserialize(&b).ok())
                    .ok_or_else(damaged)?;
                let file = from_hex(file)
                    .and_then(|b| ServerRegistration::<Suite>::deserialize(&b).ok())
                    .ok_or_else(damaged)?;
                Ok(Some(Self { setup, file }))
            }
            [header, ..] if *header == "argon2id-v1" => {
                tracing::warn!(
                    "{}: password from an older Casement; set it again",
                    path.display()
                );
                Ok(None)
            }
            _ => Err(damaged()),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), AuthError> {
        let path = dir.join(PASSWORD_FILE);
        let text = format!(
            "{FILE_HEADER} {} {}\n",
            to_hex(&self.setup.serialize()),
            to_hex(&self.file.serialize())
        );
        crate::files::write_private(&path, text.as_bytes())
            .map_err(|source| AuthError::File { path, source })
    }

    pub fn remove(dir: &Path) -> Result<(), AuthError> {
        let path = dir.join(PASSWORD_FILE);
        match fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                Err(AuthError::File { path, source: err })
            }
            _ => Ok(()),
        }
    }
}

/// Client side of an OPAQUE login.
pub struct ClientPasswordLogin {
    state: ClientLogin<Suite>,
    /// KE1, to send to the host.
    pub request: Vec<u8>,
}

impl ClientPasswordLogin {
    pub fn start(password: &str) -> Result<Self, AuthError> {
        check_length(password)?;
        let start = ClientLogin::<Suite>::start(&mut OsRng, password.as_bytes())
            .map_err(|_| AuthError::Malformed)?;
        Ok(Self {
            request: start.message.serialize().to_vec(),
            state: start.state,
        })
    }

    /// Takes the host's KE2; returns KE3 for the host. Fails with `WrongPassword` when the
    /// password (or the host) is not the registered one. Slow: Argon2id runs here.
    pub fn finish(
        self,
        password: &str,
        response: &[u8],
        exporter: &[u8],
    ) -> Result<Vec<u8>, AuthError> {
        let response =
            CredentialResponse::deserialize(response).map_err(|_| AuthError::Malformed)?;
        let params = ClientLoginFinishParameters::new(Some(exporter), Default::default(), None);
        let finish = self
            .state
            .finish(&mut OsRng, password.as_bytes(), response, params)
            .map_err(|_| AuthError::WrongPassword)?;
        Ok(finish.message.serialize().to_vec())
    }
}

/// Host side of an OPAQUE login.
pub struct HostPasswordLogin {
    state: ServerLogin<Suite>,
    /// KE2, to send to the client.
    pub response: Vec<u8>,
}

impl HostPasswordLogin {
    pub fn start(
        stored: &StoredPassword,
        request: &[u8],
        exporter: &[u8],
    ) -> Result<Self, AuthError> {
        let request = CredentialRequest::deserialize(request).map_err(|_| AuthError::Malformed)?;
        let params = ServerLoginParameters {
            context: Some(exporter),
            identifiers: Default::default(),
        };
        let start = ServerLogin::start(
            &mut OsRng,
            &stored.setup,
            Some(stored.file.clone()),
            request,
            ACCOUNT,
            params,
        )
        .map_err(|_| AuthError::Malformed)?;
        Ok(Self {
            response: start.message.serialize().to_vec(),
            state: start.state,
        })
    }

    /// True when the client proved it knows the password, over this very connection.
    pub fn finish(self, finalization: &[u8], exporter: &[u8]) -> bool {
        let Ok(finalization) = CredentialFinalization::deserialize(finalization) else {
            return false;
        };
        let params = ServerLoginParameters {
            context: Some(exporter),
            identifiers: Default::default(),
        };
        self.state.finish(finalization, params).is_ok()
    }
}

/// Devices allowed in without the password, kept in `trusted-devices` as
/// `<fingerprint hex>\t<name>` lines.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    path: Option<PathBuf>,
    devices: Vec<(Fingerprint, String)>,
}

impl TrustStore {
    pub fn load(dir: &Path) -> Result<Self, AuthError> {
        let path = dir.join(TRUST_FILE);
        let mut store = Self {
            path: Some(path.clone()),
            devices: Vec::new(),
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(store),
            Err(source) => return Err(AuthError::File { path, source }),
        };
        for line in text.lines() {
            let (hex, name) = line.split_once('\t').unwrap_or((line, ""));
            // A damaged line costs that device its trust, never the whole list.
            if let Some(fingerprint) = Fingerprint::from_hex(hex) {
                store.devices.push((fingerprint, sanitize_name(name)));
            }
        }
        Ok(store)
    }

    pub fn contains(&self, fingerprint: &Fingerprint) -> bool {
        self.devices.iter().any(|(fp, _)| fp == fingerprint)
    }

    pub fn devices(&self) -> &[(Fingerprint, String)] {
        &self.devices
    }

    pub fn add(&mut self, fingerprint: Fingerprint, name: &str) -> Result<(), AuthError> {
        self.devices.retain(|(fp, _)| *fp != fingerprint);
        self.devices.push((fingerprint, sanitize_name(name)));
        self.save()
    }

    pub fn remove(&mut self, fingerprint: &Fingerprint) -> Result<bool, AuthError> {
        let before = self.devices.len();
        self.devices.retain(|(fp, _)| fp != fingerprint);
        let removed = self.devices.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<(), AuthError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text: String = self
            .devices
            .iter()
            .map(|(fp, name)| format!("{}\t{}\n", fp.to_hex(), name))
            .collect();
        crate::files::write_private(path, text.as_bytes()).map_err(|source| AuthError::File {
            path: path.clone(),
            source,
        })
    }
}

/// Names come from the peer: keep them printable and short.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Slows down password guessing per source address: three free attempts, then a pause
/// that doubles with every failure up to five minutes.
#[derive(Default)]
pub struct FailureLimiter {
    failures: HashMap<IpAddr, (u32, Instant)>,
}

impl FailureLimiter {
    const FREE_ATTEMPTS: u32 = 3;
    const MAX_DELAY: Duration = Duration::from_secs(300);

    /// Seconds to wait before `addr` may try again, or None if it may try now.
    pub fn check(&self, addr: IpAddr, now: Instant) -> Option<u32> {
        let (count, last) = self.failures.get(&addr)?;
        if *count < Self::FREE_ATTEMPTS {
            return None;
        }
        let delay = Self::delay(*count);
        let ready = *last + delay;
        (now < ready).then(|| (ready - now).as_secs().max(1) as u32)
    }

    pub fn fail(&mut self, addr: IpAddr, now: Instant) {
        let entry = self.failures.entry(addr).or_insert((0, now));
        entry.0 += 1;
        entry.1 = now;
        // Forget sources that have been quiet for a day, so the map cannot grow forever.
        self.failures
            .retain(|_, (_, last)| now.duration_since(*last) < Duration::from_secs(86400));
    }

    pub fn succeed(&mut self, addr: IpAddr) {
        self.failures.remove(&addr);
    }

    fn delay(count: u32) -> Duration {
        let exp = count.saturating_sub(Self::FREE_ATTEMPTS).min(16);
        Duration::from_secs(1u64 << exp).min(Self::MAX_DELAY)
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(
        stored: &StoredPassword,
        password: &str,
        client_exporter: &[u8],
        host_exporter: &[u8],
    ) -> bool {
        let client = ClientPasswordLogin::start(password).unwrap();
        let host = HostPasswordLogin::start(stored, &client.request, host_exporter).unwrap();
        match client.finish(password, &host.response, client_exporter) {
            Ok(finalization) => host.finish(&finalization, host_exporter),
            Err(AuthError::WrongPassword) => false,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn opaque_accepts_only_the_password_on_this_connection() {
        let stored = StoredPassword::create("correct horse").unwrap();
        assert!(login(&stored, "correct horse", b"tls-1", b"tls-1"));
        assert!(!login(&stored, "wrong horse!", b"tls-1", b"tls-1"));
        // A login made for one TLS session does not verify in another.
        assert!(!login(&stored, "correct horse", b"tls-1", b"tls-2"));
    }

    #[test]
    fn password_round_trip_damage_and_old_format() {
        let dir = tempfile::tempdir().unwrap();
        assert!(StoredPassword::load(dir.path()).unwrap().is_none());
        assert!(matches!(
            StoredPassword::create("short"),
            Err(AuthError::PasswordTooShort)
        ));
        let stored = StoredPassword::create("пароль-подлиннее").unwrap();
        stored.save(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(PASSWORD_FILE)).unwrap();
        assert!(!text.contains("пароль"), "the file holds no password");
        let loaded = StoredPassword::load(dir.path()).unwrap().unwrap();
        assert!(login(&loaded, "пароль-подлиннее", b"x", b"x"));

        fs::write(dir.path().join(PASSWORD_FILE), "opaque-v1 00 11").unwrap();
        assert!(matches!(
            StoredPassword::load(dir.path()),
            Err(AuthError::Damaged { .. })
        ));
        fs::write(
            dir.path().join(PASSWORD_FILE),
            "argon2id-v1 65536 3 1 aa bb",
        )
        .unwrap();
        assert!(StoredPassword::load(dir.path()).unwrap().is_none());
        StoredPassword::remove(dir.path()).unwrap();
        assert!(StoredPassword::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn overlong_passwords_are_refused_before_hashing() {
        let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
        assert!(matches!(
            ClientPasswordLogin::start(&long),
            Err(AuthError::PasswordTooLong)
        ));
    }

    #[test]
    fn trust_store_survives_damage() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::load(dir.path()).unwrap();
        store.add(Fingerprint([3; 32]), "icebook\u{7}").unwrap();
        let mut text = fs::read_to_string(dir.path().join(TRUST_FILE)).unwrap();
        text.push_str("garbage line\n");
        fs::write(dir.path().join(TRUST_FILE), text).unwrap();
        let mut store = TrustStore::load(dir.path()).unwrap();
        assert!(store.contains(&Fingerprint([3; 32])));
        assert_eq!(store.devices()[0].1, "icebook");
        assert!(store.remove(&Fingerprint([3; 32])).unwrap());
        assert!(!TrustStore::load(dir.path())
            .unwrap()
            .contains(&Fingerprint([3; 32])));
    }

    #[test]
    fn limiter_backs_off_after_free_attempts() {
        let mut limiter = FailureLimiter::default();
        let addr: IpAddr = "192.168.0.5".parse().unwrap();
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(limiter.check(addr, start), None);
            limiter.fail(addr, start);
        }
        assert_eq!(limiter.check(addr, start), Some(1));
        assert_eq!(limiter.check(addr, start + Duration::from_secs(2)), None);
        limiter.fail(addr, start);
        assert_eq!(limiter.check(addr, start), Some(2));
        limiter.succeed(addr);
        assert_eq!(limiter.check(addr, start), None);
    }
}
