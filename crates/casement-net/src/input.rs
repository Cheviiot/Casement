//! Reliable transitions, disposable motion, and release of held input on disconnect.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use casement_proto::{
    control, input, Control, Input, Key, Message, PointerButton, PointerMotion, ReliableInput,
    SequencedMotion, MOTION_DATAGRAM,
};

use crate::{framing, screen::ScreenControl, NetError};

const INPUT_QUEUE: usize = 128;
const MOTION_REFRESH: Duration = Duration::from_millis(100);

#[derive(Default)]
struct PointerState {
    sequence: u64,
    position: Option<PointerMotion>,
    changed: Option<Instant>,
}

impl PointerState {
    fn packet(&self) -> Option<Vec<u8>> {
        let mut bytes = vec![MOTION_DATAGRAM];
        SequencedMotion {
            position: Some(self.position.clone()?),
            sequence: self.sequence,
        }
        .encode(&mut bytes)
        .expect("Vec encoding");
        Some(bytes)
    }
}

struct Sender {
    connection: quinn::Connection,
    queue: Option<tokio::sync::mpsc::Sender<ReliableInput>>,
    pointer: Arc<Mutex<PointerState>>,
    task: Option<tokio::task::AbortHandle>,
}

impl Drop for Sender {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Cheap to clone and safe to call from GTK without blocking its main loop.
#[derive(Clone)]
pub struct InputSender(Arc<Sender>);

impl InputSender {
    pub(crate) fn new(connection: quinn::Connection, reliable: bool) -> Self {
        let pointer = Arc::new(Mutex::new(PointerState::default()));
        let (queue, task) = if reliable {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<ReliableInput>(INPUT_QUEUE);
            let peer = connection.clone();
            let refresh_pointer = pointer.clone();
            let task = tokio::spawn(async move {
                let result: Result<(), NetError> = async {
                    let mut stream = peer.open_uni().await?;
                    let _ = stream.set_priority(2);
                    let mut refresh = tokio::time::interval(MOTION_REFRESH);
                    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                        command = rx.recv() => {
                            let Some(command) = command else { break };
                        framing::write(
                            &mut stream,
                            &Control::new(control::Kind::ReliableInput(command)),
                        )
                        .await?;
                        }
                        _ = refresh.tick() => {
                            // A final motion datagram can be lost just as the mouse
                            // stops. Refresh the same sequence while stationary;
                            // delivered duplicates never reach the desktop backend.
                            let bytes = {
                                let pointer = refresh_pointer.lock().unwrap_or_else(|e| e.into_inner());
                                pointer.changed.filter(|last| last.elapsed() >= MOTION_REFRESH).and_then(|_| pointer.packet())
                            };
                            if let Some(bytes) = bytes {
                                peer.send_datagram(bytes.into()).map_err(|e| NetError::Stream(e.to_string()))?;
                            }
                        }
                        _ = peer.closed() => return Ok(()),
                        }
                    }
                    let _ = stream.finish();
                    Ok(())
                }
                .await;
                if result.is_err() {
                    peer.close(0u32.into(), b"input stream failed");
                }
            });
            (Some(tx), Some(task.abort_handle()))
        } else {
            (None, None)
        };
        Self(Arc::new(Sender {
            connection,
            queue,
            pointer,
            task,
        }))
    }

    pub fn send(&self, input: &Input) -> Result<(), NetError> {
        let Some(queue) = &self.0.queue else {
            return self.datagram(input.encode_to_vec());
        };
        let mut pointer = self.0.pointer.lock().unwrap_or_else(|e| e.into_inner());
        match &input.kind {
            Some(input::Kind::Motion(position)) => {
                if !finite(position) {
                    return Err(NetError::Protocol("invalid pointer position".into()));
                }
                pointer.sequence = pointer.sequence.saturating_add(1);
                pointer.position = Some(position.clone());
                pointer.changed = Some(Instant::now());
                self.datagram(pointer.packet().expect("position was set"))
            }
            Some(kind) => {
                let positioned = matches!(kind, input::Kind::Button(_) | input::Kind::Scroll(_));
                let command = ReliableInput {
                    input: Some(input.clone()),
                    position: if positioned {
                        pointer.position.clone()
                    } else {
                        None
                    },
                    motion_sequence: pointer.sequence,
                };
                queue.try_send(command).map_err(|_| {
                    // Never silently lose a release or build an unbounded typing backlog.
                    // Closing makes the host release every key/button it still holds.
                    self.0
                        .connection
                        .close(0u32.into(), b"input queue overloaded");
                    NetError::Stream("input delivery stopped".into())
                })
            }
            None => Ok(()),
        }
    }

    fn datagram(&self, bytes: Vec<u8>) -> Result<(), NetError> {
        self.0
            .connection
            .send_datagram(bytes.into())
            .map_err(|e| NetError::Stream(e.to_string()))
    }

    #[cfg(test)]
    pub(crate) fn legacy(&self) -> Self {
        Self::new(self.0.connection.clone(), false)
    }
}

#[derive(Default)]
struct Held {
    pointer: PointerState,
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
}

pub(crate) struct Receiver {
    control: Arc<dyn ScreenControl>,
    held: Mutex<Held>,
}

impl Receiver {
    pub(crate) fn new(control: Arc<dyn ScreenControl>) -> Self {
        Self {
            control,
            held: Mutex::new(Held::default()),
        }
    }

    pub(crate) fn datagram(&self, bytes: &[u8]) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if bytes.first() == Some(&MOTION_DATAGRAM) {
            let Ok(packet) = SequencedMotion::decode(&bytes[1..]) else {
                return;
            };
            let Some(position) = packet.position.filter(finite) else {
                return;
            };
            if packet.sequence <= held.pointer.sequence {
                return;
            }
            held.pointer.sequence = packet.sequence;
            held.pointer.position = Some(position.clone());
            self.forward(input::Kind::Motion(position));
        } else if let Ok(input) = Input::decode(bytes) {
            if let Some(input::Kind::Motion(position)) = &input.kind {
                if !finite(position) {
                    return;
                }
                held.pointer.position = Some(position.clone());
            }
            self.apply(&mut held, input);
        }
    }

    pub(crate) fn reliable(&self, command: ReliableInput) {
        let Some(input) = command.input else { return };
        if !matches!(
            &input.kind,
            Some(input::Kind::Key(_) | input::Kind::Button(_) | input::Kind::Scroll(_))
        ) {
            return;
        }
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let positioned = matches!(
            &input.kind,
            Some(input::Kind::Button(_) | input::Kind::Scroll(_))
        );
        let position = command.position.filter(finite).filter(|_| positioned);
        let restore = if position.is_some() && held.pointer.sequence > command.motion_sequence {
            held.pointer.position.clone()
        } else {
            None
        };
        if let Some(position) = position {
            if held.pointer.position.as_ref() != Some(&position) {
                self.forward(input::Kind::Motion(position.clone()));
            }
            if restore.is_none() {
                held.pointer.position = Some(position);
                held.pointer.sequence = command.motion_sequence;
            }
        }
        self.apply(&mut held, input);
        if let Some(position) = restore {
            self.forward(input::Kind::Motion(position));
        }
    }

    fn apply(&self, held: &mut Held, input: Input) {
        match &input.kind {
            // Bound retained state to the Linux input key/button namespaces. Invalid
            // peer codes must not grow a held-key set without limit.
            Some(input::Kind::Key(key)) if key.code > 0x2ff => return,
            Some(input::Kind::Button(button)) if !(0x110..=0x11f).contains(&button.button) => {
                return
            }
            Some(input::Kind::Scroll(scroll))
                if !scroll.dx.is_finite() || !scroll.dy.is_finite() =>
            {
                return
            }
            Some(input::Kind::Key(key)) => update(&mut held.keys, key.code, key.pressed),
            Some(input::Kind::Button(button)) => {
                update(&mut held.buttons, button.button, button.pressed)
            }
            _ => {}
        }
        self.control.input(input);
    }

    fn forward(&self, kind: input::Kind) {
        self.control.input(Input { kind: Some(kind) });
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let held = self.held.get_mut().unwrap_or_else(|e| e.into_inner());
        for code in &held.keys {
            self.control.input(Input {
                kind: Some(input::Kind::Key(Key {
                    code: *code,
                    pressed: false,
                })),
            });
        }
        for button in &held.buttons {
            self.control.input(Input {
                kind: Some(input::Kind::Button(PointerButton {
                    button: *button,
                    pressed: false,
                })),
            });
        }
    }
}

fn update(set: &mut BTreeSet<u32>, code: u32, pressed: bool) {
    if pressed {
        set.insert(code);
    } else {
        set.remove(&code);
    }
}

fn finite(position: &PointerMotion) -> bool {
    position.x.is_finite() && position.y.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Capture(Mutex<Vec<Input>>);
    impl ScreenControl for Capture {
        fn request_keyframe(&self) {}
        fn input(&self, input: Input) {
            self.0.lock().unwrap().push(input);
        }
    }
    fn motion(receiver: &Receiver, sequence: u64, x: f64) {
        let mut data = vec![MOTION_DATAGRAM];
        SequencedMotion {
            sequence,
            position: Some(PointerMotion { x, y: 0.0 }),
        }
        .encode(&mut data)
        .unwrap();
        receiver.datagram(&data);
    }

    #[tokio::test]
    async fn lost_final_motion_is_refreshed_without_replaying_delivered_input() {
        let (client, server, connection, peer) = crate::test_support::loopback_pair().await;
        let sender = InputSender::new(connection, true);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.read_datagram())
                .await
                .is_err()
        );
        sender
            .send(&Input {
                kind: Some(input::Kind::Motion(PointerMotion { x: 17.0, y: 8.0 })),
            })
            .unwrap();
        let lost = peer.read_datagram().await.unwrap(); // Deliberately drop the first packet.
        let refreshed = tokio::time::timeout(Duration::from_secs(2), peer.read_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lost, refreshed);
        let capture = Arc::new(Capture::default());
        let receiver = Receiver::new(capture.clone());
        receiver.datagram(&refreshed);
        receiver.datagram(&refreshed);
        assert_eq!(
            capture.0.lock().unwrap().len(),
            1,
            "duplicates must not reach the desktop"
        );
        sender
            .send(&Input {
                kind: Some(input::Kind::Motion(PointerMotion { x: 42.0, y: 9.0 })),
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let packet = peer.read_datagram().await.unwrap();
                let motion = SequencedMotion::decode(&packet[1..]).unwrap();
                if motion.sequence == 2 {
                    receiver.datagram(&packet);
                    break;
                }
            }
        })
        .await
        .unwrap();
        receiver.datagram(&refreshed); // A delayed refresh cannot restore an old position.
        {
            let inputs = capture.0.lock().unwrap();
            assert_eq!(inputs.len(), 2);
            assert_eq!(
                inputs[1].kind,
                Some(input::Kind::Motion(PointerMotion { x: 42.0, y: 9.0 }))
            );
        }
        let task = sender.0.task.as_ref().unwrap().clone();
        drop(sender);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        client.close(0u32.into(), b"test ended");
        server.close(0u32.into(), b"test ended");
    }

    #[test]
    fn click_keeps_its_target_when_newer_motion_overtakes_it() {
        let capture = Arc::new(Capture::default());
        let receiver = Receiver::new(capture.clone());
        motion(&receiver, 3, 30.0);
        motion(&receiver, 1, 10.0); // late motion must not move the cursor back
        receiver.reliable(ReliableInput {
            input: Some(Input {
                kind: Some(input::Kind::Button(PointerButton {
                    button: 272,
                    pressed: true,
                })),
            }),
            position: Some(PointerMotion { x: 20.0, y: 0.0 }),
            motion_sequence: 2,
        });
        let inputs = capture.0.lock().unwrap();
        assert_eq!(inputs.len(), 4);
        assert_eq!(
            inputs[0].kind,
            Some(input::Kind::Motion(PointerMotion { x: 30.0, y: 0.0 }))
        );
        assert_eq!(
            inputs[1].kind,
            Some(input::Kind::Motion(PointerMotion { x: 20.0, y: 0.0 }))
        );
        assert!(matches!(&inputs[2].kind, Some(input::Kind::Button(button)) if button.pressed));
        assert_eq!(inputs[3].kind, inputs[0].kind);
    }

    #[test]
    fn disconnect_releases_held_keys_and_buttons_including_legacy_input() {
        let capture = Arc::new(Capture::default());
        {
            let receiver = Receiver::new(capture.clone());
            receiver.datagram(
                &Input {
                    kind: Some(input::Kind::Key(Key {
                        code: 29,
                        pressed: true,
                    })),
                }
                .encode_to_vec(),
            );
            receiver.datagram(
                &Input {
                    kind: Some(input::Kind::Button(PointerButton {
                        button: 272,
                        pressed: true,
                    })),
                }
                .encode_to_vec(),
            );
        }
        let inputs = capture.0.lock().unwrap();
        assert_eq!(inputs.len(), 4);
        assert!(
            matches!(&inputs[2].kind, Some(input::Kind::Key(key)) if key.code == 29 && !key.pressed)
        );
        assert!(
            matches!(&inputs[3].kind, Some(input::Kind::Button(button)) if button.button == 272 && !button.pressed)
        );
    }
}
