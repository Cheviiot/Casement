//! Encoded frames from an appsink to the network, on any system.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use casement_net::screen::EncodedFrame;

/// Frames in flight between the encoder and the network before we start dropping.
pub(crate) const QUEUE: usize = 3;

pub(crate) fn pull_frames(
    sink: gst_app::AppSink,
    frames: tokio::sync::mpsc::Sender<EncodedFrame>,
    overflow: Arc<AtomicBool>,
    size: tokio::sync::oneshot::Sender<(u32, u32)>,
) {
    let mut size = Some(size);
    let need_keyframe = AtomicBool::new(false);
    while let Ok(sample) = sink.pull_sample() {
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        if let Some(size) = size.take() {
            let dims = sample.caps().and_then(|caps| {
                let s = caps.structure(0)?;
                Some((
                    s.get::<i32>("width").ok()? as u32,
                    s.get::<i32>("height").ok()? as u32,
                ))
            });
            match dims {
                Some(dims) => {
                    let _ = size.send(dims);
                }
                None => return,
            }
        }
        let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
        if need_keyframe.load(Ordering::Relaxed) && !keyframe {
            continue;
        }
        let Ok(map) = buffer.map_readable() else {
            continue;
        };
        let frame = EncodedFrame {
            pts_us: buffer.pts().map(|t| t.useconds()).unwrap_or(0),
            keyframe,
            data: map.as_slice().to_vec(),
        };
        match frames.try_send(frame) {
            Ok(()) => {
                if keyframe {
                    need_keyframe.store(false, Ordering::Relaxed);
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // The network fell behind: the consumer clears stale queued frames and
                // waits for a new keyframe; skip deltas here and ask the encoder now.
                overflow.store(true, Ordering::Release);
                need_keyframe.store(true, Ordering::Relaxed);
                let event = gst_video::UpstreamForceKeyUnitEvent::builder()
                    .all_headers(true)
                    .build();
                sink.send_event(event);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
        }
    }
}
