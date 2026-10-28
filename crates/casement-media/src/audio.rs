//! The sound this computer plays, for the client: on Linux the default output's monitor
//! through PulseAudio's API (PipeWire answers it too), on Windows a WASAPI loopback of the
//! default output; encoded to Opus in 20 ms packets.

use std::sync::Arc;
use std::time::Duration;

use casement_net::audio::{AudioFormat, AudioPacket, AudioProvider, AudioStartFuture, AudioStream};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

const RATE: u32 = 48000;
const CHANNELS: u32 = 2;
/// About a second of packets; older ones are dropped when the network falls behind.
const QUEUE: usize = 50;

/// Capture sources to try in order; `CASEMENT_AUDIO_SRC` replaces them (e.g. for tests:
/// "audiotestsrc is-live=true wave=sine").
fn sources() -> Vec<String> {
    if let Ok(source) = std::env::var("CASEMENT_AUDIO_SRC") {
        return vec![source];
    }
    if cfg!(windows) {
        vec![
            "wasapi2src loopback=true low-latency=true".into(),
            "wasapisrc loopback=true low-latency=true".into(),
        ]
    } else {
        vec![
            "pulsesrc device=@DEFAULT_MONITOR@ buffer-time=40000 latency-time=10000".into(),
            "pipewiresrc stream-properties=props,stream.capture.sink=true".into(),
        ]
    }
}

#[derive(Default)]
pub struct DesktopAudio;

impl DesktopAudio {
    /// Fails when GStreamer or the Opus encoder are missing (on Windows libopus is linked
    /// in, see `opus.rs`).
    pub fn new() -> Result<Arc<Self>, String> {
        gst::init().map_err(|e| format!("GStreamer: {e}"))?;
        if !cfg!(windows) {
            gst::ElementFactory::find("opusenc").ok_or("no Opus encoder (opusenc)")?;
        }
        Ok(Arc::new(Self))
    }
}

impl AudioProvider for DesktopAudio {
    fn start(&self) -> AudioStartFuture {
        Box::pin(async move {
            let mut last_error = String::from("no sound source");
            for source in sources() {
                match start_with(&source).await {
                    Ok(stream) => return Ok(stream),
                    Err(e) => {
                        tracing::info!("sound from {source}: {e}");
                        last_error = e;
                    }
                }
            }
            Err(last_error)
        })
    }
}

async fn start_with(source: &str) -> Result<AudioStream, String> {
    // Windows: raw samples for our own encoder; elsewhere GStreamer encodes. One caps
    // filter only: two in a row are not valid launch syntax.
    let raw = format!("audio/x-raw,rate={RATE},channels={CHANNELS}");
    let encode = if cfg!(windows) {
        format!("{raw},format=F32LE,layout=interleaved")
    } else {
        format!("{raw} ! opusenc bitrate=96000 frame-size=20 audio-type=generic")
    };
    let description = format!(
        "{source} ! audioconvert ! audioresample ! {encode} \
         ! appsink name=sink sync=false max-buffers={QUEUE} drop=true"
    );
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| format!("GStreamer pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline".to_string())?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or("no appsink")?;
    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(format!("cannot start: {e}"));
    }
    let (packets_tx, packets) = tokio::sync::mpsc::channel(QUEUE);
    let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
    let puller = pipeline.clone();
    std::thread::Builder::new()
        .name("casement-sound".into())
        .spawn(move || pull_packets(puller, sink, packets_tx, first_tx))
        .map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(3), first_rx).await {
        Ok(Ok(())) => {}
        _ => {
            let _ = pipeline.set_state(gst::State::Null);
            return Err("no sound within 3 s".into());
        }
    }
    tracing::info!("sound: {source}");
    Ok(AudioStream {
        format: AudioFormat {
            codec: "opus".into(),
            rate: RATE,
            channels: CHANNELS,
        },
        packets,
    })
}

/// Runs until the client side goes away or the pipeline stops, then stops the pipeline.
fn pull_packets(
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    packets: tokio::sync::mpsc::Sender<AudioPacket>,
    first: tokio::sync::oneshot::Sender<()>,
) {
    let mut first = Some(first);
    #[cfg(windows)]
    let mut pcm = match crate::opus::Encoder::new(RATE, CHANNELS, 96000) {
        Ok(encoder) => Some((
            encoder,
            crate::opus::Frames::new((RATE / 50 * CHANNELS) as usize),
        )),
        Err(e) => {
            tracing::warn!("sound: {e}");
            None
        }
    };
    #[cfg(windows)]
    let mut next_pts: Option<u64> = None;
    while let Ok(sample) = sink.pull_sample() {
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        if let Some(first) = first.take() {
            if first.send(()).is_err() {
                break;
            }
        }
        let Ok(map) = buffer.map_readable() else {
            continue;
        };
        let pts_us = buffer.pts().map(|t| t.useconds()).unwrap_or(0);
        #[cfg(not(windows))]
        let ready = vec![AudioPacket {
            pts_us,
            data: map.as_slice().to_vec(),
        }];
        #[cfg(windows)]
        let ready = {
            let Some((encoder, frames)) = pcm.as_mut() else {
                break;
            };
            let samples: Vec<f32> = map
                .as_slice()
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let start = *next_pts.get_or_insert(pts_us);
            let mut ready = Vec::new();
            for (n, frame) in frames.push(&samples).into_iter().enumerate() {
                if let Some(data) = encoder.encode(&frame) {
                    ready.push(AudioPacket {
                        pts_us: start + n as u64 * 20_000,
                        data,
                    });
                }
            }
            next_pts = Some(start + ready.len() as u64 * 20_000);
            ready
        };
        let mut closed = false;
        for packet in ready {
            match packets.try_send(packet) {
                Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => closed = true,
            }
        }
        if closed {
            break;
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
}

/// Writes received Opus packets to an Ogg file, for checking what the host sent.
pub struct OpusFile {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
}

impl OpusFile {
    pub fn create(path: &std::path::Path, format: &AudioFormat) -> Result<Self, String> {
        gst::init().map_err(|e| format!("GStreamer: {e}"))?;
        let pipeline = gst::Pipeline::new();
        let caps = gst::Caps::builder("audio/x-opus")
            .field("rate", format.rate as i32)
            .field("channels", format.channels as i32)
            .field("channel-mapping-family", 0i32)
            .build();
        let src = gst_app::AppSrc::builder()
            .caps(&caps)
            .format(gst::Format::Time)
            .build();
        let parse = make("opusparse")?;
        let mux = make("oggmux")?;
        let sink = make("filesink")?;
        sink.set_property("location", path.to_string_lossy().as_ref());
        pipeline
            .add_many([src.upcast_ref(), &parse, &mux, &sink])
            .map_err(|e| e.to_string())?;
        gst::Element::link_many([src.upcast_ref(), &parse, &mux, &sink])
            .map_err(|e| e.to_string())?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| e.to_string())?;
        Ok(Self { pipeline, src })
    }

    pub fn push(&self, packet: &AudioPacket) {
        let mut buffer = gst::Buffer::from_slice(packet.data.clone());
        if let Some(buffer) = buffer.get_mut() {
            buffer.set_pts(gst::ClockTime::from_useconds(packet.pts_us));
            buffer.set_duration(gst::ClockTime::from_mseconds(20));
        }
        let _ = self.src.push_buffer(buffer);
    }

    /// Flushes and closes the file.
    pub fn finish(self) {
        let _ = self.src.end_of_stream();
        if let Some(bus) = self.pipeline.bus() {
            let _ = bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| format!("GStreamer element {factory} is missing"))
}
