//! casement-host: accepts Casement connections to this computer without the window open.
//! Runs as the systemd user service `casement-host.service` inside the graphical session
//! (the screen comes from that session's Mutter). Settings are the application's:
//! `~/.config/casement` or `$CASEMENT_CONFIG_DIR`. Restart it after changing the password
//! or the trusted devices; the application does that itself.

// Started at login on Windows: no console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use casement_net::host::HostEvent;
use casement_net::DEFAULT_PORT;

/// Files sent by clients land in the user's Downloads folder; clients may browse and
/// download from the home folder (they control this desktop anyway).
fn downloads_dir() -> Option<PathBuf> {
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .or_else(|| Some(glib::home_dir().join("Downloads")))
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

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    casement_media::use_bundled_gstreamer();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("CASEMENT_LOG"))
        .with_writer(std::io::stderr)
        .init();
    let dir = config_dir();
    let name = glib::host_name().to_string();
    let port = std::env::var("CASEMENT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let running = match casement_media::service::start_host(
        &dir,
        &name,
        port,
        true,
        false,
        casement_net::transfer::HostFiles {
            incoming: downloads_dir(),
            shared: Some(glib::home_dir()),
        },
    ) {
        Ok(running) => running,
        Err(e) => {
            eprintln!("cannot accept connections: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!(
        "accepting connections on UDP {port} as {name}, key {}",
        running.host.fingerprint().display()
    );
    if let Some(e) = &running.screen_error {
        println!("screen not shared: {e}");
    }
    if !running.has_password {
        println!("no access password: only trusted devices can connect");
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
                HostEvent::LoggedIn { peer, fingerprint, name, trusted } => println!(
                    "{peer}: {name} logged in{} (key {})",
                    if trusted { " as a trusted device" } else { " with the password" }, fingerprint.display()),
                HostEvent::Refused { peer, reason } => println!("{peer}: refused: {reason}"),
                HostEvent::FileReceived { peer, path, size } => println!("{peer}: received {} ({size} bytes)", path.display()),
                HostEvent::FileSent { peer, path, size } => println!("{peer}: downloaded {} ({size} bytes)", path.display()),
                HostEvent::Closed { peer } => println!("{peer}: closed"),
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
