//! The host's sound as the network core sees it: a source of encoded packets that go to
//! the client as QUIC datagrams (late sound is useless, so nothing is resent).
//! `casement-media` captures and encodes; tests use a fake.

use casement_proto::{AUDIO_DATAGRAM, AUDIO_HEADER_LEN};

use crate::NetError;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioPacket {
    pub pts_us: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioFormat {
    pub codec: String,
    pub rate: u32,
    pub channels: u32,
}

pub struct AudioStream {
    pub format: AudioFormat,
    /// Bounded: when the network falls behind the provider drops packets.
    pub packets: tokio::sync::mpsc::Receiver<AudioPacket>,
}

pub type AudioStartFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<AudioStream, String>> + Send>>;

/// Starts capturing; stops when the returned stream is dropped.
pub trait AudioProvider: Send + Sync {
    fn start(&self) -> AudioStartFuture;
}

pub(crate) fn encode(packet: &AudioPacket) -> Vec<u8> {
    let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + packet.data.len());
    out.push(AUDIO_DATAGRAM);
    out.extend_from_slice(&packet.pts_us.to_be_bytes());
    out.extend_from_slice(&packet.data);
    out
}

pub(crate) fn decode(datagram: &[u8]) -> Option<AudioPacket> {
    if datagram.len() <= AUDIO_HEADER_LEN || datagram[0] != AUDIO_DATAGRAM {
        return None;
    }
    Some(AudioPacket {
        pts_us: u64::from_be_bytes(datagram[1..AUDIO_HEADER_LEN].try_into().ok()?),
        data: datagram[AUDIO_HEADER_LEN..].to_vec(),
    })
}

/// Receives the host's sound; cheap to clone, but only one task should read.
#[derive(Clone)]
pub struct AudioReceiver(pub(crate) quinn::Connection);

impl AudioReceiver {
    /// The next sound packet; other datagrams are skipped. Fails when the connection ends.
    pub async fn next(&self) -> Result<AudioPacket, NetError> {
        loop {
            let datagram = self.0.read_datagram().await?;
            if let Some(packet) = decode(&datagram) {
                return Ok(packet);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagrams_round_trip() {
        let packet = AudioPacket {
            pts_us: 1_234_567,
            data: vec![9, 8, 7],
        };
        assert_eq!(decode(&encode(&packet)), Some(packet));
        assert_eq!(decode(&[AUDIO_DATAGRAM, 0, 0]), None);
        assert_eq!(decode(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 5]), None);
    }
}
