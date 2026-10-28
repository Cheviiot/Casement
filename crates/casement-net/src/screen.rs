//! The screen as the network core sees it: a source of encoded frames plus a sink for
//! input. `casement-media` implements it with Mutter, PipeWire and GStreamer; tests use a
//! fake. The core only moves bytes.

use std::future::Future;
use std::pin::Pin;

pub use casement_proto::Monitor;
use casement_proto::{Input, FRAME_FLAG_KEYFRAME, FRAME_HEADER_LEN, MAX_FRAME};

use crate::NetError;

#[derive(Clone, Debug, PartialEq)]
pub struct EncodedFrame {
    pub pts_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// What the host's screen accepts while it is shared.
pub trait ScreenControl: Send + Sync {
    fn request_keyframe(&self);
    fn input(&self, input: Input);
    /// The client copied this text: offer it as the host's clipboard.
    fn clipboard(&self, _text: String) {}
    /// The client's window changed size; only a virtual monitor follows it.
    fn resize(&self, _width: u32, _height: u32) {}
}

/// What the shared screen tells the client besides frames.
#[derive(Clone, Debug, PartialEq)]
pub enum ScreenEvent {
    /// Text copied on the host.
    Clipboard(String),
}

pub struct ScreenStream {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    /// Bounded: when the network is slower than the encoder the provider must drop frames
    /// and continue from the next keyframe, never stall capture.
    pub frames: tokio::sync::mpsc::Receiver<EncodedFrame>,
    pub control: Box<dyn ScreenControl>,
    pub events: Option<tokio::sync::mpsc::UnboundedReceiver<ScreenEvent>>,
    /// The host's monitors and the id of the one shown ("" for a virtual monitor).
    pub monitors: Vec<Monitor>,
    pub monitor: String,
}

pub type StartFuture = Pin<Box<dyn Future<Output = Result<ScreenStream, String>> + Send>>;

/// What the client asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScreenRequest {
    pub max_width: u32,
    pub max_height: u32,
    pub virtual_monitor: bool,
    /// `Monitor.id` to show; "" for the primary one.
    pub monitor: String,
}

/// Starts sharing the screen; stops when the returned stream is dropped.
pub trait ScreenProvider: Send + Sync {
    fn start(&self, request: ScreenRequest) -> StartFuture;
}

pub(crate) async fn write_frame(
    stream: &mut quinn::SendStream,
    frame: &EncodedFrame,
) -> Result<(), NetError> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    header[..4].copy_from_slice(&((frame.data.len() + 9) as u32).to_be_bytes());
    header[4..12].copy_from_slice(&frame.pts_us.to_be_bytes());
    header[12] = if frame.keyframe {
        FRAME_FLAG_KEYFRAME
    } else {
        0
    };
    stream
        .write_all(&header)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))?;
    stream
        .write_all(&frame.data)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))
}

/// The next frame, or None when the host ended the stream.
pub async fn read_frame(stream: &mut quinn::RecvStream) -> Result<Option<EncodedFrame>, NetError> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    match stream.read_exact(&mut header).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(NetError::Stream(e.to_string())),
    }
    let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    if !(9..=MAX_FRAME + 9).contains(&len) {
        return Err(NetError::Protocol(format!("video frame of {len} bytes")));
    }
    let pts_us = u64::from_be_bytes(header[4..12].try_into().unwrap());
    let mut data = vec![0u8; len - 9];
    stream
        .read_exact(&mut data)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))?;
    Ok(Some(EncodedFrame {
        pts_us,
        keyframe: header[12] & FRAME_FLAG_KEYFRAME != 0,
        data,
    }))
}
