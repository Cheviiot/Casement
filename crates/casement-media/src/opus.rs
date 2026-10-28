//! Opus without GStreamer's opusenc/opusdec (missing from Fedora's mingw GStreamer): a
//! minimal binding to libopus for Windows. The host encodes 20 ms frames of interleaved
//! f32 PCM; the application decodes packets back to it.
#![cfg_attr(not(windows), allow(dead_code))]

/// Cuts a stream of interleaved samples into whole frames.
pub(crate) struct Frames {
    pending: Vec<f32>,
    frame: usize,
}

impl Frames {
    /// `frame` samples per frame (all channels together).
    pub(crate) fn new(frame: usize) -> Self {
        Self {
            pending: Vec::with_capacity(frame * 2),
            frame,
        }
    }

    pub(crate) fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / self.frame * self.frame;
        let out = self.pending[..whole]
            .chunks_exact(self.frame)
            .map(<[f32]>::to_vec)
            .collect();
        self.pending.drain(..whole);
        out
    }
}

#[cfg(windows)]
pub use encoder::Decoder;
#[cfg(windows)]
pub(crate) use encoder::Encoder;

#[cfg(windows)]
mod encoder {
    use std::ffi::c_void;

    const OPUS_APPLICATION_AUDIO: i32 = 2049;
    const OPUS_SET_BITRATE_REQUEST: i32 = 4002;
    /// Plenty for one 20 ms packet at our bitrate.
    const MAX_PACKET: usize = 4000;

    #[link(name = "opus")]
    extern "C" {
        fn opus_encoder_create(
            fs: i32,
            channels: i32,
            application: i32,
            error: *mut i32,
        ) -> *mut c_void;
        fn opus_encode_float(
            st: *mut c_void,
            pcm: *const f32,
            frame_size: i32,
            data: *mut u8,
            max_data_bytes: i32,
        ) -> i32;
        fn opus_encoder_ctl(st: *mut c_void, request: i32, ...) -> i32;
        fn opus_encoder_destroy(st: *mut c_void);
        fn opus_decoder_create(fs: i32, channels: i32, error: *mut i32) -> *mut c_void;
        fn opus_decode_float(
            st: *mut c_void,
            data: *const u8,
            len: i32,
            pcm: *mut f32,
            frame_size: i32,
            decode_fec: i32,
        ) -> i32;
        fn opus_decoder_destroy(st: *mut c_void);
    }

    /// The longest Opus frame is 120 ms.
    const MAX_FRAME_MS: usize = 120;

    pub struct Decoder {
        state: *mut c_void,
        rate: usize,
        channels: usize,
    }

    // As with the encoder: no thread affinity, one user at a time.
    unsafe impl Send for Decoder {}

    impl Decoder {
        pub fn new(rate: u32, channels: u32) -> Result<Self, String> {
            let mut error = 0;
            let state = unsafe { opus_decoder_create(rate as i32, channels as i32, &mut error) };
            if state.is_null() || error != 0 {
                return Err(format!("Opus decoder: error {error}"));
            }
            Ok(Self {
                state,
                rate: rate as usize,
                channels: channels as usize,
            })
        }

        /// One packet to interleaved samples; an empty packet conceals a lost one.
        pub fn decode(&mut self, packet: &[u8]) -> Vec<f32> {
            let max = self.rate * MAX_FRAME_MS / 1000;
            let mut pcm = vec![0f32; max * self.channels];
            let (data, len) = if packet.is_empty() {
                (std::ptr::null(), 0)
            } else {
                (packet.as_ptr(), packet.len() as i32)
            };
            let frames = unsafe {
                opus_decode_float(self.state, data, len, pcm.as_mut_ptr(), max as i32, 0)
            };
            pcm.truncate(frames.max(0) as usize * self.channels);
            pcm
        }
    }

    impl Drop for Decoder {
        fn drop(&mut self) {
            unsafe { opus_decoder_destroy(self.state) };
        }
    }

    pub(crate) struct Encoder {
        state: *mut c_void,
        channels: usize,
    }

    // libopus encoder state has no thread affinity; one thread uses it at a time.
    unsafe impl Send for Encoder {}

    impl Encoder {
        pub(crate) fn new(rate: u32, channels: u32, bitrate: i32) -> Result<Self, String> {
            let mut error = 0;
            let state = unsafe {
                opus_encoder_create(
                    rate as i32,
                    channels as i32,
                    OPUS_APPLICATION_AUDIO,
                    &mut error,
                )
            };
            if state.is_null() || error != 0 {
                return Err(format!("Opus encoder: error {error}"));
            }
            unsafe { opus_encoder_ctl(state, OPUS_SET_BITRATE_REQUEST, bitrate) };
            Ok(Self {
                state,
                channels: channels as usize,
            })
        }

        /// One frame of interleaved samples to one packet.
        pub(crate) fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
            let mut out = vec![0u8; MAX_PACKET];
            let len = unsafe {
                opus_encode_float(
                    self.state,
                    pcm.as_ptr(),
                    (pcm.len() / self.channels) as i32,
                    out.as_mut_ptr(),
                    out.len() as i32,
                )
            };
            (len > 0).then(|| {
                out.truncate(len as usize);
                out
            })
        }
    }

    impl Drop for Encoder {
        fn drop(&mut self) {
            unsafe { opus_encoder_destroy(self.state) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_frames_only() {
        let mut frames = Frames::new(4);
        assert!(frames.push(&[1.0, 2.0, 3.0]).is_empty());
        let out = frames.push(&[4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(
            out,
            vec![vec![1.0, 2.0, 3.0, 4.0], vec![5.0, 6.0, 7.0, 8.0]]
        );
        assert_eq!(
            frames.push(&[10.0, 11.0, 12.0]),
            vec![vec![9.0, 10.0, 11.0, 12.0]]
        );
    }
}
