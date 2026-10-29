//! PipeWire → H.264 with GStreamer, and input back to Mutter.

use std::sync::Arc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use casement_net::screen::{EncodedFrame, ScreenControl, ScreenEvent, ScreenStream};
use casement_proto::{input, Input};

use crate::frames::{pull_frames, QUEUE};
use crate::input_queue::{self, Command};
use crate::mutter;

/// Text types offered and accepted on the host's clipboard, best first.
const TEXT_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// `mime-types` arrives as `(as)` (Mutter, like the portal) or plain `as`.
fn mime_types(value: &zbus::zvariant::Value<'_>) -> Vec<String> {
    use zbus::zvariant::Value;
    match value {
        Value::Array(array) => array
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        Value::Structure(structure) => structure
            .fields()
            .first()
            .map(mime_types)
            .unwrap_or_default(),
        Value::Value(inner) => mime_types(inner),
        _ => Vec::new(),
    }
}

/// Mirrors the host's clipboard: text copied on the host goes to the client, and a
/// paste on the host of what the client copied is answered from `clipboard`.
async fn clipboard_loop(
    session: Arc<mutter::Session>,
    clipboard: Arc<std::sync::Mutex<String>>,
    events: tokio::sync::mpsc::UnboundedSender<ScreenEvent>,
) {
    use futures_util::StreamExt;
    use std::collections::HashMap;
    use zbus::zvariant::OwnedValue;

    if let Err(e) = session.enable_clipboard().await {
        tracing::warn!("clipboard unavailable: {e}");
        return;
    }
    let Ok((mut owner, mut transfer)) = session.clipboard_signals().await else {
        return;
    };
    let mut last_sent: Option<String> = None;
    loop {
        tokio::select! {
            Some(message) = owner.next() => {
                let Ok((options,)) = message.body().deserialize::<(HashMap<String, OwnedValue>,)>() else {
                    tracing::warn!("clipboard: unexpected owner signal {:?}", message.body().signature());
                    continue;
                };
                tracing::debug!("clipboard owner changed: {options:?}");
                let ours = options.get("session-is-owner").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
                if ours {
                    continue;
                }
                let offered = options.get("mime-types").map(|v| mime_types(v)).unwrap_or_default();
                let Some(mime) = TEXT_TYPES.iter().find(|t| offered.iter().any(|o| o == *t)) else { continue };
                let Ok(fd) = session.selection_read(mime).await else { continue };
                if let Ok(bytes) = crate::pipe::read_all(fd, casement_proto::MAX_CLIPBOARD_TEXT).await {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    // Mutter announces one copy more than once; send each text once.
                    if text.is_empty() || last_sent.as_deref() == Some(text.as_str()) {
                        continue;
                    }
                    last_sent = Some(text.clone());
                    if events.send(ScreenEvent::Clipboard(text)).is_err() {
                        return;
                    }
                }
            }
            Some(message) = transfer.next() => {
                let Ok((_mime, serial)) = message.body().deserialize::<(String, u32)>() else { continue };
                let text = clipboard.lock().unwrap().clone();
                let ok = match session.selection_write(serial).await {
                    Ok(fd) => crate::pipe::write_all(fd, text.into_bytes()).await,
                    Err(_) => false,
                };
                let _ = session.selection_write_done(serial, ok).await;
            }
            else => return,
        }
    }
}

/// The best H.264 encoder present: VA-API hardware first, then software.
pub fn encoder_name() -> Option<&'static str> {
    ["vah264enc", "openh264enc", "x264enc"]
        .into_iter()
        .find(|name| gst::ElementFactory::find(name).is_some())
}

/// Colour conversion to NV12: on the GPU (vapostproc, keeps frames in video memory for
/// vah264enc) when possible, else on the CPU. CASEMENT_CONVERT=videoconvert|vapostproc
/// overrides it for measurements.
fn converter(encoder: &str) -> &'static str {
    let gpu = "vapostproc ! video/x-raw(memory:VAMemory),format=NV12";
    let cpu = "videoconvert ! video/x-raw,format=NV12";
    match std::env::var("CASEMENT_CONVERT").as_deref() {
        Ok("videoconvert") => cpu,
        Ok("vapostproc") => gpu,
        _ if encoder == "vah264enc" && gst::ElementFactory::find("vapostproc").is_some() => gpu,
        _ => cpu,
    }
}

/// Low latency everywhere: no B-frames, a keyframe at least every 2 s at 60 fps.
fn encoder_description(name: &str) -> String {
    match name {
        "vah264enc" => {
            "vah264enc rate-control=cbr bitrate=20000 key-int-max=120 b-frames=0 target-usage=6"
                .into()
        }
        "openh264enc" => {
            "openh264enc bitrate=12000000 rate-control=bitrate gop-size=120 complexity=low".into()
        }
        _ => "x264enc tune=zerolatency speed-preset=ultrafast bitrate=12000 key-int-max=120".into(),
    }
}

struct Control {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    input: input_queue::Sender,
    clipboard_task: tokio::task::AbortHandle,
}

impl ScreenControl for Control {
    fn request_keyframe(&self) {
        let event = gst_video::UpstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        self.sink.send_event(event);
    }

    fn input(&self, input: Input) {
        self.input.send(Command::Input(input));
    }

    fn clipboard(&self, text: String) {
        self.input.send(Command::Clipboard(text));
    }

    fn resize(&self, width: u32, height: u32) {
        // Only a virtual monitor has the size filter; PipeWire renegotiates with Mutter and
        // the encoder restarts with a keyframe at the new size.
        let Some(filter) = self.pipeline.by_name("size") else {
            return;
        };
        let (width, height) = virtual_size(width, height);
        if let Ok(caps) =
            format!("video/x-raw(ANY),width={width},height={height}").parse::<gst::Caps>()
        {
            filter.set_property("caps", &caps);
            self.request_keyframe();
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.input.close();
        let _ = self.pipeline.set_state(gst::State::Null);
        self.clipboard_task.abort();
    }
}

/// Applies input in order; ends the Mutter session when the sender goes away.
async fn input_loop(
    session: Arc<mutter::Session>,
    mut rx: input_queue::Receiver,
    clipboard: Arc<std::sync::Mutex<String>>,
) {
    let (mut scroll_x, mut scroll_y) = (0.0f64, 0.0f64);
    while let Some(command) = rx.recv().await {
        let result = rx
            .until_closed(tokio::time::timeout(Duration::from_secs(1), async {
                match command {
                    Command::Clipboard(text) => {
                        *clipboard.lock().unwrap() = text;
                        session
                            .set_selection(TEXT_TYPES)
                            .await
                            .map_err(|e| format!("clipboard: {e}"))
                    }
                    Command::Input(event) => {
                        let result = match event.kind {
                            Some(input::Kind::Motion(m)) => session.pointer_motion(m.x, m.y).await,
                            Some(input::Kind::Button(b)) => {
                                session.pointer_button(b.button as i32, b.pressed).await
                            }
                            Some(input::Kind::Key(k)) => session.key(k.code, k.pressed).await,
                            Some(input::Kind::Scroll(s)) => {
                                // Mutter takes whole wheel steps; keep the remainder for next time.
                                scroll_x += s.dx;
                                scroll_y += s.dy;
                                let (sx, sy) = (scroll_x.trunc(), scroll_y.trunc());
                                scroll_x -= sx;
                                scroll_y -= sy;
                                let mut result = Ok(());
                                if sy != 0.0 {
                                    result = session.scroll_discrete(0, sy as i32).await;
                                }
                                if sx != 0.0 && result.is_ok() {
                                    result = session.scroll_discrete(1, sx as i32).await;
                                }
                                result
                            }
                            None => Ok(()),
                        };
                        result.map_err(|e| format!("input: {e}"))
                    }
                }
            }))
            .await;
        match result {
            None => break,
            Some(Err(_)) => {
                tracing::warn!("input: Mutter call timed out after 1 s");
                break;
            }
            Some(Ok(Err(error))) => tracing::warn!("{error}"),
            Some(Ok(Ok(()))) => {}
        }
    }
    session.stop().await;
}

/// A sensible virtual monitor for a client asking `width` x `height`: even sides within
/// what encoders take (0 = a default 1280x800).
pub fn virtual_size(width: u32, height: u32) -> (u32, u32) {
    let pick = |v: u32, default: u32| {
        if v == 0 {
            default
        } else {
            v.clamp(640, 4096) & !1
        }
    };
    (pick(width, 1280), pick(height, 800).min(2160))
}

pub async fn start(
    session: mutter::Session,
    virtual_size: Option<(u32, u32)>,
) -> Result<ScreenStream, String> {
    let session = Arc::new(session);
    let encoder = encoder_name().ok_or("no H.264 encoder")?;
    let description = format!(
        "pipewiresrc path={} do-timestamp=true keepalive-time=500 {}! {} ! {} \
         ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au \
         ! appsink name=sink sync=false max-buffers={} drop=false",
        session.node_id,
        // A virtual monitor takes the size its consumer negotiates.
        virtual_size
            .map(|(w, h)| format!(
                "! capsfilter name=size caps=video/x-raw(ANY),width={w},height={h} "
            ))
            .unwrap_or_default(),
        converter(encoder),
        encoder_description(encoder),
        QUEUE
    );
    tracing::info!("capture: {description}");
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| format!("GStreamer pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline".to_string())?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or("no appsink")?;

    let (input_tx, input_rx) = input_queue::channel();
    let clipboard = Arc::new(std::sync::Mutex::new(String::new()));
    tokio::spawn(input_loop(session.clone(), input_rx, clipboard.clone()));
    let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let clipboard_task = tokio::spawn(clipboard_loop(session.clone(), clipboard, events_tx));
    let control = Control {
        pipeline: pipeline.clone(),
        sink: sink.clone(),
        input: input_tx,
        clipboard_task: clipboard_task.abort_handle(),
    };

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("cannot start capture: {e}"))?;

    let (frames_tx, frames) = tokio::sync::mpsc::channel::<EncodedFrame>(QUEUE);
    let frame_overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (size_tx, size_rx) = tokio::sync::oneshot::channel::<(u32, u32)>();
    let puller = sink.clone();
    let overflow = frame_overflow.clone();
    std::thread::Builder::new()
        .name("casement-capture".into())
        .spawn(move || pull_frames(puller, frames_tx, overflow, size_tx))
        .map_err(|e| e.to_string())?;

    let (width, height) = match tokio::time::timeout(Duration::from_secs(10), size_rx).await {
        Ok(Ok(size)) => size,
        _ => {
            return Err(format!(
                "no picture from the screen within 10 s (encoder {encoder})"
            ))
        }
    };
    tracing::info!("sharing {width}x{height} with {encoder}");
    let (monitors, monitor) = (session.monitors.clone(), session.connector.clone());
    Ok(ScreenStream {
        width,
        height,
        codec: "h264".into(),
        frames,
        frame_overflow,
        control: Box::new(control),
        events: Some(events),
        monitors,
        monitor,
    })
}
