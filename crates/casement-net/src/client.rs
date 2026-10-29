//! The controlling side: connect, look at the host's key, log in, keep the session.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use casement_proto::{
    control::Kind, ClientHello, Control, ErrorCode, PakeConfirm, PakeStep, PasswordParams, Ping,
    PROTOCOL_VERSION,
};

use crate::auth;
use crate::identity::{Fingerprint, Identity};
pub use crate::input::InputSender;
use crate::tls::{self, HostKeyVerifier, SeenKey};
use crate::{framing, transport_config, NetError};
use casement_proto::{RequestKeyframe, StartScreen};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ClientInfo {
    pub name: String,
    pub os: String,
    /// Ask the host to remember this device after a password login.
    pub remember: bool,
}

/// Connected and greeted, not logged in yet. Show `host_fingerprint` to the user when
/// it is new before calling `login`.
pub struct Handshake {
    connection: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    pub host_fingerprint: Fingerprint,
    pub host_name: String,
    pub host_os: String,
    password: Option<PasswordParams>,
    reliable_input: bool,
    _endpoint: quinn::Endpoint,
}

pub struct Session {
    connection: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    pub host_name: String,
    pub remembered: bool,
    input: InputSender,
    _endpoint: quinn::Endpoint,
}

fn refused(failure: casement_proto::Failure) -> NetError {
    let code = failure.code();
    if code == ErrorCode::AuthFailed {
        return NetError::AuthFailed;
    }
    NetError::Refused {
        code,
        message: failure.message,
        retry_after: failure.retry_after,
    }
}

/// Opens the connection and exchanges greetings. With `expected` set, a host with any
/// other key is refused with `NetError::KeyChanged` carrying the key it showed.
pub async fn connect(
    identity: &Identity,
    addr: SocketAddr,
    expected: Option<Fingerprint>,
    info: &ClientInfo,
) -> Result<Handshake, NetError> {
    connect_with_version(identity, addr, expected, info, PROTOCOL_VERSION).await
}

/// `connect` announcing another protocol version; for tests of version handling.
#[doc(hidden)]
pub async fn connect_with_version(
    identity: &Identity,
    addr: SocketAddr,
    expected: Option<Fingerprint>,
    info: &ClientInfo,
    version: u32,
) -> Result<Handshake, NetError> {
    let seen = SeenKey::default();
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls::client_config(
        identity,
        HostKeyVerifier::new(expected, seen.clone()),
    )?)
    .map_err(|e| NetError::Tls(e.to_string()))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()));

    let bind: SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    }
    .parse()
    .unwrap();
    let endpoint = quinn::Endpoint::client(bind)?;
    let connecting = endpoint.connect_with(config, addr, tls::SERVER_NAME)?;
    let connection = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Err(_) => return Err(NetError::Timeout),
        Ok(Ok(connection)) => connection,
        Ok(Err(e)) => {
            if e.to_string().contains(tls::KEY_MISMATCH) {
                if let (Some(expected), Some(seen)) = (expected, seen.get()) {
                    return Err(NetError::KeyChanged { expected, seen });
                }
            }
            return Err(e.into());
        }
    };
    let host_fingerprint = seen
        .get()
        .ok_or_else(|| NetError::Tls("the host presented no device key".into()))?;

    let (mut send, mut recv) = connection.open_bi().await?;
    framing::write(
        &mut send,
        &Control::new(Kind::ClientHello(ClientHello {
            version,
            name: info.name.clone(),
            os: info.os.clone(),
            remember: info.remember,
        })),
    )
    .await?;
    let hello = match framing::expect(&mut recv).await?.kind {
        Some(Kind::ServerHello(hello)) => hello,
        Some(Kind::Failure(failure)) => return Err(refused(failure)),
        _ => return Err(NetError::Protocol("expected the host's greeting".into())),
    };
    if hello.version != PROTOCOL_VERSION {
        return Err(NetError::Refused {
            code: ErrorCode::VersionMismatch,
            message: format!("the host speaks protocol {}", hello.version),
            retry_after: 0,
        });
    }
    Ok(Handshake {
        connection,
        send,
        recv,
        host_fingerprint,
        host_name: auth::sanitize_name(&hello.name),
        host_os: auth::sanitize_name(&hello.os),
        password: hello.password,
        reliable_input: hello.reliable_input,
        _endpoint: endpoint,
    })
}

impl Handshake {
    pub fn needs_password(&self) -> bool {
        self.password.is_some()
    }

    /// Finishes the login. `password` is required when `needs_password()`.
    pub async fn login(mut self, password: Option<&str>) -> Result<Session, NetError> {
        if self.password.take().is_some() {
            let password = password.ok_or(NetError::PasswordRequired)?.to_owned();
            let login = auth::ClientPasswordLogin::start(&password)?;
            framing::write(
                &mut self.send,
                &Control::new(Kind::PakeStep(PakeStep {
                    message: login.request.clone(),
                })),
            )
            .await?;
            let response = match framing::expect(&mut self.recv).await?.kind {
                Some(Kind::PakeStep(step)) => step.message,
                Some(Kind::Failure(failure)) => return Err(refused(failure)),
                _ => return Err(NetError::Protocol("expected a login step".into())),
            };
            let mut exporter = [0u8; 32];
            self.connection
                .export_keying_material(&mut exporter, auth::EXPORTER_LABEL, b"")
                .map_err(|_| NetError::Tls("no keying material".into()))?;
            // Argon2id runs inside; keep it off the async workers.
            let finished =
                tokio::task::spawn_blocking(move || login.finish(&password, &response, &exporter))
                    .await
                    .map_err(|_| NetError::Protocol("login stopped".into()))?;
            let finalization = match finished {
                Ok(finalization) => finalization,
                // Tell the host so it counts the attempt; then report it here.
                Err(auth::AuthError::WrongPassword) => Vec::new(),
                Err(e) => return Err(e.into()),
            };
            let wrong = finalization.is_empty();
            framing::write(
                &mut self.send,
                &Control::new(Kind::PakeConfirm(PakeConfirm { mac: finalization })),
            )
            .await?;
            if wrong {
                // Let the host's refusal arrive before the connection goes away.
                let _ = tokio::time::timeout(Duration::from_secs(1), framing::read(&mut self.recv))
                    .await;
                return Err(NetError::AuthFailed);
            }
        }
        let result = match framing::expect(&mut self.recv).await?.kind {
            Some(Kind::LoginResult(result)) if result.ok => result,
            Some(Kind::Failure(failure)) => return Err(refused(failure)),
            _ => return Err(NetError::Protocol("expected the login result".into())),
        };
        Ok(Session {
            input: InputSender::new(self.connection.clone(), self.reliable_input),
            connection: self.connection,
            send: self.send,
            recv: self.recv,
            host_name: self.host_name,
            remembered: result.remembered,
            _endpoint: self._endpoint,
        })
    }

    pub fn close(self) {
        self.connection.close(0u32.into(), b"cancelled");
    }
}

/// The host's screen: its size and the stream of encoded frames
/// (read them with `screen::read_frame`).
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub video: quinn::RecvStream,
    /// The host's monitors and the id of the one shown ("" for a virtual monitor).
    pub monitors: Vec<crate::screen::Monitor>,
    pub monitor: String,
    /// False means the client should draw its own low-latency cursor.
    pub cursor_embedded: bool,
}

impl Session {
    /// Sends a file to the host's downloads; returns the name it was saved under.
    pub async fn send_file(
        &self,
        path: &std::path::Path,
        progress: impl FnMut(u64, u64) + Send,
    ) -> Result<String, NetError> {
        crate::transfer::send(&self.connection, path, progress).await
    }

    /// The entries of `path` in the host's shared folder.
    pub async fn list_files(&self, path: &str) -> Result<casement_proto::FileListing, NetError> {
        crate::transfer::list(&self.connection, path).await
    }

    /// Downloads `path` from the host's shared folder into `dir`.
    pub async fn fetch_file(
        &self,
        path: &str,
        dir: &std::path::Path,
        progress: impl FnMut(u64, u64) + Send,
    ) -> Result<std::path::PathBuf, NetError> {
        crate::transfer::fetch(&self.connection, path, dir, progress).await
    }

    /// Asks the host to share its screen, at most `max_width`×`max_height` (0 = any).
    pub async fn start_screen(
        &mut self,
        max_width: u32,
        max_height: u32,
    ) -> Result<Screen, NetError> {
        self.start_screen_with(crate::screen::ScreenRequest {
            max_width,
            max_height,
            ..Default::default()
        })
        .await
    }

    /// Like `start_screen`, with every option of the request.
    pub async fn start_screen_with(
        &mut self,
        request: crate::screen::ScreenRequest,
    ) -> Result<Screen, NetError> {
        let crate::screen::ScreenRequest {
            max_width,
            max_height,
            virtual_monitor,
            monitor,
        } = request;
        framing::write(
            &mut self.send,
            &Control::new(Kind::StartScreen(StartScreen {
                max_width,
                max_height,
                virtual_monitor,
                monitor,
            })),
        )
        .await?;
        loop {
            match framing::expect(&mut self.recv).await?.kind {
                Some(Kind::ScreenStarted(started)) => {
                    let video = self.connection.accept_uni().await?;
                    return Ok(Screen {
                        width: started.width,
                        height: started.height,
                        codec: started.codec,
                        video,
                        monitors: started.monitors,
                        monitor: started.monitor,
                        // Before protocol field 6 every backend embedded the cursor.
                        cursor_embedded: started.cursor_embedded.unwrap_or(true),
                    });
                }
                Some(Kind::Failure(failure)) => return Err(refused(failure)),
                _ => continue,
            }
        }
    }

    pub fn input_sender(&self) -> InputSender {
        self.input.clone()
    }

    /// Asks for the host's sound; packets then arrive through `audio_receiver`.
    pub async fn start_audio(&mut self) -> Result<crate::audio::AudioFormat, NetError> {
        framing::write(
            &mut self.send,
            &Control::new(Kind::StartAudio(casement_proto::StartAudio {})),
        )
        .await?;
        loop {
            match framing::expect(&mut self.recv).await?.kind {
                Some(Kind::AudioStarted(started)) => {
                    return Ok(crate::audio::AudioFormat {
                        codec: started.codec,
                        rate: started.rate,
                        channels: started.channels,
                    })
                }
                Some(Kind::Failure(failure)) => return Err(refused(failure)),
                _ => continue,
            }
        }
    }

    pub fn audio_receiver(&self) -> crate::audio::AudioReceiver {
        crate::audio::AudioReceiver(self.connection.clone())
    }

    pub async fn request_keyframe(&mut self) -> Result<(), NetError> {
        framing::write(
            &mut self.send,
            &Control::new(Kind::RequestKeyframe(RequestKeyframe {})),
        )
        .await
    }

    /// Round-trip time of one ping on the control stream.
    pub async fn ping(&mut self) -> Result<Duration, NetError> {
        let nonce = rand_core::RngCore::next_u64(&mut rand_core::OsRng);
        let start = Instant::now();
        framing::write(&mut self.send, &Control::new(Kind::Ping(Ping { nonce }))).await?;
        loop {
            match framing::expect(&mut self.recv).await?.kind {
                Some(Kind::Pong(pong)) if pong.nonce == nonce => return Ok(start.elapsed()),
                Some(Kind::Failure(failure)) => return Err(refused(failure)),
                _ => continue,
            }
        }
    }

    /// Resolves when the connection ends, with the reason.
    pub async fn closed(&self) -> quinn::ConnectionError {
        self.connection.closed().await
    }

    pub fn close(self) {
        self.connection.close(0u32.into(), b"bye");
    }
}

/// Sends control messages after the session was split; owned by one task.
pub struct ControlSender {
    send: quinn::SendStream,
}

/// Messages the host sends during a session.
#[derive(Clone, Debug, PartialEq)]
pub enum HostMessage {
    Clipboard(String),
}

/// Reads the host's control messages; give it a task of its own (reads are not
/// cancellation-safe inside `select!`).
pub struct ControlReceiver {
    recv: quinn::RecvStream,
}

/// The connection itself after the split: input, closing, waiting for the end.
pub struct SessionHandle {
    connection: quinn::Connection,
    input: InputSender,
    _endpoint: quinn::Endpoint,
}

impl Session {
    pub fn split(self) -> (ControlSender, ControlReceiver, SessionHandle) {
        (
            ControlSender { send: self.send },
            ControlReceiver { recv: self.recv },
            SessionHandle {
                connection: self.connection,
                input: self.input,
                _endpoint: self._endpoint,
            },
        )
    }
}

/// Cuts `text` to the protocol's clipboard limit at a character boundary.
pub fn clip_text(text: &str) -> &str {
    if text.len() <= casement_proto::MAX_CLIPBOARD_TEXT {
        return text;
    }
    let mut end = casement_proto::MAX_CLIPBOARD_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl ControlSender {
    pub async fn request_keyframe(&mut self) -> Result<(), NetError> {
        framing::write(
            &mut self.send,
            &Control::new(Kind::RequestKeyframe(RequestKeyframe {})),
        )
        .await
    }

    pub async fn resize(&mut self, width: u32, height: u32) -> Result<(), NetError> {
        let message = casement_proto::ResizeScreen { width, height };
        framing::write(&mut self.send, &Control::new(Kind::ResizeScreen(message))).await
    }

    pub async fn clipboard(&mut self, text: &str) -> Result<(), NetError> {
        let message = casement_proto::ClipboardText {
            text: clip_text(text).to_string(),
        };
        framing::write(&mut self.send, &Control::new(Kind::ClipboardText(message))).await
    }
}

impl ControlReceiver {
    /// The next message, or None when the host ended the session.
    pub async fn next(&mut self) -> Result<Option<HostMessage>, NetError> {
        loop {
            match framing::read(&mut self.recv).await? {
                None => return Ok(None),
                Some(Control {
                    kind: Some(Kind::ClipboardText(clipboard)),
                }) => return Ok(Some(HostMessage::Clipboard(clipboard.text))),
                Some(_) => continue,
            }
        }
    }
}

/// Sends files on its own; cheap to clone, usable from any task.
#[derive(Clone)]
pub struct FileSender(quinn::Connection);

impl FileSender {
    pub async fn send(
        &self,
        path: &std::path::Path,
        progress: impl FnMut(u64, u64) + Send,
    ) -> Result<String, NetError> {
        crate::transfer::send(&self.0, path, progress).await
    }

    /// The entries of `path` ('/'-separated, "" for the top) in the host's shared folder.
    pub async fn list(&self, path: &str) -> Result<casement_proto::FileListing, NetError> {
        crate::transfer::list(&self.0, path).await
    }

    /// Downloads `path` from the host's shared folder into `dir`; returns where it landed.
    pub async fn fetch(
        &self,
        path: &str,
        dir: &std::path::Path,
        progress: impl FnMut(u64, u64) + Send,
    ) -> Result<std::path::PathBuf, NetError> {
        crate::transfer::fetch(&self.0, path, dir, progress).await
    }
}

impl SessionHandle {
    pub fn file_sender(&self) -> FileSender {
        FileSender(self.connection.clone())
    }

    /// Sends a file to the host's downloads; returns the name it was saved under.
    pub async fn send_file(
        &self,
        path: &std::path::Path,
        progress: impl FnMut(u64, u64) + Send,
    ) -> Result<String, NetError> {
        crate::transfer::send(&self.connection, path, progress).await
    }

    /// Closes and waits (up to a second) until the close reached the host, so a process
    /// that exits right after does not leave the host waiting for its idle timeout.
    pub async fn close_gracefully(self) {
        self.connection.close(0u32.into(), b"bye");
        let _ = tokio::time::timeout(Duration::from_secs(1), self._endpoint.wait_idle()).await;
    }

    pub fn input_sender(&self) -> InputSender {
        self.input.clone()
    }

    pub fn audio_receiver(&self) -> crate::audio::AudioReceiver {
        crate::audio::AudioReceiver(self.connection.clone())
    }

    pub async fn closed(&self) -> quinn::ConnectionError {
        self.connection.closed().await
    }

    pub fn close(self) {
        self.connection.close(0u32.into(), b"bye");
    }
}
