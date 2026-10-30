//! Diagnose the same automatic routing used by the GTK application, without input.
//! route_probe CONFIG_DIR FINGERPRINT DEVICE_NAME PRIMARY_SOCKET [--screen]

use std::sync::Arc;
use std::time::{Duration, Instant};

use casement_net::{client::ClientInfo, routes, Fingerprint, Identity};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 5 {
        return Err("expected CONFIG_DIR FINGERPRINT DEVICE_NAME PRIMARY_SOCKET [--screen]".into());
    }
    let identity = Arc::new(Identity::load_or_create(std::path::Path::new(&args[1]))?);
    let expected = Fingerprint::from_hex(&args[2]).ok_or("invalid fingerprint")?;
    let primary: std::net::SocketAddr = args[4].parse()?;
    let started = Instant::now();
    let (handshake, selected) = routes::connect(
        identity,
        vec![(primary.ip().to_string(), primary.port())],
        Some(expected),
        ClientInfo {
            name: "Casement route probe".into(),
            os: casement_net::os_name().into(),
            remember: false,
        },
        args[3].clone(),
    )
    .await?;
    println!(
        "route={selected} handshake_ms={:.0}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    if handshake.needs_password() {
        handshake.close();
        return Err("device is not trusted; no login attempted".into());
    }
    let mut session = handshake.login(None).await?;
    println!("login=ok endpoints={:?}", session.host_endpoints);
    println!(
        "ping_ms={:.0}",
        session.ping().await?.as_secs_f64() * 1000.0
    );
    if args.iter().any(|arg| arg == "--screen") {
        let screen_started = Instant::now();
        let mut screen = session
            .start_screen_with(casement_net::screen::ScreenRequest {
                max_width: 1280,
                max_height: 720,
                bitrate_kbps: 2000,
                adaptive_bitrate: true,
                max_bitrate_kbps: 4000,
                ..Default::default()
            })
            .await?;
        let frame = tokio::time::timeout(
            Duration::from_secs(20),
            casement_net::screen::read_frame(&mut screen.video),
        )
        .await??
        .ok_or("screen ended before the first frame")?;
        println!(
            "screen={}x{} first_frame_ms={:.0} bytes={}",
            screen.width,
            screen.height,
            screen_started.elapsed().as_secs_f64() * 1000.0,
            frame.data.len()
        );
    }
    session.close();
    Ok(())
}
