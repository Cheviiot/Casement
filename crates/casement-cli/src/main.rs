//! casement-cli: setting up this computer without the window (`setup`, `status`,
//! `accept`, `password`, `trusted`, see setup.rs) and the network core for checks between
//! machines (`listen`, `connect`, `discover` and the probes).

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use casement_net::client::{self, ClientInfo};
use casement_net::discovery::{Browser, DiscoveryEvent};
use casement_net::host::HostEvent;
use casement_net::{Fingerprint, Identity, NetError, DEFAULT_PORT};

mod setup;

#[derive(Parser)]
#[command(
    version,
    about = "Casement без окна: настройка этого компьютера и проверки"
)]
struct Cli {
    /// Папка настроек (по умолчанию та же, что у приложения)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
// Parsed once at start: the size of the largest variant does not matter.
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Мастер настройки: пароль, приём подключений, доверенные устройства
    Setup,
    /// Как настроен этот компьютер
    Status,
    /// Принимать подключения (служба casement-host) или нет
    Accept {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Задать (set) или убрать (clear) пароль доступа
    Password {
        #[arg(value_parser = ["set", "clear"])]
        action: String,
    },
    /// Доверенные устройства: list, remove НОМЕР|ИМЯ|НАЧАЛО_ОТПЕЧАТКА
    Trusted {
        #[command(subcommand)]
        action: Trusted,
    },
    /// Отпечаток ключа этого устройства
    Fingerprint,
    /// Принимать подключения из терминала (до Ctrl+C) и объявлять компьютер в сети
    Listen {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Also accept public addresses (off: only LAN and Tailscale)
        #[arg(long)]
        any_source: bool,
        /// Do not share the screen, only accept logins
        #[arg(long)]
        no_screen: bool,
        /// Accept files from clients into this directory
        #[arg(long)]
        files_dir: Option<PathBuf>,
        /// Let clients browse and download files from this directory
        #[arg(long)]
        shared_dir: Option<PathBuf>,
    },
    /// Подключиться, войти и проверить связь, экран, звук, буфер, файлы
    Connect {
        target: String,
        /// Expected key fingerprint (64 hex digits); without it you confirm the key shown
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long, default_value_t = 5)]
        pings: u32,
        /// Ask for the screen and write the H.264 stream to this file
        #[arg(long)]
        record: Option<PathBuf>,
        /// Frames to record
        #[arg(long, default_value_t = 120)]
        frames: u32,
        /// Move the remote pointer to this position before recording, e.g. 200,150
        #[arg(long)]
        pointer: Option<String>,
        /// While recording, move the remote pointer in a circle at 60 Hz (load test)
        #[arg(long)]
        wiggle: bool,
        /// Put this text on the remote computer's clipboard
        #[arg(long)]
        clipboard: Option<String>,
        /// Stay connected this many seconds, printing the remote clipboard as it changes
        #[arg(long, default_value_t = 0)]
        clipboard_wait: u64,
        /// Ask for a virtual monitor of this size instead of the host's screen, e.g. 1280x720
        #[arg(long = "virtual")]
        virtual_screen: Option<String>,
        /// Send these files to the host's Downloads folder
        #[arg(long = "send")]
        send_files: Vec<PathBuf>,
        /// List this folder of the host's shared folder ("" for the top)
        #[arg(long = "ls")]
        list: Vec<String>,
        /// Download this file from the host's shared folder into --into
        #[arg(long = "fetch")]
        fetch: Vec<String>,
        /// Show this monitor of the host (an id from the list printed on connect)
        #[arg(long)]
        monitor: Option<String>,
        /// Where --fetch puts files
        #[arg(long, default_value = ".")]
        into: PathBuf,
        /// Receive the host's sound for this many seconds and report packets and loss
        #[arg(long, default_value_t = 0)]
        audio: u64,
        /// With --audio: also write the sound to this Ogg Opus file (Linux)
        #[arg(long)]
        audio_record: Option<PathBuf>,
        /// After the first frames, ask the virtual monitor to become this size, e.g. 1600x900
        #[arg(long)]
        resize: Option<String>,
        /// Measure input-to-frame latency over this many pointer moves
        #[arg(long, default_value_t = 0)]
        latency: u32,
    },
    /// Проверка: прочитать буфер обмена этого компьютера через GNOME (или положить --set)
    ClipboardProbe {
        #[arg(long)]
        set: Option<String>,
        #[arg(long, default_value_t = 10)]
        hold: u64,
    },
    /// Проверка: сравнить способы декодирования H.264 из файла connect --record
    #[cfg(target_os = "linux")]
    DecodeBench { file: PathBuf },
    /// Компьютеры с Casement в локальной сети
    Discover {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
}

#[derive(Subcommand)]
enum Trusted {
    /// Список доверенных устройств
    List,
    /// Перестать доверять устройству: оно снова спросит пароль
    Remove {
        /// Номер из списка, имя или начало отпечатка
        device: String,
    },
}

fn config_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) =
        explicit.or_else(|| std::env::var_os("CASEMENT_CONFIG_DIR").map(PathBuf::from))
    {
        return Ok(dir);
    }
    if cfg!(windows) {
        let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
        return Ok(PathBuf::from(appdata).join("casement"));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(base.join("casement"))
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "casement".into())
}

fn resolve(target: &str) -> Result<SocketAddr> {
    let with_port = if target.parse::<SocketAddr>().is_ok()
        || target
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok())
    {
        target.to_string()
    } else if target.contains(':') && !target.starts_with('[') {
        format!("[{target}]:{DEFAULT_PORT}")
    } else {
        format!("{target}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {target}"))?
        .min_by_key(|a| a.is_ipv6())
        .with_context(|| format!("no address for {target}"))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Ошибка: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    #[cfg(windows)]
    casement_media::use_bundled_gstreamer();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("CASEMENT_LOG"))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let dir = config_dir(cli.config)?;
    let identity = Identity::load_or_create(&dir)?;

    match cli.command {
        Command::Setup => setup::wizard(&dir, &identity, &host_name())?,
        Command::Status => setup::status(&dir, &identity, &host_name())?,
        Command::Accept { state } => setup::accept(state == "on")?,
        Command::Password { action } => setup::password(&dir, &action)?,
        Command::Trusted { action } => match action {
            Trusted::List => setup::trusted_list(&dir)?,
            Trusted::Remove { device } => setup::trusted_remove(&dir, &device)?,
        },
        Command::Fingerprint => {
            println!("{}", identity.fingerprint().display());
            println!("{}", identity.fingerprint().to_hex());
        }
        Command::Listen {
            port,
            any_source,
            no_screen,
            files_dir,
            shared_dir,
        } => {
            let name = host_name();
            let files = casement_net::transfer::HostFiles {
                incoming: files_dir,
                shared: shared_dir,
            };
            let running = start_host(&dir, &name, port, !no_screen, any_source, files)
                .map_err(anyhow::Error::msg)?;
            if !running.has_password {
                eprintln!("No access password set: only trusted devices can connect (casement-cli password set).");
            }
            match &running.screen_error {
                None => println!("Screen sharing with {}", encoder_name()),
                Some(e) => eprintln!("Screen not shared: {e}"),
            }
            println!(
                "Listening on UDP {} as {name}, key {}",
                running.host.local_addr()?,
                running.host.fingerprint().display()
            );
            let host = running.host.clone();
            tokio::spawn(async move { host.run().await });
            let mut events = running.events;
            let terminate = terminate_signal();
            tokio::pin!(terminate);
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = &mut terminate => break,
                    Some(event) = events.recv() => match event {
                        HostEvent::LoggedIn { peer, fingerprint, name, trusted } => println!(
                            "{peer}: {name} logged in{} (key {})",
                            if trusted { " as a trusted device" } else { " with the password" },
                            fingerprint.display()),
                        HostEvent::Refused { peer, reason } => println!("{peer}: refused: {reason}"),
                        HostEvent::FileReceived { peer, path, size } => println!("{peer}: received {} ({size} bytes)", path.display()),
                        HostEvent::FileSent { peer, path, size } => println!("{peer}: downloaded {} ({size} bytes)", path.display()),
                        HostEvent::Closed { peer } => println!("{peer}: closed"),
                    },
                }
            }
            running.host.close();
        }
        Command::Connect {
            target,
            fingerprint,
            pings,
            record,
            frames,
            pointer,
            wiggle,
            clipboard,
            clipboard_wait,
            latency,
            virtual_screen,
            resize,
            send_files,
            list,
            fetch,
            into,
            audio,
            audio_record,
            monitor,
        } => {
            let addr = resolve(&target)?;
            let expected = match fingerprint {
                Some(hex) => Some(
                    Fingerprint::from_hex(&hex).context("the fingerprint must be 64 hex digits")?,
                ),
                None => None,
            };
            let info = ClientInfo {
                name: host_name(),
                os: casement_net::os_name().into(),
                remember: true,
            };
            let handshake = match client::connect(&identity, addr, expected, &info).await {
                Err(NetError::KeyChanged { expected, seen }) => bail!(
                    "WARNING: the key of {target} changed!\n  expected {}\n  got      {}\nDo not connect unless you know why.",
                    expected.display(), seen.display()),
                other => other?,
            };
            println!(
                "{} ({}) at {addr}, key {}",
                handshake.host_name,
                handshake.host_os,
                handshake.host_fingerprint.display()
            );
            if expected.is_none() {
                println!(
                    "First connection: the key is taken as it is (pin it with --fingerprint)."
                );
            }
            let host_key = handshake.host_fingerprint;
            let password = if handshake.needs_password() {
                Some(read_password("Access password: ")?)
            } else {
                None
            };
            let mut session = handshake.login(password.as_deref()).await?;
            println!(
                "Logged in{}.",
                if session.remembered {
                    "; this device is now trusted"
                } else {
                    ""
                }
            );
            for _ in 0..pings {
                let rtt = session.ping().await?;
                println!("ping {:.2} ms", rtt.as_secs_f64() * 1000.0);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            println!("Next time: --fingerprint {}", host_key.to_hex());
            for path in &send_files {
                let start = std::time::Instant::now();
                let saved = session.send_file(path, |_, _| {}).await?;
                let size = std::fs::metadata(path)?.len();
                let secs = start.elapsed().as_secs_f64();
                println!(
                    "Sent {} as {saved:?}: {size} bytes in {secs:.2} s ({:.1} MB/s)",
                    path.display(),
                    size as f64 / secs / 1e6
                );
            }
            for path in &list {
                let listing = session.list_files(path).await?;
                println!("/{}:", listing.path);
                for entry in &listing.entries {
                    if entry.is_dir {
                        println!("  {}/", entry.name);
                    } else {
                        println!("  {}  {} bytes", entry.name, entry.size);
                    }
                }
                if listing.truncated {
                    println!("  (more entries not shown)");
                }
            }
            for path in &fetch {
                let start = std::time::Instant::now();
                let mut size = 0;
                let saved = session
                    .fetch_file(path, &into, |_, total| size = total)
                    .await?;
                let secs = start.elapsed().as_secs_f64();
                println!(
                    "Downloaded {path} to {}: {size} bytes in {secs:.2} s ({:.1} MB/s)",
                    saved.display(),
                    size as f64 / secs / 1e6
                );
            }
            if audio > 0 {
                receive_audio(&mut session, audio, audio_record.as_deref()).await?;
            }
            if record.is_none() && clipboard.is_none() && clipboard_wait == 0 && latency == 0 {
                session.close();
                return Ok(());
            }
            let request = match &virtual_screen {
                Some(size) => {
                    let (w, h) = size.split_once('x').context("--virtual WIDTHxHEIGHT")?;
                    casement_net::screen::ScreenRequest {
                        max_width: w.parse()?,
                        max_height: h.parse()?,
                        virtual_monitor: true,
                        ..Default::default()
                    }
                }
                None => casement_net::screen::ScreenRequest {
                    monitor: monitor.clone().unwrap_or_default(),
                    ..Default::default()
                },
            };
            let mut screen = session.start_screen_with(request).await?;
            for m in &screen.monitors {
                println!(
                    "monitor {}{}: {} {}x{}{}",
                    m.id,
                    if m.primary { " (primary)" } else { "" },
                    m.name,
                    m.width,
                    m.height,
                    if m.id == screen.monitor {
                        " — shown"
                    } else {
                        ""
                    }
                );
            }
            println!("Screen {}x{} {}", screen.width, screen.height, screen.codec);
            let (mut control, mut messages, handle) = session.split();
            let input = handle.input_sender();
            if let Some(pointer) = pointer {
                let (x, y) = pointer.split_once(',').context("--pointer X,Y")?;
                let motion = casement_proto::PointerMotion {
                    x: x.trim().parse()?,
                    y: y.trim().parse()?,
                };
                input.send(&casement_proto::Input {
                    kind: Some(casement_proto::input::Kind::Motion(motion)),
                })?;
            }
            let listener = tokio::spawn(async move {
                while let Ok(Some(message)) = messages.next().await {
                    let casement_net::client::HostMessage::Clipboard(text) = message;
                    println!("Host clipboard: {text:?}");
                }
            });
            if let Some(text) = &clipboard {
                control.clipboard(text).await?;
                println!("Clipboard sent: {text:?}");
            }
            let wiggler = wiggle.then(|| {
                let sender = input.clone();
                let (cx, cy, r) = (
                    screen.width as f64 / 2.0,
                    screen.height as f64 / 2.0,
                    screen.height as f64 / 4.0,
                );
                tokio::spawn(async move {
                    let mut tick = tokio::time::interval(Duration::from_millis(16));
                    let mut step = 0u64;
                    loop {
                        tick.tick().await;
                        step += 1;
                        let angle = step as f64 * 0.1;
                        let motion = casement_proto::PointerMotion {
                            x: cx + r * angle.cos(),
                            y: cy + r * angle.sin(),
                        };
                        let _ = sender.send(&casement_proto::Input {
                            kind: Some(casement_proto::input::Kind::Motion(motion)),
                        });
                    }
                })
            });
            if latency > 0 {
                // Frames arrive only when the screen changes: wait for quiet, move the pointer,
                // time the first frame. Measures input -> capture -> encode -> network.
                let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel();
                let mut video = screen.video;
                tokio::spawn(async move {
                    while let Ok(Some(_)) = casement_net::screen::read_frame(&mut video).await {
                        if frames_tx.send(std::time::Instant::now()).is_err() {
                            return;
                        }
                    }
                });
                let mut samples = Vec::new();
                for i in 0..latency {
                    // Drain until 300 ms without frames.
                    loop {
                        match tokio::time::timeout(Duration::from_millis(300), frames_rx.recv())
                            .await
                        {
                            Ok(Some(_)) => continue,
                            Ok(None) => bail!("the video stream ended"),
                            Err(_) => break,
                        }
                    }
                    let x = 300.0 + (i % 2) as f64 * 400.0;
                    let motion = casement_proto::PointerMotion { x, y: 300.0 };
                    let sent = std::time::Instant::now();
                    input.send(&casement_proto::Input {
                        kind: Some(casement_proto::input::Kind::Motion(motion)),
                    })?;
                    match tokio::time::timeout(Duration::from_secs(2), frames_rx.recv()).await {
                        Ok(Some(arrived)) => {
                            samples.push(arrived.duration_since(sent).as_secs_f64() * 1000.0)
                        }
                        _ => println!("move {i}: no frame within 2 s"),
                    }
                }
                samples.sort_by(f64::total_cmp);
                if !samples.is_empty() {
                    let pick = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
                    println!("input -> frame latency over {} moves: min {:.1} ms, median {:.1} ms, p90 {:.1} ms, max {:.1} ms",
                        samples.len(), pick(0.0), pick(0.5), pick(0.9), pick(1.0));
                }
                handle.close_gracefully().await;
                return Ok(());
            }
            if let Some(size) = &resize {
                let (w, h) = size.split_once('x').context("--resize WIDTHxHEIGHT")?;
                let (w, h): (u32, u32) = (w.parse()?, h.parse()?);
                tokio::time::sleep(Duration::from_millis(1500)).await;
                control.resize(w, h).await?;
                println!("Asked for {w}x{h}");
            }
            if let Some(path) = record {
                let mut out = std::fs::File::create(&path)?;
                let start = std::time::Instant::now();
                let (mut bytes, mut keyframes, mut got) = (0usize, 0u32, 0u32);
                for _ in 0..frames {
                    let Some(frame) = casement_net::screen::read_frame(&mut screen.video).await?
                    else {
                        break;
                    };
                    bytes += frame.data.len();
                    keyframes += frame.keyframe as u32;
                    got += 1;
                    out.write_all(&frame.data)?;
                }
                let secs = start.elapsed().as_secs_f64();
                println!("{got} frames in {secs:.1} s ({:.1} fps), {keyframes} keyframes, {:.1} Mbit/s -> {}",
                    got as f64 / secs, bytes as f64 * 8.0 / secs / 1e6, path.display());
            } else {
                // Keep the video flowing (the host stops sharing when nobody reads).
                tokio::spawn(async move {
                    while let Ok(Some(_)) =
                        casement_net::screen::read_frame(&mut screen.video).await
                    {
                    }
                });
            }
            if clipboard_wait > 0 {
                tokio::time::sleep(Duration::from_secs(clipboard_wait)).await;
            }
            if let Some(w) = wiggler {
                w.abort();
            }
            listener.abort();
            handle.close_gracefully().await;
        }
        Command::ClipboardProbe { set, hold } => {
            match clipboard_probe(set.as_deref(), Duration::from_secs(hold)).await {
                Ok(Some(text)) => println!("Clipboard: {text:?}"),
                Ok(None) => println!("Clipboard holds no text"),
                Err(e) => bail!("clipboard probe: {e}"),
            }
        }
        #[cfg(target_os = "linux")]
        Command::DecodeBench { file } => {
            let variants = [
                (
                    "VA, CPU convert to BGRA",
                    "vah264dec ! videoconvert ! video/x-raw,format=BGRA",
                ),
                (
                    "VA, GPU convert to BGRA",
                    "vah264dec ! vapostproc ! video/x-raw,format=BGRA",
                ),
                (
                    "VA, DMABuf (zero copy)",
                    "vah264dec ! video/x-raw(memory:DMABuf)",
                ),
                (
                    "VA, GPU convert to DMABuf",
                    "vah264dec ! vapostproc ! video/x-raw(memory:DMABuf)",
                ),
                (
                    "libav, CPU convert to BGRA",
                    "avdec_h264 ! videoconvert ! video/x-raw,format=BGRA",
                ),
            ];
            for (name, tail) in variants {
                match casement_media::decode_bench(&file, tail) {
                    Ok((frames, wall, cpu)) => println!(
                        "{name}: {frames} frames in {wall:.2} s ({:.0} fps), CPU {:.1} ms per frame",
                        frames as f64 / wall,
                        cpu * 1000.0 / frames.max(1) as f64
                    ),
                    Err(e) => println!("{name}: {e}"),
                }
            }
        }
        Command::Discover { seconds } => {
            let browser = Browser::start()?;
            let own = identity.fingerprint();
            let deadline = tokio::time::sleep(Duration::from_secs(seconds));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    event = browser.next() => match event {
                        Some(DiscoveryEvent::Found(peer)) if peer.fingerprint != own => println!(
                            "{} ({}) {:?} port {} key {}",
                            peer.name, peer.os, peer.addresses, peer.port, peer.fingerprint.display()),
                        Some(DiscoveryEvent::Lost { id }) => println!("gone: {id}"),
                        Some(_) => {}
                        None => break,
                    },
                }
            }
        }
    }
    Ok(())
}

/// For scripted checks the password may come from this variable instead of the terminal.
/// Receives sound for `seconds`, counting packets and gaps in their timestamps.
async fn receive_audio(
    session: &mut client::Session,
    seconds: u64,
    record: Option<&std::path::Path>,
) -> Result<()> {
    let format = session.start_audio().await?;
    println!(
        "Sound: {} {} Hz, {} channels",
        format.codec, format.rate, format.channels
    );
    #[cfg(target_os = "linux")]
    let file = match record {
        Some(path) => {
            Some(casement_media::OpusFile::create(path, &format).map_err(anyhow::Error::msg)?)
        }
        None => None,
    };
    #[cfg(not(target_os = "linux"))]
    if record.is_some() {
        bail!("recording sound needs Linux");
    }
    let receiver = session.audio_receiver();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let (mut packets, mut bytes, mut gaps) = (0u64, 0u64, 0u64);
    let mut first_pts: Option<u64> = None;
    let mut last_pts: Option<u64> = None;
    while let Ok(packet) = tokio::time::timeout_at(deadline, receiver.next()).await {
        let packet = packet?;
        // Packets are 20 ms apart; a longer step means some were lost on the way.
        if last_pts.is_some_and(|last| packet.pts_us.saturating_sub(last) > 30_000) {
            gaps += 1;
        }
        first_pts.get_or_insert(packet.pts_us);
        last_pts = Some(packet.pts_us);
        packets += 1;
        bytes += packet.data.len() as u64;
        #[cfg(target_os = "linux")]
        if let Some(file) = &file {
            file.push(&packet);
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(file) = file {
        file.finish();
    }
    println!(
        "Received {packets} packets ({:.1} per s, {:.0} kbit/s), {gaps} gaps, {:.1} ms apart",
        packets as f64 / seconds as f64,
        bytes as f64 * 8.0 / seconds as f64 / 1000.0,
        match (first_pts, last_pts) {
            (Some(first), Some(last)) if packets > 1 =>
                (last - first) as f64 / (packets - 1) as f64 / 1000.0,
            _ => 0.0,
        }
    );
    Ok(())
}

use setup::read_password;

// The screen comes from casement-media on Linux (GNOME) and Windows; the clipboard probe is
// GNOME only. Elsewhere the CLI still listens, logs in and moves files.
#[cfg(any(target_os = "linux", windows))]
use casement_media::service::start_host;

#[cfg(any(target_os = "linux", windows))]
fn encoder_name() -> &'static str {
    casement_media::encoder_name().unwrap_or("?")
}

#[cfg(target_os = "linux")]
async fn clipboard_probe(set: Option<&str>, hold: Duration) -> Result<Option<String>, String> {
    casement_media::clipboard_probe(set, hold).await
}

#[cfg(not(any(target_os = "linux", windows)))]
fn encoder_name() -> &'static str {
    "none"
}

#[cfg(not(target_os = "linux"))]
async fn clipboard_probe(_set: Option<&str>, _hold: Duration) -> Result<Option<String>, String> {
    Err("the clipboard probe needs GNOME on Linux".into())
}

#[cfg(not(any(target_os = "linux", windows)))]
struct RunningHost {
    host: std::sync::Arc<casement_net::host::Host>,
    events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    screen_error: Option<String>,
    has_password: bool,
    _announcement: Option<casement_net::discovery::Announcement>,
}

#[cfg(not(any(target_os = "linux", windows)))]
fn start_host(
    dir: &std::path::Path,
    name: &str,
    port: u16,
    _share_screen: bool,
    allow_any_source: bool,
    files: casement_net::transfer::HostFiles,
) -> Result<RunningHost, String> {
    use casement_net::auth::TrustStore;
    use casement_net::host::{Host, HostConfig};
    let identity = Identity::load_or_create(dir).map_err(|e| e.to_string())?;
    let password = casement_net::auth::StoredPassword::load(dir).map_err(|e| e.to_string())?;
    let trust = TrustStore::load(dir).map_err(|e| e.to_string())?;
    let has_password = password.is_some();
    let fingerprint = identity.fingerprint();
    let config = HostConfig {
        name: name.to_string(),
        os: casement_net::os_name().into(),
        bind: SocketAddr::from(([0u16; 8], port)),
        allow_any_source,
        screen: None,
        files_dir: files.incoming,
        shared_dir: files.shared,
        audio: None,
        settings_dir: Some(dir.to_path_buf()),
    };
    let (host, events) =
        Host::bind(identity, config, password, trust).map_err(|e| e.to_string())?;
    let announcement = casement_net::discovery::Announcement::start(name, port, &fingerprint).ok();
    Ok(RunningHost {
        host: std::sync::Arc::new(host),
        events,
        screen_error: Some("screen sharing on this system comes in a later version".into()),
        has_password,
        _announcement: announcement,
    })
}

/// SIGTERM (systemd stop) ends `listen` like Ctrl+C, so clients get a proper close.
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
