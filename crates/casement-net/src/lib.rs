//! Casement networking: who this device is, how it talks to peers and how it finds them.
//! See docs/ARCHITECTURE.md ("Сеть и безопасность").

pub mod audio;
pub mod auth;
pub mod client;
pub mod discovery;
mod files;
mod framing;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub mod glibc_compat;
pub mod host;
pub mod identity;
mod input;
pub mod policy;
pub mod screen;
#[cfg(test)]
mod tests;
pub mod tls;
pub mod transfer;
pub mod video;

use std::time::Duration;

pub use casement_proto::{ErrorCode, PROTOCOL_VERSION};
pub use identity::{Fingerprint, Identity, IdentityError};

/// Default UDP port for QUIC.
pub const DEFAULT_PORT: u16 = 47820;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Auth(#[from] auth::AuthError),
    #[error("network: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS: {0}")]
    Tls(String),
    #[error("cannot connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("connection: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("stream: {0}")]
    Stream(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("no answer from the computer")]
    Timeout,
    #[error("the computer's key changed: expected {}, got {}", expected.display(), seen.display())]
    KeyChanged {
        expected: Fingerprint,
        seen: Fingerprint,
    },
    #[error("the computer asks for a password")]
    PasswordRequired,
    #[error("wrong password")]
    AuthFailed,
    #[error("refused ({code:?}): {message}")]
    Refused {
        code: ErrorCode,
        message: String,
        retry_after: u32,
    },
}

impl From<rustls::Error> for NetError {
    fn from(e: rustls::Error) -> Self {
        NetError::Tls(e.to_string())
    }
}

impl From<rcgen::Error> for NetError {
    fn from(e: rcgen::Error) -> Self {
        NetError::Tls(e.to_string())
    }
}

pub(crate) fn transport_config() -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(Duration::from_secs(5)));
    config.max_idle_timeout(Some(
        Duration::from_secs(30).try_into().expect("valid timeout"),
    ));
    config
}

/// "linux", "windows", … for greetings and discovery.
pub fn os_name() -> &'static str {
    std::env::consts::OS
}

/// Best-effort human-readable operating system name for discovery and peer labels.
pub fn os_display_name() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/etc/os-release") {
            for key in ["PRETTY_NAME", "NAME"] {
                if let Some(value) = contents.lines().find_map(|line| {
                    line.strip_prefix(&format!("{key}="))
                        .map(|value| value.trim().trim_matches(['\"', '\'']).to_string())
                }) {
                    if !value.is_empty() && !value.chars().any(char::is_control) {
                        return value;
                    }
                }
            }
        }
        return "Linux".into();
    }
    #[cfg(windows)]
    {
        "Windows".into()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        std::env::consts::OS.to_string()
    }
}

#[cfg(test)]
mod os_display_tests {
    #[test]
    fn display_name_uses_a_nonempty_system_fallback() {
        let name = super::os_display_name();
        assert!(!name.trim().is_empty());
        assert!(!name.chars().any(char::is_control));
    }
}

#[cfg(test)]
mod test_support;
