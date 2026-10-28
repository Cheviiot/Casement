//! The host's real screen. On GNOME: Mutter's RemoteDesktop and ScreenCast D-Bus APIs (the
//! ones gnome-remote-desktop uses: no portal dialog, any process in the user's session may
//! call them), the PipeWire stream they create, and a GStreamer H.264 encoder; input goes
//! back through the same Mutter session. On Windows: see `windows.rs`.

mod audio;
#[cfg(target_os = "linux")]
mod capture;
mod frames;
#[cfg(target_os = "linux")]
mod mutter;
mod opus;
#[cfg(target_os = "linux")]
mod pipe;
pub mod service;
mod text;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use casement_net::screen::{ScreenProvider, ScreenRequest, StartFuture};

pub use audio::{DesktopAudio, OpusFile};
#[cfg(target_os = "linux")]
pub use capture::encoder_name;
#[cfg(windows)]
pub use opus::Decoder as OpusDecoder;
#[cfg(windows)]
pub use windows::{encoder_name, WindowsScreen};

/// Shares a monitor (the primary one unless the client asks for another) of the GNOME
/// session this process runs in.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub struct GnomeScreen;

#[cfg(target_os = "linux")]
impl GnomeScreen {
    /// Fails when GStreamer or the H.264 encoders are missing.
    pub fn new() -> Result<Arc<Self>, String> {
        gstreamer::init().map_err(|e| format!("GStreamer: {e}"))?;
        encoder_name()
            .ok_or_else(|| "no H.264 encoder (vah264enc, openh264enc or x264enc)".to_string())?;
        Ok(Arc::new(Self))
    }
}

#[cfg(target_os = "linux")]
impl ScreenProvider for GnomeScreen {
    fn start(&self, request: ScreenRequest) -> StartFuture {
        Box::pin(async move {
            let virtual_size = request
                .virtual_monitor
                .then(|| capture::virtual_size(request.max_width, request.max_height));
            let session = mutter::Session::start(virtual_size, &request.monitor)
                .await
                .map_err(|e| format!("GNOME screen sharing: {e}"))?;
            capture::start(session, virtual_size).await
        })
    }
}

#[cfg(target_os = "linux")]
/// A check without a keyboard: with `set`, offers that text as this computer's clipboard
/// for `hold` and returns None; without it, returns the clipboard's text. Uses its own
/// Mutter session, separate from any shared screen.
pub async fn clipboard_probe(
    set: Option<&str>,
    hold: std::time::Duration,
) -> Result<Option<String>, String> {
    use futures_util::StreamExt;
    let session = mutter::Session::start(None, "")
        .await
        .map_err(|e| e.to_string())?;
    session
        .enable_clipboard()
        .await
        .map_err(|e| e.to_string())?;
    let (mut owner, mut transfer) = session
        .clipboard_signals()
        .await
        .map_err(|e| e.to_string())?;
    let result = if let Some(text) = set {
        session
            .set_selection(&["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"])
            .await
            .map_err(|e| e.to_string())?;
        let deadline = tokio::time::sleep(hold);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                Some(message) = transfer.next() => {
                    if let Ok((_mime, serial)) = message.body().deserialize::<(String, u32)>() {
                        let ok = match session.selection_write(serial).await {
                            Ok(fd) => pipe::write_all(fd, text.as_bytes().to_vec()).await,
                            Err(_) => false,
                        };
                        let _ = session.selection_write_done(serial, ok).await;
                    }
                }
            }
        }
        Ok(None)
    } else {
        // Mutter reports the current owner right after the clipboard is enabled.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), owner.next()).await;
        match session.selection_read("text/plain;charset=utf-8").await {
            Ok(fd) => {
                let bytes = pipe::read_all(fd, casement_proto::MAX_CLIPBOARD_TEXT)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            }
            Err(e) => Err(e.to_string()),
        }
    };
    session.stop().await;
    result
}

#[cfg(target_os = "linux")]
/// Decodes an H.264 Annex B file through `filesrc ! h264parse ! <tail>` as fast as it goes;
/// returns (frames, wall seconds, CPU seconds of this process). For comparing decoders.
pub fn decode_bench(path: &std::path::Path, tail: &str) -> Result<(u64, f64, f64), String> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    gst::init().map_err(|e| e.to_string())?;
    let description = format!(
        "filesrc location=\"{}\" ! h264parse ! {tail} ! fakesink name=sink sync=false signal-handoffs=true",
        path.display()
    );
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| e.to_string())?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline".to_string())?;
    let frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter = frames.clone();
    pipeline
        .by_name("sink")
        .ok_or("no sink")?
        .connect("handoff", false, move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        });
    let cpu = || -> f64 {
        // utime + stime in clock ticks (fields 14 and 15 of /proc/self/stat).
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let after = stat.rsplit_once(')').map(|(_, rest)| rest).unwrap_or("");
        let fields: Vec<&str> = after.split_whitespace().collect();
        let tick = |i: usize| {
            fields
                .get(i)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        (tick(11) + tick(12)) / 100.0
    };
    let (start, cpu_start) = (std::time::Instant::now(), cpu());
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let result = match bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(120),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    ) {
        Some(message) => match message.view() {
            gst::MessageView::Error(e) => Err(e.error().to_string()),
            _ => Ok(()),
        },
        None => Err("timed out".into()),
    };
    let (wall, used) = (start.elapsed().as_secs_f64(), cpu() - cpu_start);
    let _ = pipeline.set_state(gst::State::Null);
    result.map(|()| {
        (
            frames.load(std::sync::atomic::Ordering::Relaxed),
            wall,
            used,
        )
    })
}

/// Points GStreamer at the plugins shipped next to the program (`bin\..\lib\gstreamer-1.0`,
/// the layout of `scripts/bundle-windows`): the mingw build looks only at the path it was
/// built with. Call first thing in `main`; does nothing elsewhere or when already set.
pub fn use_bundled_gstreamer() {
    #[cfg(windows)]
    {
        let Some(root) = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.parent()?.to_path_buf()))
        else {
            return;
        };
        let plugins = root.join("lib").join("gstreamer-1.0");
        if !plugins.is_dir() || std::env::var_os("GST_PLUGIN_SYSTEM_PATH_1_0").is_some() {
            return;
        }
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
        let scanner = root
            .join("libexec")
            .join("gstreamer-1.0")
            .join("gst-plugin-scanner.exe");
        if scanner.is_file() {
            std::env::set_var("GST_PLUGIN_SCANNER_1_0", scanner);
        }
        // Our own plugin cache: a system-wide GStreamer must not share it.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let dir = std::path::Path::new(&local).join("casement");
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var("GST_REGISTRY_1_0", dir.join("gstreamer-registry.bin"));
        }
    }
}
