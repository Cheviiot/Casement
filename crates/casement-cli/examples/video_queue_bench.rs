use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use casement_net::{
    auth::TrustStore,
    client::{self, ClientInfo},
    host::{Host, HostConfig},
    identity::Identity,
    screen::{
        EncodedFrame, ScreenControl, ScreenEvent, ScreenProvider, ScreenRequest, ScreenStream,
        StartFuture,
    },
    video::VideoBudget,
};
use casement_proto::Input;
use tokio::sync::mpsc;

const FPS: u64 = 60;
const READ_FPS: u64 = 15;
const PAYLOAD_BYTES: usize = 64 * 1024;
const WARMUP: Duration = Duration::from_secs(5);
const MEASURE: Duration = Duration::from_secs(5);
const PROVIDER_QUEUE: usize = 3;

struct Metrics {
    origin: Instant,
    force_keyframe: AtomicBool,
    produced: AtomicU64,
    dropped_before_keyframe: AtomicU64,
    overflow_events: AtomicU64,
    source_recoveries: AtomicU64,
    host_keyframe_requests: AtomicU64,
}

impl Metrics {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            force_keyframe: AtomicBool::new(false),
            produced: AtomicU64::new(0),
            dropped_before_keyframe: AtomicU64::new(0),
            overflow_events: AtomicU64::new(0),
            source_recoveries: AtomicU64::new(0),
            host_keyframe_requests: AtomicU64::new(0),
        }
    }
}

struct FakeControl(Arc<Metrics>);

impl ScreenControl for FakeControl {
    fn request_keyframe(&self) {
        self.0
            .host_keyframe_requests
            .fetch_add(1, Ordering::Relaxed);
        self.0.force_keyframe.store(true, Ordering::Release);
    }

    fn input(&self, _: Input) {}
}

struct FakeScreen {
    metrics: Arc<Metrics>,
}

impl ScreenProvider for FakeScreen {
    fn start(&self, _: ScreenRequest) -> StartFuture {
        let metrics = self.metrics.clone();
        Box::pin(async move {
            let (tx, frames) = mpsc::channel(PROVIDER_QUEUE);
            let frame_overflow = Arc::new(AtomicBool::new(false));
            let overflow_writer = frame_overflow.clone();
            let source_metrics = metrics.clone();
            tokio::spawn(async move {
                let period = Duration::from_nanos(1_000_000_000 / FPS);
                let mut ticker = tokio::time::interval(period);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut sequence = 0u64;
                let mut waiting_for_keyframe = false;
                loop {
                    ticker.tick().await;
                    let forced = source_metrics.force_keyframe.swap(false, Ordering::AcqRel);
                    let keyframe = forced || sequence % (FPS * 2) == 0;
                    if waiting_for_keyframe && !keyframe {
                        source_metrics
                            .dropped_before_keyframe
                            .fetch_add(1, Ordering::Relaxed);
                        sequence += 1;
                        continue;
                    }
                    let frame = EncodedFrame {
                        pts_us: source_metrics.origin.elapsed().as_micros() as u64,
                        keyframe,
                        data: vec![sequence as u8; PAYLOAD_BYTES],
                    };
                    match tx.try_send(frame) {
                        Ok(()) => {
                            if waiting_for_keyframe && keyframe {
                                source_metrics
                                    .source_recoveries
                                    .fetch_add(1, Ordering::Relaxed);
                                waiting_for_keyframe = false;
                            }
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            source_metrics
                                .overflow_events
                                .fetch_add(1, Ordering::Relaxed);
                            overflow_writer.store(true, Ordering::Release);
                            waiting_for_keyframe = true;
                            // Ask the synthetic encoder for a keyframe after provider overflow.
                            source_metrics.force_keyframe.store(true, Ordering::Release);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
                    source_metrics.produced.fetch_add(1, Ordering::Relaxed);
                    sequence += 1;
                }
            });
            let (events_tx, events) = mpsc::unbounded_channel::<ScreenEvent>();
            drop(events_tx);
            Ok(ScreenStream {
                width: 1280,
                height: 800,
                codec: "h264-synthetic".into(),
                frames,
                frame_overflow,
                control: Box::new(FakeControl(metrics)),
                events: Some(events),
                monitors: Vec::new(),
                monitor: String::new(),
                cursor_embedded: true,
            })
        })
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) as f64 * p).ceil() as usize;
    sorted[index]
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bounded-decoder".into());
    if !matches!(mode.as_str(), "bounded-decoder" | "current" | "slow-reader") {
        return Err(format!("unknown mode: {mode}; use bounded-decoder or slow-reader").into());
    }
    let scratch = std::env::var_os("CASEMENT_BENCH_DIR")
        .map(std::path::PathBuf::from)
        .ok_or("CASEMENT_BENCH_DIR is required")?;
    let host_dir = scratch.join("host");
    let client_dir = scratch.join("client");
    std::fs::create_dir_all(&host_dir)?;
    std::fs::create_dir_all(&client_dir)?;
    let host_identity = Identity::load_or_create(&host_dir)?;
    let client_identity = Identity::load_or_create(&client_dir)?;
    let mut trust = TrustStore::load(&host_dir)?;
    trust.add(client_identity.fingerprint(), "synthetic benchmark")?;
    let metrics = Arc::new(Metrics::new());
    let host_config = HostConfig {
        name: "latency-loopback".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse::<SocketAddr>()?,
        allow_any_source: false,
        screen: Some(Arc::new(FakeScreen {
            metrics: metrics.clone(),
        })),
        files_dir: None,
        shared_dir: None,
        audio: None,
        settings_dir: None,
    };
    let (host, _events) = Host::bind(host_identity, host_config, None, trust)?;
    let address = host.local_addr()?;
    let host = Arc::new(host);
    let host_runner = tokio::spawn({
        let host = host.clone();
        async move { host.run().await }
    });
    let info = ClientInfo {
        name: "synthetic-client".into(),
        os: "linux".into(),
        remember: false,
    };
    let handshake = client::connect(&client_identity, address, None, &info).await?;
    let mut session = handshake.login(None).await?;
    let mut screen = session.start_screen(0, 0).await?;
    let benchmark_start = metrics.origin.elapsed();
    let warmup_end = benchmark_start + WARMUP;
    let benchmark_end = warmup_end + MEASURE;
    let mut ages = Vec::<u64>::new();
    let mut wire_frames_received = 0u64;
    let mut frames_accepted = 0u64;
    let mut frames_dropped = 0u64;
    let mut frames_consumed = 0u64;
    let mut measured_keyframes = 0u64;
    let mut measurement_start = None;
    if mode == "bounded-decoder" {
        let (mut control, _messages, handle) = session.split();
        let (frames_tx, mut frames_rx) = mpsc::channel::<EncodedFrame>(2);
        let mut video = screen.video;
        tokio::spawn(async move {
            loop {
                match casement_net::screen::read_frame(&mut video).await {
                    Ok(Some(frame)) => {
                        if frames_tx.send(frame).await.is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            }
        });
        let read_period = Duration::from_nanos(1_000_000_000 / READ_FPS);
        let mut decode_tick = tokio::time::interval(read_period);
        decode_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut recovery_tick = tokio::time::interval(Duration::from_millis(100));
        recovery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut queue = VecDeque::new();
        let mut budget = VideoBudget::default();
        while metrics.origin.elapsed() < benchmark_end {
            tokio::select! {
                Some(frame) = frames_rx.recv() => {
                    let now = metrics.origin.elapsed();
                    if now >= warmup_end {
                        wire_frames_received += 1;
                    }
                    if budget.accept(frame.keyframe, queue.len() as u64) {
                        if now >= warmup_end {
                            frames_accepted += 1;
                        }
                        queue.push_back(frame);
                    } else if now >= warmup_end {
                        frames_dropped += 1;
                    }
                }
                _ = decode_tick.tick() => {
                    if let Some(frame) = queue.pop_front() {
                        let now = metrics.origin.elapsed();
                        if now >= warmup_end {
                            measurement_start.get_or_insert(now);
                            ages.push((now.as_micros() as u64).saturating_sub(frame.pts_us));
                            frames_consumed += 1;
                            if frame.keyframe {
                                measured_keyframes += 1;
                            }
                        }
                    }
                }
                _ = recovery_tick.tick(), if budget.recovering => {
                    control.request_keyframe().await?;
                }
            }
        }
        handle.close_gracefully().await;
    } else {
        let read_period = Duration::from_nanos(1_000_000_000 / READ_FPS);
        let mut reader_tick = tokio::time::interval(read_period);
        reader_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while metrics.origin.elapsed() < benchmark_end {
            reader_tick.tick().await;
            let frame = casement_net::screen::read_frame(&mut screen.video).await?;
            let Some(frame) = frame else { break };
            let now = metrics.origin.elapsed();
            if now >= warmup_end {
                measurement_start.get_or_insert(now);
                ages.push((now.as_micros() as u64).saturating_sub(frame.pts_us));
                wire_frames_received += 1;
                frames_consumed += 1;
                if frame.keyframe {
                    measured_keyframes += 1;
                }
            }
        }
        session.close();
    }
    host.close();
    let _ = tokio::time::timeout(Duration::from_secs(2), host_runner).await;
    ages.sort_unstable();
    let p50 = percentile(&ages, 0.50);
    let p90 = percentile(&ages, 0.90);
    let max = ages.last().copied().unwrap_or_default();
    let elapsed = measurement_start.map_or(0.0, |start| {
        (metrics.origin.elapsed() - start)
            .min(MEASURE)
            .as_secs_f64()
    });
    println!("mode={mode}");
    println!("metric=synthetic_frame_age_at_consumer_ms");
    println!("measurement_seconds={elapsed:.3}");
    println!("frames_wire_received={wire_frames_received}");
    println!("frames_accepted_to_decoder={frames_accepted}");
    println!("frames_dropped_by_video_budget={frames_dropped}");
    println!("frames_consumed_by_model={frames_consumed}");
    println!("synthetic_queue_age_ms_p50={:.3}", p50 as f64 / 1000.0);
    println!("synthetic_queue_age_ms_p90={:.3}", p90 as f64 / 1000.0);
    println!("synthetic_queue_age_ms_max={:.3}", max as f64 / 1000.0);
    println!(
        "source_frames_enqueued={}",
        metrics.produced.load(Ordering::Relaxed)
    );
    println!(
        "source_queue_overflows={}",
        metrics.overflow_events.load(Ordering::Relaxed)
    );
    println!(
        "source_dropped_before_keyframe={}",
        metrics.dropped_before_keyframe.load(Ordering::Relaxed)
    );
    println!(
        "source_recovery_keyframes_enqueued={}",
        metrics.source_recoveries.load(Ordering::Relaxed)
    );
    println!(
        "host_keyframe_requests={}",
        metrics.host_keyframe_requests.load(Ordering::Relaxed)
    );
    println!("measurement_keyframes_consumed={measured_keyframes}");
    println!("payload_bytes={PAYLOAD_BYTES}");
    println!("producer_fps={FPS}");
    println!("consumer_fps={READ_FPS}");
    println!("provider_queue_capacity={}", PROVIDER_QUEUE);
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("benchmark failed: {error}");
        std::process::exit(1);
    }
}
