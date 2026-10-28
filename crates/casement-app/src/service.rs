//! The background host from the window: `casement_media::autostart` off the main loop.

use casement_media::autostart;

/// Runs blocking work (systemctl, reg) away from the main loop.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    crate::core::run(async move { tokio::task::spawn_blocking(work).await.expect("task") }).await
}

pub fn installed() -> bool {
    autostart::installed()
}

pub async fn enabled() -> bool {
    blocking(autostart::enabled).await
}

pub async fn set_enabled(on: bool) -> Result<(), String> {
    blocking(move || autostart::set_enabled(on)).await
}

pub async fn ensure_running() {
    blocking(autostart::ensure_running).await
}

/// What the window shows about the system service's settings (Windows), which only
/// administrators may read or change.
pub struct MachineAccess {
    pub fingerprint: String,
    pub password: bool,
    pub trusted: usize,
}

/// Windows with the system service: this computer's key, password and trusted devices are
/// the machine's, not this user's.
pub fn machine() -> bool {
    #[cfg(windows)]
    return autostart::system_service();
    #[cfg(not(windows))]
    false
}

/// The machine's settings as the service last published them.
pub fn machine_access() -> Option<MachineAccess> {
    #[cfg(windows)]
    return autostart::MachineStatus::load().map(|s| MachineAccess {
        fingerprint: s.fingerprint,
        password: s.password,
        trusted: s.trusted,
    });
    #[cfg(not(windows))]
    None
}

/// Opens the setup wizard as administrator and waits until it is closed.
pub async fn configure_machine() -> Result<(), String> {
    #[cfg(windows)]
    return blocking(autostart::configure_as_admin).await;
    #[cfg(not(windows))]
    Err("only on Windows".into())
}
