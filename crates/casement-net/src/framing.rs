//! Control messages on a QUIC stream: a 32-bit big-endian length, then the protobuf bytes.

use casement_proto::{Control, Message, MAX_CONTROL_FRAME};

use crate::NetError;

pub async fn write(stream: &mut quinn::SendStream, message: &Control) -> Result<(), NetError> {
    let body = message.encode_to_vec();
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    stream
        .write_all(&frame)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))
}

/// The next message, or None when the peer finished the stream cleanly.
pub async fn read(stream: &mut quinn::RecvStream) -> Result<Option<Control>, NetError> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        // The peer closed the whole connection on purpose: the same as finishing the stream.
        Err(quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(
            quinn::ConnectionError::ApplicationClosed(_)
            | quinn::ConnectionError::LocallyClosed
            | quinn::ConnectionError::TimedOut,
        ))) => return Ok(None),
        Err(e) => return Err(NetError::Stream(e.to_string())),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_CONTROL_FRAME {
        return Err(NetError::Protocol(format!("control frame of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))?;
    Control::decode(body.as_slice())
        .map(Some)
        .map_err(|e| NetError::Protocol(format!("undecodable message: {e}")))
}

/// Like `read`, but a closed stream is an error: used where an answer is required.
pub async fn expect(stream: &mut quinn::RecvStream) -> Result<Control, NetError> {
    read(stream)
        .await?
        .ok_or_else(|| NetError::Protocol("the peer closed the stream".into()))
}
