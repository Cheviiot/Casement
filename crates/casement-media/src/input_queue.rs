//! Bounded ordered input delivery for slow desktop backends.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use casement_proto::{input, Input};
use tokio::sync::Notify;

const CAPACITY: usize = 128;

pub(crate) enum Command {
    Input(Input),
    Clipboard(String),
}

#[derive(Default)]
struct State {
    commands: VecDeque<Command>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Notify,
    closed: Notify,
}

pub(crate) struct Sender(Arc<Shared>);

pub(crate) struct Receiver(Arc<Shared>);

pub(crate) fn channel() -> (Sender, Receiver) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State::default()),
        ready: Notify::new(),
        closed: Notify::new(),
    });
    (Sender(shared.clone()), Receiver(shared))
}

impl Sender {
    pub(crate) fn send(&self, command: Command) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }

        if let Command::Input(Input {
            kind: Some(input::Kind::Motion(_)),
        }) = &command
        {
            if matches!(
                state.commands.back(),
                Some(Command::Input(Input {
                    kind: Some(input::Kind::Motion(_)),
                }))
            ) {
                *state.commands.back_mut().expect("motion was at queue tail") = command;
                self.0.ready.notify_one();
                return;
            }
        }

        if state.commands.len() == CAPACITY {
            // The receiver must terminate the backend session: a dropped press or release
            // would otherwise leave the desktop with a stuck key or pointer button.
            state.commands.clear();
            state.closed = true;
            self.0.ready.notify_one();
            self.0.closed.notify_one();
            return;
        }

        state.commands.push_back(command);
        self.0.ready.notify_one();
    }

    pub(crate) fn close(&self) {
        close(&self.0);
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.close();
    }
}

impl Receiver {
    pub(crate) async fn recv(&mut self) -> Option<Command> {
        loop {
            // Register before checking state. `notify_one` retains a permit if a producer
            // races with this check, so an enqueue or close cannot be missed.
            let notified = self.0.ready.notified();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(command) = state.commands.pop_front() {
                    return Some(command);
                }
                if state.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    pub(crate) async fn until_closed<F: Future>(&self, future: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            _ = self.closed() => None,
            output = future => Some(output),
        }
    }

    async fn closed(&self) {
        loop {
            let notified = self.0.closed.notified();
            if self
                .0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .closed
            {
                return;
            }
            notified.await;
        }
    }
}

fn close(shared: &Shared) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    state.commands.clear();
    state.closed = true;
    shared.ready.notify_one();
    shared.closed.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use casement_proto::{input, Input, Key, PointerButton, PointerMotion};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn motion(x: f64) -> Command {
        Command::Input(Input {
            kind: Some(input::Kind::Motion(PointerMotion { x, y: 7.0 })),
        })
    }

    fn button(pressed: bool) -> Command {
        Command::Input(Input {
            kind: Some(input::Kind::Button(PointerButton {
                button: 0x110,
                pressed,
            })),
        })
    }

    #[tokio::test]
    async fn delayed_consumer_gets_only_latest_adjacent_motion() {
        let (sender, mut receiver) = channel();
        for x in 0..10_000 {
            sender.send(motion(x as f64));
        }

        match receiver.recv().await.unwrap() {
            Command::Input(Input {
                kind: Some(input::Kind::Motion(position)),
            }) => assert_eq!(position.x, 9_999.0),
            _ => panic!("latest motion was not retained"),
        }
        drop(sender);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn motion_click_motion_keeps_target_and_order() {
        let (sender, mut receiver) = channel();
        sender.send(motion(10.0));
        sender.send(motion(20.0));
        sender.send(button(true));
        sender.send(motion(30.0));
        sender.send(motion(40.0));

        let first = receiver.recv().await.unwrap();
        let click = receiver.recv().await.unwrap();
        let last = receiver.recv().await.unwrap();
        match first {
            Command::Input(Input {
                kind: Some(input::Kind::Motion(position)),
            }) => assert_eq!(position.x, 20.0),
            _ => panic!("motion before click missing"),
        }
        assert!(matches!(
            click,
            Command::Input(Input {
                kind: Some(input::Kind::Button(PointerButton { pressed: true, .. }))
            })
        ));
        match last {
            Command::Input(Input {
                kind: Some(input::Kind::Motion(position)),
            }) => assert_eq!(position.x, 40.0),
            _ => panic!("motion after click missing"),
        }
    }

    #[tokio::test]
    async fn enqueue_and_close_wake_waiter_and_drop_discards_backlog() {
        let (sender, mut receiver) = channel();
        let waiter = tokio::spawn(async move { receiver.recv().await });
        tokio::task::yield_now().await;
        sender.send(motion(1.0));
        assert!(matches!(
            waiter.await.unwrap(),
            Some(Command::Input(Input {
                kind: Some(input::Kind::Motion(_))
            }))
        ));

        let (sender, mut receiver) = channel();
        sender.send(Command::Input(Input {
            kind: Some(input::Kind::Key(Key {
                code: 30,
                pressed: true,
            })),
        }));
        drop(sender);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .is_none()
        );

        let (sender, mut receiver) = channel();
        let waiter = tokio::spawn(async move { receiver.recv().await });
        tokio::task::yield_now().await;
        drop(sender);
        assert!(tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn overflow_discards_partial_transition_queue_and_terminates() {
        let (sender, mut receiver) = channel();
        for index in 0..CAPACITY {
            sender.send(button(index % 2 == 0));
        }
        sender.send(button(false));

        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .is_none()
        );
        sender.send(button(true));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .is_none()
        );
    }

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn close_cancels_an_in_flight_operation() {
        let (sender, receiver) = channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let probe_dropped = dropped.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            receiver
                .until_closed(async move {
                    let _probe = DropProbe(probe_dropped);
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                })
                .await
        });

        started_rx.await.unwrap();
        sender.close();
        assert!(tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }
}
