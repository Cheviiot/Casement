//! casement-host: accepts Casement connections to this computer without the window open.
//!
//! - Without arguments it runs in the user's graphical session: on Linux as the systemd
//!   user service `casement-host.service` (the screen comes from that session's Mutter),
//!   on Windows from the user's autostart. Settings are the application's
//!   (`~/.config/casement`, `%APPDATA%\casement` or `$CASEMENT_CONFIG_DIR`).
//! - `--service` (Windows) is the system service "Casement"; `--worker --desktop NAME` is
//!   what that service starts in the console session, with the machine's settings in
//!   `%ProgramData%\Casement` (see windows.rs).

// Started at login or by the service on Windows: no console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use casement_net::host::HostEvent;
use casement_net::transfer::HostFiles;
use casement_net::DEFAULT_PORT;

#[cfg(windows)]
mod windows;

/// Files sent by clients land in the user's Downloads folder; clients may browse and
/// download from the home folder (they control this desktop anyway).
fn user_files() -> HostFiles {
    HostFiles {
        incoming: glib::user_special_dir(glib::UserDirectory::Downloads)
            .or_else(|| Some(glib::home_dir().join("Downloads"))),
        shared: Some(glib::home_dir()),
    }
}

/// The same place as `casement-cli` uses: on Windows %APPDATA%\casement (GLib's user config
/// dir there is %LOCALAPPDATA%, which the CLI would not find).
fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CASEMENT_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("casement");
        }
    }
    glib::user_config_dir().join("casement")
}

/// Logs to `file` (a service has no terminal) or to stderr; info and up unless
/// $CASEMENT_LOG says otherwise.
fn init_logging(file: Option<PathBuf>) {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("CASEMENT_LOG")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    let file = file.and_then(|path| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Each worker appends (a desktop switch starts a new one); past 1 MB the log starts
        // over, keeping the previous one as .old.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
            let _ = std::fs::rename(&path, path.with_extension("old"));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    match file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .init(),
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--prepare") {
        return match casement_media::prepare_bundled_gstreamer() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(_) => std::process::ExitCode::FAILURE,
        };
    }
    casement_media::use_bundled_gstreamer();

    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("--service") {
        init_logging(Some(windows::machine_dir().join("service.log")));
        return match windows::run_service() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("not started by the service manager: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    #[cfg(windows)]
    let (dir, files) = if args.first().map(String::as_str) == Some("--worker") {
        let desktop = args
            .iter()
            .skip_while(|a| *a != "--desktop")
            .nth(1)
            .cloned()
            .unwrap_or_else(|| "Default".into());
        let machine = windows::machine_dir();
        init_logging(Some(machine.join("host.log")));
        tracing::info!("worker on the {desktop} desktop");
        windows::follow_input_desktop(desktop);
        // At the login screen nobody's home folder is in use: the shared public folders.
        let public = std::env::var_os("PUBLIC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Public"));
        let files = HostFiles {
            incoming: Some(public.join("Downloads")),
            shared: Some(public),
        };
        (machine, files)
    } else {
        init_logging(None);
        (config_dir(), user_files())
    };
    #[cfg(not(windows))]
    let (dir, files) = {
        let _ = args;
        init_logging(None);
        (config_dir(), user_files())
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!("cannot start: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(dir, files))
}

async fn run(dir: PathBuf, files: HostFiles) -> std::process::ExitCode {
    let name = glib::host_name().to_string();
    let port = std::env::var("CASEMENT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let running = match casement_media::service::start_host(&dir, &name, port, true, false, files) {
        Ok(running) => running,
        Err(e) => {
            tracing::error!("cannot accept connections: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    tracing::info!(
        "accepting connections on UDP {port} as {name}, key {}",
        running.host.fingerprint().display()
    );
    if let Some(e) = &running.screen_error {
        tracing::warn!("screen not shared: {e}");
    }
    if !running.has_password {
        tracing::info!("no access password: only trusted devices can connect");
    }
    #[cfg(windows)]
    if dir == windows::machine_dir() {
        let fingerprint = running.host.fingerprint().display();
        tokio::spawn(windows::publish_status(dir.clone(), fingerprint));
    }
    let host = running.host.clone();
    tokio::spawn(async move { host.run().await });
    let mut events = running.events;
    let term = terminate_signal();
    tokio::pin!(term);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = &mut term => break,
            Some(event) = events.recv() => match event {
                HostEvent::LoggedIn { peer, fingerprint, name, trusted } => tracing::info!(
                    "{peer}: {name} logged in{} (key {})",
                    if trusted { " as a trusted device" } else { " with the password" },
                    fingerprint.display()),
                HostEvent::Refused { peer, reason } => tracing::info!("{peer}: refused: {reason}"),
                HostEvent::FileReceived { peer, path, size } => {
                    tracing::info!("{peer}: received {} ({size} bytes)", path.display())
                }
                HostEvent::FileSent { peer, path, size } => {
                    tracing::info!("{peer}: downloaded {} ({size} bytes)", path.display())
                }
                HostEvent::Closed { peer } => tracing::info!("{peer}: closed"),
            },
        }
    }
    running.host.close();
    std::process::ExitCode::SUCCESS
}

/// SIGTERM (systemd stop) ends the service like Ctrl+C.
#[cfg(unix)]
async fn terminate_signal() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut signal) => {
            signal.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn terminate_signal() {
    std::future::pending::<()>().await
}
