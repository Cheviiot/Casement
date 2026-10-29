//! Host and client in one process over loopback: every login path of stage 1.

use std::net::SocketAddr;
use std::path::Path;

use casement_proto::ErrorCode;

use crate::auth::{StoredPassword, TrustStore};
use crate::client::{self, ClientInfo};
use crate::host::{Host, HostConfig, HostEvent};
use crate::identity::{Fingerprint, Identity};
use crate::NetError;

const PASSWORD: &str = "correct horse battery";

/// A screen that sends numbered frames and records the input it gets.
#[derive(Default)]
struct FakeScreen {
    inputs: std::sync::Arc<std::sync::Mutex<Vec<casement_proto::Input>>>,
    keyframes: std::sync::Arc<std::sync::atomic::AtomicU32>,
    clipboard: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// Sound that sends numbered packets every 5 ms until the stream is dropped.
struct FakeAudio;

impl crate::audio::AudioProvider for FakeAudio {
    fn start(&self) -> crate::audio::AudioStartFuture {
        let (tx, packets) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            for n in 0u64.. {
                let packet = crate::audio::AudioPacket {
                    pts_us: n * 5000,
                    data: vec![n as u8; 200],
                };
                if tx.send(packet).await.is_err() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        });
        Box::pin(async move {
            Ok(crate::audio::AudioStream {
                format: crate::audio::AudioFormat {
                    codec: "opus".into(),
                    rate: 48000,
                    channels: 2,
                },
                packets,
            })
        })
    }
}

struct FakeControl {
    inputs: std::sync::Arc<std::sync::Mutex<Vec<casement_proto::Input>>>,
    keyframes: std::sync::Arc<std::sync::atomic::AtomicU32>,
    clipboard: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    events: tokio::sync::mpsc::UnboundedSender<crate::screen::ScreenEvent>,
}

impl crate::screen::ScreenControl for FakeControl {
    fn request_keyframe(&self) {
        self.keyframes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn input(&self, input: casement_proto::Input) {
        self.inputs.lock().unwrap().push(input);
    }
    fn clipboard(&self, text: String) {
        // Echo it back changed, as if an application on the host copied something.
        let _ = self
            .events
            .send(crate::screen::ScreenEvent::Clipboard(format!(
                "host: {text}"
            )));
        self.clipboard.lock().unwrap().push(text);
    }
}

impl crate::screen::ScreenProvider for FakeScreen {
    fn start(&self, request: crate::screen::ScreenRequest) -> crate::screen::StartFuture {
        let max_width = request.max_width;
        let monitors = vec![
            crate::screen::Monitor {
                id: "FAKE-1".into(),
                name: "Первый".into(),
                width: 1920,
                height: 1080,
                primary: true,
            },
            crate::screen::Monitor {
                id: "FAKE-2".into(),
                name: "Второй".into(),
                width: 1280,
                height: 1024,
                primary: false,
            },
        ];
        let monitor = if request.monitor.is_empty() {
            "FAKE-1".to_string()
        } else {
            request.monitor.clone()
        };
        let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let control = FakeControl {
            inputs: self.inputs.clone(),
            keyframes: self.keyframes.clone(),
            clipboard: self.clipboard.clone(),
            events: events_tx,
        };
        Box::pin(async move {
            let (tx, frames) = tokio::sync::mpsc::channel(4);
            let frame_overflow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            tokio::spawn(async move {
                for n in 0..5u64 {
                    let frame = crate::screen::EncodedFrame {
                        pts_us: n * 16_666,
                        keyframe: n == 0,
                        data: vec![n as u8; 1000 + n as usize],
                    };
                    if tx.send(frame).await.is_err() {
                        return;
                    }
                }
            });
            Ok(crate::screen::ScreenStream {
                width: max_width.min(1920),
                height: 1080,
                codec: "h264".into(),
                frames,
                frame_overflow,
                control: Box::new(control),
                events: Some(events),
                monitors,
                monitor,
            })
        })
    }
}

struct Running {
    addr: SocketAddr,
    fingerprint: Fingerprint,
    host: std::sync::Arc<Host>,
    events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
}

fn start_host(dir: &Path, password: Option<&str>) -> Running {
    let identity = Identity::load_or_create(&dir.join("host")).unwrap();
    let fingerprint = identity.fingerprint();
    let password = password.map(|p| StoredPassword::create(p).unwrap());
    let trust = TrustStore::load(&dir.join("host")).unwrap();
    let config = HostConfig {
        name: "test-host".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        allow_any_source: false,
        screen: Some(std::sync::Arc::new(FakeScreen::default())),
        files_dir: Some(dir.join("host").join("downloads")),
        shared_dir: Some(dir.join("host").join("shared")),
        audio: Some(std::sync::Arc::new(FakeAudio)),
        settings_dir: None,
    };
    let (host, events) = Host::bind(identity, config, password, trust).unwrap();
    let host = std::sync::Arc::new(host);
    let addr = host.local_addr().unwrap();
    let runner = host.clone();
    tokio::spawn(async move { runner.run().await });
    Running {
        addr,
        fingerprint,
        host,
        events,
    }
}

fn start_host_with_providers(
    dir: &Path,
    screen: Option<std::sync::Arc<dyn crate::screen::ScreenProvider>>,
    audio: Option<std::sync::Arc<dyn crate::audio::AudioProvider>>,
) -> Running {
    let identity = Identity::load_or_create(&dir.join("host")).unwrap();
    let fingerprint = identity.fingerprint();
    let trust = TrustStore::load(&dir.join("host")).unwrap();
    let config = HostConfig {
        name: "test-host".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        allow_any_source: false,
        screen,
        files_dir: None,
        shared_dir: None,
        audio,
        settings_dir: None,
    };
    let password = StoredPassword::create(PASSWORD).unwrap();
    let (host, events) = Host::bind(identity, config, Some(password), trust).unwrap();
    let host = std::sync::Arc::new(host);
    let addr = host.local_addr().unwrap();
    let runner = host.clone();
    tokio::spawn(async move { runner.run().await });
    Running {
        addr,
        fingerprint,
        host,
        events,
    }
}

struct DropProbe(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct PendingScreen {
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl crate::screen::ScreenProvider for PendingScreen {
    fn start(&self, _request: crate::screen::ScreenRequest) -> crate::screen::StartFuture {
        let started = self.started.lock().unwrap().take();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            let _probe = DropProbe(dropped);
            if let Some(started) = started {
                let _ = started.send(());
            }
            std::future::pending::<Result<crate::screen::ScreenStream, String>>().await
        })
    }
}

struct PendingAudio {
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl crate::audio::AudioProvider for PendingAudio {
    fn start(&self) -> crate::audio::AudioStartFuture {
        let started = self.started.lock().unwrap().take();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            let _probe = DropProbe(dropped);
            if let Some(started) = started {
                let _ = started.send(());
            }
            std::future::pending::<Result<crate::audio::AudioStream, String>>().await
        })
    }
}

struct DropTrackedControl(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl crate::screen::ScreenControl for DropTrackedControl {
    fn request_keyframe(&self) {}

    fn input(&self, _input: casement_proto::Input) {}
}

impl Drop for DropTrackedControl {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct EndedScreen {
    events: std::sync::Arc<
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<crate::screen::ScreenEvent>>>,
    >,
    control_dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl crate::screen::ScreenProvider for EndedScreen {
    fn start(&self, _request: crate::screen::ScreenRequest) -> crate::screen::StartFuture {
        let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        *self.events.lock().unwrap() = Some(events_tx);
        let control = Box::new(DropTrackedControl(self.control_dropped.clone()));
        Box::pin(async move {
            let (_frames_tx, frames) = tokio::sync::mpsc::channel(1);
            Ok(crate::screen::ScreenStream {
                width: 640,
                height: 480,
                codec: "h264".into(),
                frames,
                frame_overflow: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                control,
                events: Some(events),
                monitors: Vec::new(),
                monitor: String::new(),
            })
        })
    }
}

async fn wait_for_closed(events: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(event) = events.recv().await {
            if matches!(event, HostEvent::Closed { .. }) {
                return;
            }
        }
        panic!("host event stream ended before Closed");
    })
    .await
    .expect("host should report the disconnected peer promptly");
}

fn client_identity(dir: &Path, name: &str) -> Identity {
    Identity::load_or_create(&dir.join(name)).unwrap()
}

fn info(remember: bool) -> ClientInfo {
    ClientInfo {
        name: "test-client".into(),
        os: "linux".into(),
        remember,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_use_password_then_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");

    // First contact: nothing expected, the key is shown, a password is needed.
    let handshake = client::connect(&me, host.addr, None, &info(true))
        .await
        .unwrap();
    assert_eq!(handshake.host_fingerprint, host.fingerprint);
    assert_eq!(handshake.host_name, "test-host");
    assert!(handshake.needs_password());
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();
    assert!(session.remembered);
    session.ping().await.unwrap();
    match host.events.recv().await.unwrap() {
        HostEvent::LoggedIn {
            fingerprint,
            trusted,
            name,
            ..
        } => {
            assert_eq!(fingerprint, me.fingerprint());
            assert!(!trusted);
            assert_eq!(name, "test-client");
        }
        other => panic!("unexpected {other:?}"),
    }
    session.close();

    // Second time: the host knows this device, no password at all.
    let handshake = client::connect(&me, host.addr, Some(host.fingerprint), &info(true))
        .await
        .unwrap();
    assert!(!handshake.needs_password());
    let mut session = handshake.login(None).await.unwrap();
    session.ping().await.unwrap();
    session.close();
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screen_frames_and_input() {
    use casement_proto::{input, Input, Key, PointerMotion};
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::load_or_create(&dir.path().join("host")).unwrap();
    let fake = std::sync::Arc::new(FakeScreen::default());
    let (inputs, keyframes, clipboard) = (
        fake.inputs.clone(),
        fake.keyframes.clone(),
        fake.clipboard.clone(),
    );
    let config = HostConfig {
        name: "screen-host".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        allow_any_source: false,
        screen: Some(fake),
        files_dir: None,
        shared_dir: None,
        audio: None,
        settings_dir: None,
    };
    let password = StoredPassword::create(PASSWORD).unwrap();
    let (host, _events) =
        Host::bind(identity, config, Some(password), TrustStore::default()).unwrap();
    let host = std::sync::Arc::new(host);
    let addr = host.local_addr().unwrap();
    let runner = host.clone();
    tokio::spawn(async move { runner.run().await });

    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, addr, None, &info(false))
        .await
        .unwrap();
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();
    let mut screen = session
        .start_screen_with(crate::screen::ScreenRequest {
            max_width: 1280,
            max_height: 720,
            monitor: "FAKE-2".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(screen.monitors.len(), 2);
    assert_eq!(
        (screen.monitors[0].name.as_str(), screen.monitors[0].primary),
        ("Первый", true)
    );
    assert_eq!(screen.monitor, "FAKE-2");
    assert_eq!(
        client::clip_text(&"ж".repeat(600_000)).len(),
        casement_proto::MAX_CLIPBOARD_TEXT
    );
    assert_eq!(
        (screen.width, screen.height, screen.codec.as_str()),
        (1280, 1080, "h264")
    );
    for n in 0..5u64 {
        let frame = crate::screen::read_frame(&mut screen.video)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.pts_us, n * 16_666);
        assert_eq!(frame.keyframe, n == 0);
        assert_eq!(frame.data.len(), 1000 + n as usize);
    }
    assert!(
        crate::screen::read_frame(&mut screen.video)
            .await
            .unwrap()
            .is_none(),
        "stream ends with the source"
    );

    let sender = session.input_sender();
    sender
        .send(&Input {
            kind: Some(input::Kind::Motion(PointerMotion { x: 10.5, y: 20.0 })),
        })
        .unwrap();
    sender
        .send(&Input {
            kind: Some(input::Kind::Key(Key {
                code: 30,
                pressed: true,
            })),
        })
        .unwrap();
    session.request_keyframe().await.unwrap();
    session.ping().await.unwrap(); // the keyframe request is handled before this answer
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while inputs.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let got = inputs.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        2,
        "motion and the reliable key arrived on loopback"
    );
    assert_eq!(keyframes.load(std::sync::atomic::Ordering::SeqCst), 1);

    // A legacy client still uses datagrams. New key/button/scroll commands share an
    // ordered reliable stream; the provider sees the original input kinds.
    sender
        .legacy()
        .send(&Input {
            kind: Some(input::Kind::Key(Key {
                code: 30,
                pressed: false,
            })),
        })
        .unwrap();
    for pressed in [true, false] {
        sender
            .send(&Input {
                kind: Some(input::Kind::Key(Key { code: 42, pressed })),
            })
            .unwrap();
        sender
            .send(&Input {
                kind: Some(input::Kind::Button(casement_proto::PointerButton {
                    button: 272,
                    pressed,
                })),
            })
            .unwrap();
    }
    sender
        .send(&Input {
            kind: Some(input::Kind::Scroll(casement_proto::Scroll {
                dx: 0.0,
                dy: 1.0,
            })),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while inputs.lock().unwrap().len() < 8 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let keys: Vec<_> = inputs
        .lock()
        .unwrap()
        .iter()
        .filter_map(|input| match &input.kind {
            Some(input::Kind::Key(key)) if key.code == 42 => Some(key.pressed),
            _ => None,
        })
        .collect();
    assert_eq!(keys, [true, false]);
    assert!(inputs.lock().unwrap().iter().any(
        |input| matches!(&input.kind, Some(input::Kind::Key(key)) if key.code == 30 && !key.pressed)
    ));

    // Clipboard both ways, through the split session.
    let (mut sender, mut receiver, handle) = session.split();
    sender.clipboard("привет").await.unwrap();
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        message,
        Some(client::HostMessage::Clipboard("host: привет".into()))
    );
    assert_eq!(clipboard.lock().unwrap().as_slice(), ["привет".to_string()]);
    let sender = handle.input_sender();
    sender
        .send(&Input {
            kind: Some(input::Kind::Key(Key {
                code: 29,
                pressed: true,
            })),
        })
        .unwrap();
    sender
        .send(&Input {
            kind: Some(input::Kind::Button(casement_proto::PointerButton {
                button: 272,
                pressed: true,
            })),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let pressed = {
                let events = inputs.lock().unwrap();
                matches!(&events[events.len()-2].kind, Some(input::Kind::Key(key)) if key.code == 29 && key.pressed)
                    && matches!(&events[events.len()-1].kind, Some(input::Kind::Button(button)) if button.button == 272 && button.pressed)
            };
            if pressed { break }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    handle.close();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let released = {
                let events = inputs.lock().unwrap();
                matches!(&events[events.len()-2].kind, Some(input::Kind::Key(key)) if key.code == 29 && !key.pressed)
                    && matches!(&events[events.len()-1].kind, Some(input::Kind::Button(button)) if !button.pressed)
            };
            if released { break }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_during_screen_start_drops_provider_and_closes_host_session() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = std::sync::Arc::new(PendingScreen {
        started: std::sync::Mutex::new(Some(started_tx)),
        dropped: dropped.clone(),
    });
    let mut host = start_host_with_providers(&dir.path().join("server"), Some(provider), None);
    let me = client_identity(&dir.path().join("client"), "identity");
    let handshake = client::connect(&me, host.addr, Some(host.fingerprint), &info(false))
        .await
        .unwrap();
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();

    let mut start = Box::pin(session.start_screen(640, 480));
    tokio::select! {
        _ = started_rx => {},
        _ = &mut start => panic!("screen startup unexpectedly completed"),
    }
    drop(start);
    session.close();

    wait_for_closed(&mut host.events).await;
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "closing the peer drops the pending screen provider future"
    );
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_during_audio_start_drops_provider_and_closes_host_session() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = std::sync::Arc::new(PendingAudio {
        started: std::sync::Mutex::new(Some(started_tx)),
        dropped: dropped.clone(),
    });
    let mut host = start_host_with_providers(&dir.path().join("server"), None, Some(provider));
    let me = client_identity(&dir.path().join("client"), "identity");
    let handshake = client::connect(&me, host.addr, Some(host.fingerprint), &info(false))
        .await
        .unwrap();
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();

    let mut start = Box::pin(session.start_audio());
    tokio::select! {
        _ = started_rx => {},
        _ = &mut start => panic!("audio startup unexpectedly completed"),
    }
    drop(start);
    session.close();

    wait_for_closed(&mut host.events).await;
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "closing the peer drops the pending audio provider future"
    );
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_screen_event_closes_connection_and_drops_backend_control() {
    let dir = tempfile::tempdir().unwrap();
    let events = std::sync::Arc::new(std::sync::Mutex::new(None));
    let control_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = std::sync::Arc::new(EndedScreen {
        events: events.clone(),
        control_dropped: control_dropped.clone(),
    });
    let mut host = start_host_with_providers(&dir.path().join("server"), Some(provider), None);
    let me = client_identity(&dir.path().join("client"), "identity");
    let handshake = client::connect(&me, host.addr, Some(host.fingerprint), &info(false))
        .await
        .unwrap();
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();
    let _screen = session.start_screen(640, 480).await.unwrap();

    events
        .lock()
        .unwrap()
        .take()
        .expect("screen provider installed its event sender")
        .send(crate::screen::ScreenEvent::Ended(
            "screen input backend failed".into(),
        ))
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), session.closed())
        .await
        .expect("backend failure should close the client connection");
    wait_for_closed(&mut host.events).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !control_dropped.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend control should be released promptly");
    host.host.close();
}

#[tokio::test]
async fn reliable_input_queue_overload_closes_instead_of_losing_transitions() {
    use casement_proto::{input, Input, Key};
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let identity = client_identity(dir.path(), "client");
    let session = client::connect(&identity, host.addr, None, &info(false))
        .await
        .unwrap()
        .login(Some(PASSWORD))
        .await
        .unwrap();
    let sender = session.input_sender();
    // This current-thread runtime cannot drain the worker during the synchronous
    // burst, making queue exhaustion deterministic without an OS input target.
    let mut stopped = false;
    for _ in 0..1024 {
        if sender
            .send(&Input {
                kind: Some(input::Kind::Key(Key {
                    code: 30,
                    pressed: true,
                })),
            })
            .is_err()
        {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "typing queue must be bounded");
    tokio::time::timeout(std::time::Duration::from_secs(1), session.closed())
        .await
        .unwrap();
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_password_then_rate_limit() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");

    for _ in 0..3 {
        let handshake = client::connect(&me, host.addr, None, &info(false))
            .await
            .unwrap();
        let err = handshake
            .login(Some("wrong password!"))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, NetError::AuthFailed), "{err:?}");
    }
    let err = client::connect(&me, host.addr, None, &info(false))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            NetError::Refused {
                code: ErrorCode::RateLimited,
                retry_after: 1..,
                ..
            }
        ),
        "{err:?}"
    );

    // Not remembered after failures: the right password still needs to be typed.
    assert!(!TrustStore::load(&dir.path().join("host"))
        .unwrap()
        .contains(&me.fingerprint()));
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_key_is_refused_before_login() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let impostor_expectation = Fingerprint([0x42; 32]);
    let err = client::connect(&me, host.addr, Some(impostor_expectation), &info(false))
        .await
        .err()
        .unwrap();
    match err {
        NetError::KeyChanged { expected, seen } => {
            assert_eq!(expected, impostor_expectation);
            assert_eq!(seen, host.fingerprint);
        }
        other => panic!("unexpected {other:?}"),
    }
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_password_means_no_strangers() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), None);
    let me = client_identity(dir.path(), "client");
    let err = client::connect(&me, host.addr, None, &info(true))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            NetError::Refused {
                code: ErrorCode::NoPasswordSet,
                ..
            }
        ),
        "{err:?}"
    );
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_password_is_reported_locally() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, host.addr, None, &info(false))
        .await
        .unwrap();
    assert!(matches!(
        handshake.login(None).await,
        Err(NetError::PasswordRequired)
    ));
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn other_protocol_version_is_refused_politely() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let err = client::connect_with_version(&me, host.addr, None, &info(false), 99)
        .await
        .err()
        .unwrap();
    match err {
        NetError::Refused {
            code: ErrorCode::VersionMismatch,
            message,
            ..
        } => {
            assert!(message.contains("99"), "{message}")
        }
        other => panic!("unexpected {other:?}"),
    }
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nobody_listening_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let me = client_identity(dir.path(), "client");
    // A bound but silent UDP socket: QUIC gets no answer.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();
    let err = client::connect(&me, addr, None, &info(false))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, NetError::Timeout | NetError::Connection(_)),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_arrive_whole_and_never_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, host.addr, None, &info(false))
        .await
        .unwrap();
    let session = handshake.login(Some(PASSWORD)).await.unwrap();

    let source = dir.path().join("отчёт.bin");
    let data: Vec<u8> = (0..3_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    std::fs::write(&source, &data).unwrap();
    let mut last = (0, 0);
    let saved = session
        .send_file(&source, |sent, total| last = (sent, total))
        .await
        .unwrap();
    assert_eq!(saved, "отчёт.bin");
    assert_eq!(last, (3_000_000, 3_000_000));
    let downloads = dir.path().join("host").join("downloads");
    assert_eq!(std::fs::read(downloads.join("отчёт.bin")).unwrap(), data);

    let again = session.send_file(&source, |_, _| {}).await.unwrap();
    assert_eq!(again, "отчёт (1).bin");
    let leftovers: Vec<_> = std::fs::read_dir(&downloads)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".casement-part"))
        .collect();
    assert!(leftovers.is_empty(), "no partial files stay behind");
    session.close();
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_uploads_with_same_name_keep_both_contents() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, host.addr, None, &info(false))
        .await
        .unwrap();
    let session = handshake.login(Some(PASSWORD)).await.unwrap();

    let first = vec![0x31; 4_000_000];
    let second = vec![0xC7; 4_000_000];
    let sources = dir.path().join("sources");
    std::fs::create_dir_all(sources.join("first")).unwrap();
    std::fs::create_dir_all(sources.join("second")).unwrap();
    let first_path = sources.join("first/same.bin");
    let second_path = sources.join("second/same.bin");
    std::fs::write(&first_path, &first).unwrap();
    std::fs::write(&second_path, &second).unwrap();

    let downloads = dir.path().join("host").join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(downloads.join("same.bin"), b"keep existing").unwrap();
    let (a, b) = tokio::join!(
        session.send_file(&first_path, |_, _| {}),
        session.send_file(&second_path, |_, _| {}),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a, b);
    assert_eq!(
        std::fs::read(downloads.join("same.bin")).unwrap(),
        b"keep existing"
    );
    let contents = [
        std::fs::read(downloads.join(a)).unwrap(),
        std::fs::read(downloads.join(b)).unwrap(),
    ];
    assert!(contents.contains(&first));
    assert!(contents.contains(&second));
    assert_no_partial_files(&downloads);
    session.close();
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_fetches_with_same_name_keep_both_contents() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("host").join("shared");
    std::fs::create_dir_all(shared.join("first")).unwrap();
    std::fs::create_dir_all(shared.join("second")).unwrap();
    let first = vec![0x25; 4_000_000];
    let second = vec![0xD2; 4_000_000];
    std::fs::write(shared.join("first/same.bin"), &first).unwrap();
    std::fs::write(shared.join("second/same.bin"), &second).unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, host.addr, None, &info(false))
        .await
        .unwrap();
    let session = handshake.login(Some(PASSWORD)).await.unwrap();

    let downloads = dir.path().join("client-downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(downloads.join("same.bin"), b"keep existing").unwrap();
    let (a, b) = tokio::join!(
        session.fetch_file("first/same.bin", &downloads, |_, _| {}),
        session.fetch_file("second/same.bin", &downloads, |_, _| {}),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a, b);
    assert_eq!(
        std::fs::read(downloads.join("same.bin")).unwrap(),
        b"keep existing"
    );
    let contents = [std::fs::read(a).unwrap(), std::fs::read(b).unwrap()];
    assert!(contents.contains(&first));
    assert!(contents.contains(&second));
    assert_no_partial_files(&downloads);
    session.close();
    host.host.close();
}

fn assert_no_partial_files(dir: &std::path::Path) {
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".casement-part")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "no partial files stay behind: {leftovers:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_folder_lists_and_downloads_but_never_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("host").join("shared");
    std::fs::create_dir_all(shared.join("Документы")).unwrap();
    let data: Vec<u8> = (0..2_500_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(shared.join("Документы").join("план.bin"), &data).unwrap();
    std::fs::write(shared.join("b.txt"), b"bee").unwrap();
    std::fs::write(dir.path().join("secret.txt"), b"outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path(), shared.join("escape")).unwrap();

    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, host.addr, None, &info(false))
        .await
        .unwrap();
    let session = handshake.login(Some(PASSWORD)).await.unwrap();

    let top = session.list_files("").await.unwrap();
    let names: Vec<_> = top
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_dir))
        .collect();
    assert_eq!(names, [("Документы", true), ("b.txt", false)]);
    assert!(names.contains(&("b.txt", false)));
    let inner = session.list_files("Документы/").await.unwrap();
    assert_eq!(inner.entries.len(), 1);
    assert_eq!(inner.entries[0].size, data.len() as u64);

    let target = dir.path().join("client-downloads");
    let mut last = (0, 0);
    let saved = session
        .fetch_file("Документы/план.bin", &target, |got, total| {
            last = (got, total)
        })
        .await
        .unwrap();
    assert_eq!(saved, target.join("план.bin"));
    assert_eq!(std::fs::read(&saved).unwrap(), data);
    assert_eq!(last, (data.len() as u64, data.len() as u64));
    let again = session
        .fetch_file("b.txt", &target, |_, _| {})
        .await
        .unwrap();
    let twice = session
        .fetch_file("b.txt", &target, |_, _| {})
        .await
        .unwrap();
    assert_eq!(again, target.join("b.txt"));
    assert_eq!(twice, target.join("b (1).txt"));

    for bad in [
        "../secret.txt",
        "Документы/../../secret.txt",
        "escape/secret.txt",
        "nope.txt",
    ] {
        #[cfg(not(unix))]
        if bad.starts_with("escape") {
            continue;
        }
        let err = session
            .fetch_file(bad, &target, |_, _| {})
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::Protocol(_)), "{bad}: {err:?}");
    }
    assert!(session.list_files("..").await.is_err());
    assert!(session
        .fetch_file("Документы", &target, |_, _| {})
        .await
        .is_err());
    session.close();
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sound_arrives_as_datagrams() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, host.addr, None, &info(false))
        .await
        .unwrap();
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();
    let format = session.start_audio().await.unwrap();
    assert_eq!(
        (format.codec.as_str(), format.rate, format.channels),
        ("opus", 48000, 2)
    );
    let audio = session.audio_receiver();
    let mut last = None;
    for _ in 0..20 {
        let packet = tokio::time::timeout(std::time::Duration::from_secs(5), audio.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet.data.len(), 200);
        assert_eq!(packet.data[0] as u64, packet.pts_us / 5000 % 256);
        if let Some(previous) = last {
            assert!(packet.pts_us > previous, "packets in order on loopback");
        }
        last = Some(packet.pts_us);
    }
    // The control stream still works alongside the sound.
    session.ping().await.unwrap();
    session.close();
    host.host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sound_is_refused_when_not_shared() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::load_or_create(&dir.path().join("host")).unwrap();
    let config = HostConfig {
        name: "quiet-host".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        allow_any_source: false,
        screen: None,
        files_dir: None,
        shared_dir: None,
        audio: None,
        settings_dir: None,
    };
    let password = StoredPassword::create(PASSWORD).unwrap();
    let (host, _events) =
        Host::bind(identity, config, Some(password), TrustStore::default()).unwrap();
    let host = std::sync::Arc::new(host);
    let addr = host.local_addr().unwrap();
    let runner = host.clone();
    tokio::spawn(async move { runner.run().await });
    let me = client_identity(dir.path(), "client");
    let handshake = client::connect(&me, addr, None, &info(false))
        .await
        .unwrap();
    let mut session = handshake.login(Some(PASSWORD)).await.unwrap();
    let err = session.start_audio().await.unwrap_err();
    assert!(
        matches!(
            err,
            NetError::Refused {
                code: ErrorCode::NotAllowed,
                ..
            }
        ),
        "{err:?}"
    );
    session.ping().await.unwrap();
    session.close();
    host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_on_disk_apply_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("host");
    let identity = Identity::load_or_create(&settings).unwrap();
    let config = HostConfig {
        name: "headless".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        allow_any_source: false,
        screen: None,
        files_dir: None,
        shared_dir: None,
        audio: None,
        settings_dir: Some(settings.clone()),
    };
    // Started without a password, as a freshly installed computer is.
    let (host, _events) = Host::bind(identity, config, None, TrustStore::default()).unwrap();
    let host = std::sync::Arc::new(host);
    let addr = host.local_addr().unwrap();
    let runner = host.clone();
    tokio::spawn(async move { runner.run().await });
    let me = client_identity(dir.path(), "client");

    let err = match client::connect(&me, addr, None, &info(true)).await {
        Err(e) => e,
        Ok(handshake) => match handshake.login(None).await {
            Err(e) => e,
            Ok(_) => panic!("logged in without a password or trust"),
        },
    };
    assert!(
        matches!(
            err,
            NetError::Refused {
                code: ErrorCode::NoPasswordSet,
                ..
            }
        ),
        "{err:?}"
    );

    // As `casement-cli password set` does, with the host running.
    StoredPassword::create(PASSWORD)
        .unwrap()
        .save(&settings)
        .unwrap();
    let handshake = client::connect(&me, addr, None, &info(true)).await.unwrap();
    assert!(handshake.needs_password());
    let session = handshake.login(Some(PASSWORD)).await.unwrap();
    assert!(session.remembered);
    session.close();
    assert_eq!(TrustStore::load(&settings).unwrap().devices().len(), 1);

    // Removing the device on disk makes it ask for the password again.
    let mut trust = TrustStore::load(&settings).unwrap();
    trust.remove(&me.fingerprint()).unwrap();
    let handshake = client::connect(&me, addr, None, &info(true)).await.unwrap();
    assert!(handshake.needs_password());
    handshake.close();
    host.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_password_attempts_count_too() {
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(dir.path(), Some(PASSWORD));
    let me = client_identity(dir.path(), "client");
    // Start logins and drop them half-way, as a guesser hiding failures would.
    for _ in 0..3 {
        let handshake = client::connect(&me, host.addr, None, &info(false))
            .await
            .unwrap();
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(400),
            handshake.login(Some("guess-guess-1")),
        )
        .await;
    }
    let err = client::connect(&me, host.addr, None, &info(false))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            NetError::Refused {
                code: ErrorCode::RateLimited,
                ..
            }
        ),
        "{err:?}"
    );
    host.host.close();
}
