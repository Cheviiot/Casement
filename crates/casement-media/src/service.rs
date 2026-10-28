//! One way to run a host, shared by the `casement-host` service, `casement-cli listen` and
//! the application's fallback: settings from the directory, the screen when available
//! (GNOME or Windows), an mDNS announcement.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use casement_net::auth::{StoredPassword, TrustStore};
use casement_net::discovery::Announcement;
use casement_net::host::{Host, HostConfig, HostEvent};
use casement_net::screen::ScreenProvider;
use casement_net::{Identity, NetError};

pub struct RunningHost {
    pub host: Arc<Host>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    /// Why the screen is not shared, if it is not.
    pub screen_error: Option<String>,
    pub has_password: bool,
    _announcement: Option<Announcement>,
}

/// Binds the host; call inside a Tokio runtime, then spawn `host.run()`.
pub fn start_host(
    dir: &Path,
    name: &str,
    port: u16,
    share_screen: bool,
    allow_any_source: bool,
    files: casement_net::transfer::HostFiles,
) -> Result<RunningHost, String> {
    let identity = Identity::load_or_create(dir).map_err(|e| e.to_string())?;
    let password = StoredPassword::load(dir).map_err(|e| e.to_string())?;
    let trust = TrustStore::load(dir).map_err(|e| e.to_string())?;
    let (screen, screen_error) = if share_screen {
        #[cfg(target_os = "linux")]
        let screen = crate::GnomeScreen::new();
        #[cfg(windows)]
        let screen = crate::WindowsScreen::new();
        #[cfg(not(any(target_os = "linux", windows)))]
        let screen: Result<Arc<crate::GnomeScreen>, String> = Err("no screen capture here".into());
        match screen {
            Ok(screen) => (Some(screen as Arc<dyn ScreenProvider>), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, Some("screen sharing is off".into()))
    };
    let has_password = password.is_some();
    let config = HostConfig {
        name: name.to_string(),
        os: casement_net::os_name().into(),
        bind: SocketAddr::from(([0u16; 8], port)),
        allow_any_source,
        screen,
        files_dir: files.incoming,
        shared_dir: files.shared,
        settings_dir: Some(dir.to_path_buf()),
        // Sound does not need the screen: it is offered whenever this system can encode it.
        audio: crate::DesktopAudio::new()
            .map_err(|e| tracing::info!("sound not shared: {e}"))
            .ok()
            .map(|audio| audio as Arc<dyn casement_net::audio::AudioProvider>),
    };
    let fingerprint = identity.fingerprint();
    let (host, events) = Host::bind(identity, config, password, trust).map_err(|e| match e {
        NetError::Io(io) if io.kind() == std::io::ErrorKind::AddrInUse => {
            format!("UDP port {port} is already used by another program")
        }
        other => other.to_string(),
    })?;
    let announcement = Announcement::start(name, port, &fingerprint).ok();
    Ok(RunningHost {
        host: Arc::new(host),
        events,
        screen_error,
        has_password,
        _announcement: announcement,
    })
}
