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
