//! The controlled side: accepts QUIC connections, runs the login and keeps sessions.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use casement_proto::{
    control::Kind, Control, ErrorCode, LoginResult, PakeStep, Pong, ServerHello, PROTOCOL_VERSION,
};

use crate::auth::{self, FailureLimiter, StoredPassword, TrustStore};
use crate::identity::{Fingerprint, Identity};
use crate::screen::{self, ScreenControl, ScreenProvider};
use crate::{framing, tls, transport_config, NetError};
use casement_proto::{Input, Message, ScreenStarted};

/// The host's UDP socket. On an IPv6 address it also takes IPv4: Windows makes `[::]`
/// IPv6-only by default (Linux does not), which left Windows hosts deaf to IPv4 clients.
fn dual_stack_socket(addr: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// A shared screen: what it accepts, the task sending frames and the task reading input.
struct Sharing {
    control: Arc<dyn ScreenControl>,
    pump: tokio::task::JoinHandle<()>,
    input: tokio::task::JoinHandle<()>,
}

/// How long a client may take for each login step before it is dropped.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct HostConfig {
    pub name: String,
    pub os: String,
    pub bind: SocketAddr,
    /// Accept connections from public addresses too (off by default).
    pub allow_any_source: bool,
    /// The screen to share; None answers screen requests with a refusal.
    pub screen: Option<Arc<dyn ScreenProvider>>,
    /// Where files sent by clients go; None refuses files.
    pub files_dir: Option<std::path::PathBuf>,
    /// The folder clients may browse and download from; None refuses.
    pub shared_dir: Option<std::path::PathBuf>,
    /// The sound to send; None answers sound requests with a refusal.
    pub audio: Option<Arc<dyn crate::audio::AudioProvider>>,
}

/// What the host tells its owner as things happen.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    LoggedIn {
        peer: SocketAddr,
        fingerprint: Fingerprint,
        name: String,
        trusted: bool,
    },
    Refused {
        peer: SocketAddr,
        reason: String,
    },
    FileReceived {
        peer: SocketAddr,
        path: std::path::PathBuf,
        size: u64,
    },
    FileSent {
        peer: SocketAddr,
        path: std::path::PathBuf,
        size: u64,
    },
    Closed {
        peer: SocketAddr,
    },
}

struct Shared {
    identity: Identity,
    config: HostConfig,
    password: Mutex<Option<StoredPassword>>,
    trust: Mutex<TrustStore>,
    limiter: Mutex<FailureLimiter>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
}

pub struct Host {
    endpoint: quinn::Endpoint,
    shared: Arc<Shared>,
}

impl Host {
    /// Binds the UDP socket. `password` and `trust` are the host's current settings.
    pub fn bind(
        identity: Identity,
        config: HostConfig,
        password: Option<StoredPassword>,
        trust: TrustStore,
    ) -> Result<(Self, tokio::sync::mpsc::UnboundedReceiver<HostEvent>), NetError> {
        let crypto =
            quinn::crypto::rustls::QuicServerConfig::try_from(tls::server_config(&identity)?)
                .map_err(|e| NetError::Tls(e.to_string()))?;
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        server.transport_config(Arc::new(transport_config()));
        let endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server),
            dual_stack_socket(config.bind)?,
            quinn::default_runtime()
                .ok_or_else(|| NetError::Io(std::io::Error::other("no async runtime")))?,
        )?;
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            identity,
            config,
            password: Mutex::new(password),
            trust: Mutex::new(trust),
            limiter: Mutex::new(FailureLimiter::default()),
            events,
        });
        Ok((Self { endpoint, shared }, rx))
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.shared.identity.fingerprint()
    }

    pub fn set_password(&self, password: Option<StoredPassword>) {
        *self.shared.password.lock().unwrap() = password;
    }

    /// Accepts connections until `close` is called.
    pub async fn run(&self) {
        while let Some(incoming) = self.endpoint.accept().await {
            let peer = incoming.remote_address();
            if !self.shared.config.allow_any_source && !crate::policy::is_private_source(peer.ip())
            {
                incoming.refuse();
                let _ = self.shared.events.send(HostEvent::Refused {
                    peer,
                    reason: "address outside private networks".into(),
                });
                continue;
            }
            let shared = self.shared.clone();
            tokio::spawn(async move {
                let connection = match incoming.await {
                    Ok(connection) => connection,
                    Err(e) => {
                        let _ = shared.events.send(HostEvent::Refused {
                            peer,
                            reason: e.to_string(),
                        });
                        return;
                    }
                };
                if let Err(e) = serve(&shared, &connection).await {
                    let _ = shared.events.send(HostEvent::Refused {
                        peer,
                        reason: e.to_string(),
                    });
                }
                connection.close(0u32.into(), b"bye");
                let _ = shared.events.send(HostEvent::Closed { peer });
            });
        }
    }

    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"host stopped");
    }
}

fn peer_fingerprint(connection: &quinn::Connection) -> Option<Fingerprint> {
    let identity = connection.peer_identity()?;
    let certs = identity
        .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
        .ok()?;
    tls::certificate_fingerprint(certs.first()?)
}

async fn refuse(
    send: &mut quinn::SendStream,
    code: ErrorCode,
    message: &str,
    retry_after: u32,
) -> NetError {
    let mut failure = Control::failure(code, message);
    if let Some(Kind::Failure(f)) = failure.kind.as_mut() {
        f.retry_after = retry_after;
    }
    let _ = framing::write(send, &failure).await;
    let _ = send.finish();
    // Give the frame a moment to leave before the connection is closed.
    let _ = tokio::time::timeout(Duration::from_millis(500), send.stopped()).await;
    NetError::Refused {
        code,
        message: message.into(),
        retry_after,
    }
}

async fn step<T>(
    future: impl std::future::Future<Output = Result<T, NetError>>,
) -> Result<T, NetError> {
    tokio::time::timeout(STEP_TIMEOUT, future)
        .await
        .map_err(|_| NetError::Protocol("the peer did not answer in time".into()))?
}

async fn serve(shared: &Shared, connection: &quinn::Connection) -> Result<(), NetError> {
    let peer = connection.remote_address();
    let ip: IpAddr = peer.ip();
    let client = peer_fingerprint(connection)
        .ok_or_else(|| NetError::Protocol("the client presented no device key".into()))?;
    let (mut send, mut recv) = step(async { Ok(connection.accept_bi().await?) }).await?;

    let hello = match step(framing::expect(&mut recv)).await?.kind {
        Some(Kind::ClientHello(hello)) => hello,
        _ => return Err(refuse(&mut send, ErrorCode::Protocol, "expected a greeting", 0).await),
    };
    if hello.version != PROTOCOL_VERSION {
        let message = format!(
            "protocol {} is not supported, this host speaks {PROTOCOL_VERSION}",
            hello.version
        );
        return Err(refuse(&mut send, ErrorCode::VersionMismatch, &message, 0).await);
    }
    let client_name = auth::sanitize_name(&hello.name);
    let mut server_hello = ServerHello {
        version: PROTOCOL_VERSION,
        name: shared.config.name.clone(),
        os: shared.config.os.clone(),
        password: None,
    };

    let trusted = shared.trust.lock().unwrap().contains(&client);
    let mut remembered = trusted;
    if !trusted {
        let stored = shared.password.lock().unwrap().clone();
        let Some(stored) = stored else {
            return Err(refuse(
                &mut send,
                ErrorCode::NoPasswordSet,
                "no access password is set on this computer",
                0,
            )
            .await);
        };
        let wait = shared.limiter.lock().unwrap().check(ip, Instant::now());
        if let Some(wait) = wait {
            return Err(refuse(
                &mut send,
                ErrorCode::RateLimited,
                "too many failed attempts",
                wait,
            )
            .await);
        }
        // The presence of `password` tells the client to log in with OPAQUE.
        server_hello.password = Some(casement_proto::PasswordParams::default());
        framing::write(&mut send, &Control::new(Kind::ServerHello(server_hello))).await?;

        let mut exporter = [0u8; 32];
        connection
            .export_keying_material(&mut exporter, auth::EXPORTER_LABEL, b"")
            .map_err(|_| NetError::Tls("no keying material".into()))?;
        let request = match step(framing::expect(&mut recv)).await?.kind {
            Some(Kind::PakeStep(step)) => step.message,
            _ => {
                return Err(
                    refuse(&mut send, ErrorCode::Protocol, "expected a login step", 0).await,
                )
            }
        };
        let login = match auth::HostPasswordLogin::start(&stored, &request, &exporter) {
            Ok(login) => login,
            Err(_) => {
                return Err(refuse(&mut send, ErrorCode::Protocol, "malformed login step", 0).await)
            }
        };
        // Count the attempt now: a client that learns the password was wrong could otherwise
        // drop the connection before confirming and guess without limit. Success clears it.
        {
            shared.limiter.lock().unwrap().fail(ip, Instant::now());
        }
        framing::write(
            &mut send,
            &Control::new(Kind::PakeStep(PakeStep {
                message: login.response.clone(),
            })),
        )
        .await?;
        let finalization = match step(framing::expect(&mut recv)).await?.kind {
            Some(Kind::PakeConfirm(confirm)) => confirm.mac,
            _ => {
                return Err(
                    refuse(&mut send, ErrorCode::Protocol, "expected a confirmation", 0).await,
                )
            }
        };
        // An empty confirmation means the client already saw the password was wrong.
        if finalization.is_empty() || !login.finish(&finalization, &exporter) {
            return Err(refuse(&mut send, ErrorCode::AuthFailed, "wrong password", 0).await);
        }
        shared.limiter.lock().unwrap().succeed(ip);
        if hello.remember {
            let added = shared.trust.lock().unwrap().add(client, &client_name);
            remembered = added.is_ok();
        }
    } else {
        framing::write(&mut send, &Control::new(Kind::ServerHello(server_hello))).await?;
    }

    framing::write(
        &mut send,
        &Control::new(Kind::LoginResult(LoginResult {
            ok: true,
            remembered,
        })),
    )
    .await?;
    let _ = shared.events.send(HostEvent::LoggedIn {
        peer,
        fingerprint: client,
        name: client_name,
        trusted,
    });

    // Files go both ways on further bidirectional streams, each in its own task.
    let files = {
        let connection = connection.clone();
        let files = crate::transfer::HostFiles {
            incoming: shared.config.files_dir.clone(),
            shared: shared.config.shared_dir.clone(),
        };
        let events = shared.events.clone();
        tokio::spawn(async move {
            while let Ok((send, recv)) = connection.accept_bi().await {
                let (files, events) = (files.clone(), events.clone());
                tokio::spawn(async move {
                    use crate::transfer::Served;
                    match crate::transfer::serve(send, recv, &files).await {
                        Ok(Served::Received(path, size)) => {
                            let _ = events.send(HostEvent::FileReceived { peer, path, size });
                        }
                        Ok(Served::Sent(path, size)) => {
                            let _ = events.send(HostEvent::FileSent { peer, path, size });
                        }
                        Ok(Served::Listed) | Err(_) => {}
                    }
                });
            }
        })
    };

    // Session: pings, the screen on request, input as datagrams.
    let mut sharing: Option<Sharing> = None;
    // Reads happen in their own task: a read cancelled half-way inside select! would lose
    // bytes and desynchronise the stream.
    let (control_tx, mut control_rx) =
        tokio::sync::mpsc::channel::<Result<Option<Control>, NetError>>(16);
    let reader = tokio::spawn(async move {
        loop {
            let message = framing::read(&mut recv).await;
            let last = !matches!(message, Ok(Some(_)));
            if control_tx.send(message).await.is_err() || last {
                return;
            }
        }
    });
    let mut screen_events: Option<tokio::sync::mpsc::UnboundedReceiver<screen::ScreenEvent>> = None;
    let mut audio: Option<tokio::task::JoinHandle<()>> = None;
    let result = loop {
        let message = tokio::select! {
            message = control_rx.recv() => match message {
                Some(Ok(Some(message))) => message,
                Some(Ok(None)) | None => break Ok(()),
                Some(Err(e)) => break Err(e),
            },
            Some(event) = async {
                match screen_events.as_mut() {
                    Some(events) => events.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let screen::ScreenEvent::Clipboard(text) = event;
                let message = Control::new(Kind::ClipboardText(casement_proto::ClipboardText { text }));
                if let Err(e) = framing::write(&mut send, &message).await {
                    break Err(e);
                }
                continue;
            }
        };
        match message.kind {
            Some(Kind::ResizeScreen(size)) => {
                if let Some(Sharing { control, .. }) = &sharing {
                    control.resize(size.width, size.height);
                }
            }
            Some(Kind::ClipboardText(clipboard)) => {
                if let Some(Sharing { control, .. }) = &sharing {
                    control.clipboard(clipboard.text);
                }
            }
            Some(Kind::Ping(ping)) => {
                framing::write(
                    &mut send,
                    &Control::new(Kind::Pong(Pong { nonce: ping.nonce })),
                )
                .await?;
            }
            Some(Kind::RequestKeyframe(_)) => {
                if let Some(Sharing { control, .. }) = &sharing {
                    control.request_keyframe();
                }
            }
            Some(Kind::StartAudio(_)) => {
                if let Some(pump) = audio.take() {
                    pump.abort();
                }
                let started = match shared.config.audio.clone() {
                    Some(provider) => provider.start().await,
                    None => Err("this computer does not share its sound".into()),
                };
                let stream = match started {
                    Ok(stream) => stream,
                    Err(message) => {
                        framing::write(
                            &mut send,
                            &Control::failure(ErrorCode::NotAllowed, message),
                        )
                        .await?;
                        continue;
                    }
                };
                let format = stream.format.clone();
                framing::write(
                    &mut send,
                    &Control::new(Kind::AudioStarted(casement_proto::AudioStarted {
                        codec: format.codec,
                        rate: format.rate,
                        channels: format.channels,
                    })),
                )
                .await?;
                let connection = connection.clone();
                let mut packets = stream.packets;
                audio = Some(tokio::spawn(async move {
                    while let Some(packet) = packets.recv().await {
                        // More than 100 ms waiting means old sound: skip ahead to now.
                        if packets.len() > 5 {
                            continue;
                        }
                        // Too large or the peer cannot take datagrams: skip that packet.
                        if let Err(quinn::SendDatagramError::ConnectionLost(_)) =
                            connection.send_datagram(crate::audio::encode(&packet).into())
                        {
                            return;
                        }
                    }
                }));
            }
            Some(Kind::StartScreen(request)) if sharing.is_none() => {
                let Some(provider) = shared.config.screen.clone() else {
                    framing::write(
                        &mut send,
                        &Control::failure(
                            ErrorCode::NotAllowed,
                            "this computer does not share its screen",
                        ),
                    )
                    .await?;
                    continue;
                };
                let started = provider
                    .start(screen::ScreenRequest {
                        max_width: request.max_width,
                        max_height: request.max_height,
                        virtual_monitor: request.virtual_monitor,
                        monitor: request.monitor,
                    })
                    .await;
                let stream = match started {
                    Ok(stream) => stream,
                    Err(message) => {
                        framing::write(&mut send, &Control::failure(ErrorCode::Internal, message))
                            .await?;
                        continue;
                    }
                };
                framing::write(
                    &mut send,
                    &Control::new(Kind::ScreenStarted(ScreenStarted {
                        width: stream.width,
                        height: stream.height,
                        codec: stream.codec.clone(),
                        monitors: stream.monitors.clone(),
                        monitor: stream.monitor.clone(),
                    })),
                )
                .await?;
                screen_events = stream.events;
                let control: Arc<dyn ScreenControl> = Arc::from(stream.control);
                let mut frames = stream.frames;
                let mut video = connection.open_uni().await?;
                let pump = tokio::spawn(async move {
                    while let Some(frame) = frames.recv().await {
                        if screen::write_frame(&mut video, &frame).await.is_err() {
                            break;
                        }
                    }
                    let _ = video.finish();
                });
                let input_control = control.clone();
                let datagrams = connection.clone();
                let input = tokio::spawn(async move {
                    while let Ok(bytes) = datagrams.read_datagram().await {
                        if let Ok(input) = Input::decode(bytes.as_ref()) {
                            input_control.input(input);
                        }
                    }
                });
                sharing = Some(Sharing {
                    control,
                    pump,
                    input,
                });
            }
            _ => {}
        }
    };
    reader.abort();
    files.abort();
    if let Some(pump) = audio {
        pump.abort();
    }
    if let Some(Sharing { pump, input, .. }) = sharing {
        pump.abort();
        input.abort();
    }
    result
}

#[cfg(test)]
mod socket_tests {
    #[test]
    fn ipv6_socket_hears_ipv4() {
        let socket = super::dual_stack_socket("[::]:0".parse().unwrap()).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = socket.local_addr().unwrap().port();
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.send_to(b"hi", ("127.0.0.1", port)).unwrap();
        let mut buf = [0u8; 8];
        let (n, from) = socket.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi");
        assert!(from.ip().to_canonical().is_ipv4());
    }
}
