use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    net::UdpSocket,
    task::JoinHandle,
    time::{self, Instant},
};

const MAX_QUEUE: usize = 128;
const MAX_BYTES_PER_SECOND: f64 = 1_000_000.0;
type PacketQueue = BinaryHeap<Reverse<(Instant, u64, Vec<u8>, bool)>>;

#[derive(Default)]
struct Counters {
    forwarded: AtomicU64,
    dropped: AtomicU64,
    peak_queue: AtomicU64,
}

/// A snapshot of packets handled by the relay.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub forwarded: u64,
    pub dropped: u64,
    pub peak_queue: u64,
}

/// Loopback UDP relay with deterministic loss, delay, rate limiting, and outage controls.
pub struct Relay {
    addr: SocketAddr,
    task: JoinHandle<()>,
    counters: Arc<Counters>,
}

impl Relay {
    pub async fn start(
        host: SocketAddr,
        impaired: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let front = UdpSocket::bind("127.0.0.1:0").await?;
        let addr = front.local_addr()?;
        let back = UdpSocket::bind("127.0.0.1:0").await?;
        back.connect(host).await?;
        let counters = Arc::new(Counters::default());
        let task_counters = counters.clone();
        let task = tokio::spawn(async move {
            run(front, back, impaired, paused, task_counters).await;
        });
        Ok(Self {
            addr,
            task,
            counters,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stats(&self) -> Stats {
        Stats {
            forwarded: self.counters.forwarded.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
            peak_queue: self.counters.peak_queue.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    front: UdpSocket,
    back: UdpSocket,
    impaired: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    counters: Arc<Counters>,
) {
    let mut client = None;
    let mut queue = PacketQueue::new();
    let mut sequence = 0u64;
    let mut loss_sequence = 0u64;
    let mut next_host_send = Instant::now();
    let mut next_client_send = Instant::now();
    let mut front_buf = vec![0; 65_535];
    let mut back_buf = vec![0; 65_535];

    loop {
        let deadline = queue.peek().map(|Reverse((due, ..))| *due);
        tokio::select! {
            received = front.recv_from(&mut front_buf) => {
                let Ok((len, peer)) = received else { return };
                client = Some(peer);
                admit(
                    &front_buf[..len], true, impaired.load(Ordering::Relaxed), paused.load(Ordering::Relaxed),
                    &mut queue, &mut sequence, &mut loss_sequence, &mut next_host_send,
                    &mut next_client_send, &back, &front, peer, &counters,
                ).await;
            }
            received = back.recv(&mut back_buf) => {
                let Ok(len) = received else { return };
                if let Some(peer) = client {
                    admit(
                        &back_buf[..len], false, impaired.load(Ordering::Relaxed), paused.load(Ordering::Relaxed),
                        &mut queue, &mut sequence, &mut loss_sequence, &mut next_host_send,
                        &mut next_client_send, &back, &front, peer, &counters,
                    ).await;
                }
            }
            _ = async {
                if let Some(deadline) = deadline {
                    time::sleep_until(deadline).await
                } else {
                    futures_sleep_pending().await
                }
            } => {
                while queue.peek().is_some_and(|Reverse((due, ..))| *due <= Instant::now()) {
                    let Reverse((_, _, data, to_host)) = queue.pop().expect("peeked packet");
                    if paused.load(Ordering::Relaxed) {
                        counters.dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let sent = if to_host {
                        back.send(&data).await
                    } else {
                        front
                            .send_to(&data, client.expect("client learned"))
                            .await
                    };
                    if sent.is_ok() {
                        counters.forwarded.fetch_add(1, Ordering::Relaxed);
                    } else {
                        counters.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

async fn futures_sleep_pending() {
    std::future::pending::<()>().await;
}

#[allow(clippy::too_many_arguments)]
async fn admit(
    bytes: &[u8],
    to_host: bool,
    impaired: bool,
    paused: bool,
    queue: &mut PacketQueue,
    sequence: &mut u64,
    loss_sequence: &mut u64,
    next_host_send: &mut Instant,
    next_client_send: &mut Instant,
    back: &UdpSocket,
    front: &UdpSocket,
    client: SocketAddr,
    counters: &Counters,
) {
    if paused {
        counters.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if !impaired {
        let sent = if to_host {
            back.send(bytes).await
        } else {
            front.send_to(bytes, client).await
        };
        if sent.is_ok() {
            counters.forwarded.fetch_add(1, Ordering::Relaxed);
        } else {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }
    *loss_sequence += 1;
    if *loss_sequence % 20 == 0 {
        counters.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if queue.len() >= MAX_QUEUE {
        counters.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let rate_slot = if to_host {
        next_host_send
    } else {
        next_client_send
    };
    let now = Instant::now();
    let jitter_ms = *sequence % 11;
    let due = now + Duration::from_millis(20 + jitter_ms);
    let due = due.max(*rate_slot);
    *rate_slot = due + Duration::from_secs_f64(bytes.len() as f64 / MAX_BYTES_PER_SECOND);
    let id = *sequence;
    *sequence += 1;
    queue.push(Reverse((due, id, bytes.to_vec(), to_host)));
    counters
        .peak_queue
        .fetch_max(queue.len() as u64, Ordering::Relaxed);
}
