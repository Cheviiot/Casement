//! Casement wire messages (protobuf through prost; the schema is this file).
//!
//! The control stream carries `Control` envelopes, each framed by the transport with a
//! 32-bit big-endian length. Tags are part of the wire format: never reuse or renumber
//! them, only add new ones.

pub use prost::Message;

/// Version of the protocol in this file. Peers with another major version refuse politely.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest control frame either side accepts (room for 1 MiB of clipboard text).
pub const MAX_CONTROL_FRAME: usize = 2 * 1024 * 1024;
/// Clipboard text beyond this is cut (at a character boundary).
pub const MAX_CLIPBOARD_TEXT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ErrorCode {
    Unspecified = 0,
    VersionMismatch = 1,
    AuthFailed = 2,
    RateLimited = 3,
    NotAllowed = 4,
    NoPasswordSet = 5,
    Busy = 6,
    Protocol = 7,
    Internal = 8,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ClientHello {
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(string, tag = "3")]
    pub os: String,
    /// Ask the host to remember this device after a successful password login.
    #[prost(bool, tag = "4")]
    pub remember: bool,
}

/// Argon2id settings the client must use to turn the password into the PAKE secret.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PasswordParams {
    #[prost(bytes = "vec", tag = "1")]
    pub salt: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub memory_kib: u32,
    #[prost(uint32, tag = "3")]
    pub iterations: u32,
    #[prost(uint32, tag = "4")]
    pub parallelism: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ServerHello {
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(string, tag = "3")]
    pub os: String,
    /// Absent when this client is already trusted and logged in.
    #[prost(message, optional, tag = "4")]
    pub password: Option<PasswordParams>,
    /// Keys, buttons and scrolling may use a separate ordered input stream.
    #[prost(bool, tag = "5")]
    pub reliable_input: bool,
}

/// One SPAKE2 message.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PakeStep {
    #[prost(bytes = "vec", tag = "1")]
    pub message: Vec<u8>,
}

/// Key confirmation: HMAC of the TLS exporter with the PAKE key.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PakeConfirm {
    #[prost(bytes = "vec", tag = "1")]
    pub mac: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct LoginResult {
    #[prost(bool, tag = "1")]
    pub ok: bool,
    /// Whether the host stored this device as trusted.
    #[prost(bool, tag = "2")]
    pub remembered: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Failure {
    #[prost(enumeration = "ErrorCode", tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
    /// For RateLimited: seconds before the next attempt is accepted.
    #[prost(uint32, tag = "3")]
    pub retry_after: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Ping {
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Pong {
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
}

/// Client → host after login: send the screen, at most this large (0 = any).
#[derive(Clone, PartialEq, prost::Message)]
pub struct StartScreen {
    #[prost(uint32, tag = "1")]
    pub max_width: u32,
    #[prost(uint32, tag = "2")]
    pub max_height: u32,
    /// Instead of mirroring the host's monitor, give the client a virtual monitor of
    /// exactly max_width x max_height (GNOME: like gnome-remote-desktop's extend mode).
    #[prost(bool, tag = "3")]
    pub virtual_monitor: bool,
    /// Which monitor to show (`Monitor.id` from an earlier ScreenStarted); "" = primary.
    #[prost(string, tag = "4")]
    pub monitor: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Monitor {
    /// Stable within the host's session, e.g. the connector "HDMI-1".
    #[prost(string, tag = "1")]
    pub id: String,
    /// For people, e.g. "Dell 27\"".
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(uint32, tag = "3")]
    pub width: u32,
    #[prost(uint32, tag = "4")]
    pub height: u32,
    #[prost(bool, tag = "5")]
    pub primary: bool,
}

/// Host → client: video follows on a unidirectional stream.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ScreenStarted {
    #[prost(uint32, tag = "1")]
    pub width: u32,
    #[prost(uint32, tag = "2")]
    pub height: u32,
    /// "h264": Annex B byte stream, one access unit per frame, no B-frames.
    #[prost(string, tag = "3")]
    pub codec: String,
    /// The host's monitors (may be empty when it cannot tell).
    #[prost(message, repeated, tag = "4")]
    pub monitors: Vec<Monitor>,
    /// The one shown (`Monitor.id`); "" for a virtual monitor or when unknown.
    #[prost(string, tag = "5")]
    pub monitor: String,
    /// True when the cursor is already painted into the video. Optional so new
    /// clients remain compatible with hosts from before this field existed.
    #[prost(bool, optional, tag = "6")]
    pub cursor_embedded: Option<bool>,
}

/// Client → host: the decoder lost track; send a keyframe soon.
#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestKeyframe {}

/// Either direction: the sender's clipboard now holds this text.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ClipboardText {
    #[prost(string, tag = "1")]
    pub text: String,
}

/// First message on a file stream (client → host): the raw bytes follow after the reply.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FileOffer {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(uint64, tag = "2")]
    pub size: u64,
}

/// Host → client on a file stream: whether to send (after the offer) and whether the file
/// was kept (after FileDone); `message` names the saved file or says what went wrong.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FileReply {
    #[prost(bool, tag = "1")]
    pub accepted: bool,
    #[prost(string, tag = "2")]
    pub message: String,
}

/// After the bytes: SHA-256 of the whole file, checked by the host before keeping it.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FileDone {
    #[prost(bytes = "vec", tag = "1")]
    pub sha256: Vec<u8>,
}

/// Client → host, first message on a file stream: list this folder of the host's shared
/// folder. `path` is relative, components separated by '/', "" for the shared folder.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FileList {
    #[prost(string, tag = "1")]
    pub path: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FileEntry {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(bool, tag = "2")]
    pub is_dir: bool,
    #[prost(uint64, tag = "3")]
    pub size: u64,
    /// Seconds since the Unix epoch, 0 when unknown.
    #[prost(uint64, tag = "4")]
    pub modified: u64,
}

/// Host → client: the folder's entries; `truncated` when there were more than fit.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FileListing {
    #[prost(string, tag = "1")]
    pub path: String,
    #[prost(message, repeated, tag = "2")]
    pub entries: Vec<FileEntry>,
    #[prost(bool, tag = "3")]
    pub truncated: bool,
}

/// Client → host, first message on a file stream: send me this file (path as in
/// FileList). The host answers FileOffer, the bytes, then FileDone — or Failure.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FileFetch {
    #[prost(string, tag = "1")]
    pub path: String,
}

/// Client → host: send the sound this computer plays. The host answers AudioStarted
/// or Failure; packets then come as datagrams (see AUDIO_DATAGRAM).
#[derive(Clone, PartialEq, prost::Message)]
pub struct StartAudio {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AudioStarted {
    /// "opus": one Opus packet per datagram.
    #[prost(string, tag = "1")]
    pub codec: String,
    #[prost(uint32, tag = "2")]
    pub rate: u32,
    #[prost(uint32, tag = "3")]
    pub channels: u32,
}

/// Client → host: the window changed; resize the virtual monitor (ignored when mirroring).
#[derive(Clone, PartialEq, prost::Message)]
pub struct ResizeScreen {
    #[prost(uint32, tag = "1")]
    pub width: u32,
    #[prost(uint32, tag = "2")]
    pub height: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Control {
    #[prost(
        oneof = "control::Kind",
        tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22"
    )]
    pub kind: Option<control::Kind>,
}

pub mod control {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "1")]
        ClientHello(super::ClientHello),
        #[prost(message, tag = "2")]
        ServerHello(super::ServerHello),
        #[prost(message, tag = "3")]
        PakeStep(super::PakeStep),
        #[prost(message, tag = "4")]
        PakeConfirm(super::PakeConfirm),
        #[prost(message, tag = "5")]
        LoginResult(super::LoginResult),
        #[prost(message, tag = "6")]
        Failure(super::Failure),
        #[prost(message, tag = "7")]
        Ping(super::Ping),
        #[prost(message, tag = "8")]
        Pong(super::Pong),
        #[prost(message, tag = "9")]
        StartScreen(super::StartScreen),
        #[prost(message, tag = "10")]
        ScreenStarted(super::ScreenStarted),
        #[prost(message, tag = "11")]
        RequestKeyframe(super::RequestKeyframe),
        #[prost(message, tag = "12")]
        ClipboardText(super::ClipboardText),
        #[prost(message, tag = "13")]
        ResizeScreen(super::ResizeScreen),
        #[prost(message, tag = "14")]
        FileOffer(super::FileOffer),
        #[prost(message, tag = "15")]
        FileReply(super::FileReply),
        #[prost(message, tag = "16")]
        FileDone(super::FileDone),
        #[prost(message, tag = "17")]
        FileList(super::FileList),
        #[prost(message, tag = "18")]
        FileListing(super::FileListing),
        #[prost(message, tag = "19")]
        FileFetch(super::FileFetch),
        #[prost(message, tag = "20")]
        StartAudio(super::StartAudio),
        #[prost(message, tag = "21")]
        AudioStarted(super::AudioStarted),
        #[prost(message, tag = "22")]
        ReliableInput(super::ReliableInput),
    }
}

/// Header of every frame on the video stream, before `len` bytes of codec data:
/// u32 length (BE) of everything after it, u64 presentation time in µs (BE), u8 flags.
pub const FRAME_HEADER_LEN: usize = 4 + 8 + 1;
pub const FRAME_FLAG_KEYFRAME: u8 = 1;
/// Host → client datagrams start with this byte, then u64 pts in µs (BE), then one
/// packet. Client → host datagrams are `Input` messages.
pub const AUDIO_DATAGRAM: u8 = 1;
pub const AUDIO_HEADER_LEN: usize = 1 + 8;

/// Client → host motion datagram. This byte is not a valid protobuf field key, so
/// legacy Input datagrams and audio cannot be confused with the new envelope.
pub const MOTION_DATAGRAM: u8 = 2;

#[derive(Clone, PartialEq, prost::Message)]
pub struct SequencedMotion {
    #[prost(message, optional, tag = "1")]
    pub position: Option<PointerMotion>,
    #[prost(uint64, tag = "2")]
    pub sequence: u64,
}

/// Sent only after reliable_input was advertised, on a dedicated client → host
/// unidirectional stream. Position binds clicks/scrolling to their original target
/// even when newer motion datagrams overtake the reliable command.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ReliableInput {
    #[prost(message, optional, tag = "1")]
    pub input: Option<Input>,
    #[prost(message, optional, tag = "2")]
    pub position: Option<PointerMotion>,
    #[prost(uint64, tag = "3")]
    pub motion_sequence: u64,
}

/// Largest encoded frame accepted (a 4K keyframe at high quality stays well below).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Input from the client. Legacy peers use datagrams; capable peers deliver
/// transitions reliably and send latest-wins motion separately.
/// Coordinates are pixels of the screen as announced in `ScreenStarted`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Input {
    #[prost(oneof = "input::Kind", tags = "1, 2, 3, 4")]
    pub kind: Option<input::Kind>,
}

pub mod input {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "1")]
        Motion(super::PointerMotion),
        #[prost(message, tag = "2")]
        Button(super::PointerButton),
        #[prost(message, tag = "3")]
        Scroll(super::Scroll),
        #[prost(message, tag = "4")]
        Key(super::Key),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PointerMotion {
    #[prost(double, tag = "1")]
    pub x: f64,
    #[prost(double, tag = "2")]
    pub y: f64,
}

/// `button` is a Linux evdev code (BTN_LEFT = 0x110, BTN_RIGHT = 0x111, BTN_MIDDLE = 0x112).
#[derive(Clone, PartialEq, prost::Message)]
pub struct PointerButton {
    #[prost(uint32, tag = "1")]
    pub button: u32,
    #[prost(bool, tag = "2")]
    pub pressed: bool,
}

/// Smooth scrolling in "lines" (one wheel notch = 1.0); positive dy scrolls down.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Scroll {
    #[prost(double, tag = "1")]
    pub dx: f64,
    #[prost(double, tag = "2")]
    pub dy: f64,
}

/// `code` is a Linux evdev key code (KEY_A = 30): physical keys, layout-independent.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Key {
    #[prost(uint32, tag = "1")]
    pub code: u32,
    #[prost(bool, tag = "2")]
    pub pressed: bool,
}

impl Control {
    pub fn new(kind: control::Kind) -> Self {
        Self { kind: Some(kind) }
    }

    pub fn failure(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(control::Kind::Failure(Failure {
            code: code as i32,
            message: message.into(),
            retry_after: 0,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let msg = Control::new(control::Kind::ServerHello(ServerHello {
            version: PROTOCOL_VERSION,
            name: "gem12".into(),
            os: "linux".into(),
            password: Some(PasswordParams {
                salt: vec![1; 16],
                memory_kib: 65536,
                iterations: 3,
                parallelism: 1,
            }),
            reliable_input: true,
        }));
        let bytes = msg.encode_to_vec();
        assert_eq!(Control::decode(bytes.as_slice()).unwrap(), msg);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A newer peer may add fields; tag 1000 (varint) must not break decoding.
        let mut bytes = Control::new(control::Kind::Ping(Ping { nonce: 7 })).encode_to_vec();
        bytes.extend_from_slice(&[0xC0, 0x3E, 0x01]);
        let decoded = Control::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.kind, Some(control::Kind::Ping(Ping { nonce: 7 })));
    }

    #[test]
    fn reliable_input_capability_is_optional_for_legacy_greetings() {
        #[derive(Clone, PartialEq, prost::Message)]
        struct LegacyHello {
            #[prost(uint32, tag = "1")]
            version: u32,
            #[prost(string, tag = "2")]
            name: String,
            #[prost(string, tag = "3")]
            os: String,
            #[prost(message, optional, tag = "4")]
            password: Option<PasswordParams>,
        }
        let legacy = LegacyHello {
            version: PROTOCOL_VERSION,
            name: "host".into(),
            os: "linux".into(),
            password: None,
        };
        let mut current = ServerHello::decode(legacy.encode_to_vec().as_slice()).unwrap();
        assert!(!current.reliable_input);
        current.reliable_input = true;
        assert_eq!(
            LegacyHello::decode(current.encode_to_vec().as_slice()).unwrap(),
            legacy
        );
    }

    #[test]
    fn failure_code_survives() {
        let bytes = Control::failure(ErrorCode::RateLimited, "wait").encode_to_vec();
        match Control::decode(bytes.as_slice()).unwrap().kind {
            Some(control::Kind::Failure(f)) => {
                assert_eq!(f.code(), ErrorCode::RateLimited);
                assert_eq!(f.message, "wait");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
