//! Loopback-only UI fixture: real H.264 and file transfers, no desktop capture or input.

use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::{Duration, Instant};

use casement_net::auth::TrustStore;
use casement_net::host::{Host, HostConfig};
use casement_net::identity::Identity;
use casement_net::screen::{
    EncodedFrame, Monitor, ScreenControl, ScreenProvider, ScreenRequest, ScreenStream, StartFuture,
};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

struct FixtureScreen {
    root: PathBuf,
}

struct Control(gst::Pipeline);

impl ScreenControl for Control {
    fn request_keyframe(&self) {
        let event = gst::event::CustomUpstream::new(
            gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .field("count", 0u32)
                .build(),
        );
        if let Some(sink) = self.0.by_name("sink") {
            sink.send_event(event);
        }
    }

    fn input(&self, _: casement_proto::Input) {
        // Deliberately never forward synthetic UI input to the operating system.
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

impl ScreenProvider for FixtureScreen {
    fn start(&self, request: ScreenRequest) -> StartFuture {
        let root = self.root.clone();
        Box::pin(async move {
            let monitor = if request.monitor == "fixture-2" {
                "fixture-2"
            } else {
                "fixture-1"
            };
            std::fs::write(root.join("shown-monitor"), monitor).map_err(|e| e.to_string())?;
            let pattern = if monitor == "fixture-2" {
                "smpte"
            } else {
                "ball"
            };
            let encoder = if gst::ElementFactory::find("openh264enc").is_some() {
                "openh264enc bitrate=1000000 gop-size=30 multi-thread=2"
            } else {
                "x264enc tune=zerolatency speed-preset=ultrafast bitrate=1000 key-int-max=30 threads=2"
            };
            let pipeline = gst::parse::launch(&format!(
                "videotestsrc is-live=true pattern={pattern} \
                 ! video/x-raw,width=640,height=360,framerate=30/1 \
                 ! videoconvert ! {encoder} ! h264parse config-interval=-1 \
                 ! video/x-h264,stream-format=byte-stream,alignment=au \
                 ! appsink name=sink sync=false max-buffers=3"
            ))
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;
            let sink = pipeline
                .by_name("sink")
                .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
                .ok_or("no appsink")?;
            let control = Control(pipeline.clone());
            pipeline
                .set_state(gst::State::Playing)
                .map_err(|e| e.to_string())?;
            let (tx, frames) = tokio::sync::mpsc::channel(3);
            let overflow = Arc::new(AtomicBool::new(false));
            let overflow_writer = overflow.clone();
            tokio::task::spawn_blocking(move || {
                while let Ok(sample) = sink.pull_sample() {
                    let Some(buffer) = sample.buffer() else {
                        continue;
                    };
                    let Ok(bytes) = buffer.map_readable() else {
                        continue;
                    };
                    let frame = EncodedFrame {
                        pts_us: buffer.pts().map(|time| time.useconds()).unwrap_or(0),
                        keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                        data: bytes.as_slice().to_vec(),
                    };
                    match tx.try_send(frame) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            overflow_writer.store(true, std::sync::atomic::Ordering::Release);
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
            });
            Ok(ScreenStream {
                width: 640,
                height: 360,
                codec: "h264".into(),
                frames,
                frame_overflow: overflow,
                control: Box::new(control),
                events: None,
                monitors: vec![
                    Monitor {
                        id: "fixture-1".into(),
                        name: "Тестовый экран".into(),
                        width: 640,
                        height: 360,
                        primary: true,
                    },
                    Monitor {
                        id: "fixture-2".into(),
                        name: "Второй экран".into(),
                        width: 640,
                        height: 360,
                        primary: false,
                    },
                ],
                monitor: monitor.into(),
            })
        })
    }
}

fn seed(root: &Path) -> std::io::Result<()> {
    for folder in [
        "client",
        "host",
        "shared/folder",
        "uploads",
        "downloads",
        "xdg",
    ] {
        std::fs::create_dir_all(root.join(folder))?;
    }
    std::fs::write(root.join("client/.casement-ui-fixture"), "")?;
    std::fs::write(root.join("shared/visible.txt"), "download fixture\n")?;
    std::fs::write(root.join("shared/.hidden.txt"), "hidden fixture\n")?;
    std::fs::write(root.join("shared/folder/nested.txt"), "nested fixture\n")?;
    std::fs::write(root.join("downloads/upload-a.txt"), "first upload\n")?;
    std::fs::write(root.join("downloads/upload-b.txt"), "second upload\n")?;
    std::fs::write(
        root.join("xdg/user-dirs.dirs"),
        format!(
            "XDG_DOWNLOAD_DIR=\"{}\"\n",
            root.join("downloads").display()
        ),
    )?;
    Ok(())
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("fixture directory required")?,
    );
    if !root.join(".casement-ui-fixture").is_file() {
        return Err("refusing an unmarked fixture directory".into());
    }
    let root = root.canonicalize()?;
    gst::init()?;
    seed(&root)?;
    let client = Identity::load_or_create(&root.join("client"))?;
    let identity = Identity::load_or_create(&root.join("host"))?;
    let mut trust = TrustStore::load(&root.join("host"))?;
    trust.add(client.fingerprint(), "UI fixture")?;
    let config = HostConfig {
        name: "Тестовый компьютер".into(),
        os: "linux".into(),
        bind: "127.0.0.1:0".parse()?,
        allow_any_source: false,
        screen: Some(Arc::new(FixtureScreen { root: root.clone() })),
        files_dir: Some(root.join("uploads")),
        shared_dir: Some(root.join("shared")),
        audio: None,
        settings_dir: None,
    };
    let (host, mut events) = Host::bind(identity, config, None, trust)?;
    let file = glib::KeyFile::new();
    file.set_string("fixture", "name", "Тестовый компьютер");
    file.set_string("fixture", "host", "127.0.0.1");
    file.set_uint64("fixture", "port", u64::from(host.local_addr()?.port()));
    std::fs::write(root.join("client/connections.ini"), file.to_data())?;
    std::fs::write(root.join("ready"), host.local_addr()?.to_string())?;
    let host = Arc::new(host);
    let server = host.clone();
    let serving = tokio::spawn(async move { server.run().await });
    let logging = tokio::spawn(async move { while events.recv().await.is_some() {} });
    let started = Instant::now();
    while !root.join("stop").exists() && started.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    host.close();
    let _ = serving.await;
    logging.abort();
    Ok(())
}
