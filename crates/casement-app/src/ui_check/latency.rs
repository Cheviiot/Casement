//! Causal motion → host colour → encoded H.264 → GTK paint measurement.

use super::*;
use casement_net::client::FileSender;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

struct Load {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    job: Option<tokio::task::JoinHandle<Result<(), String>>>,
    bytes: Arc<AtomicU64>,
}

impl Load {
    fn start(files: FileSender, root: &Path) -> Self {
        let dir = root.join("downloads/latency");
        let bytes = Arc::new(AtomicU64::new(0));
        let progress = bytes.clone();
        let (stop, cancel) = tokio::sync::oneshot::channel();
        let job = crate::core::runtime().spawn(async move {
            tokio::select! {
                _ = cancel => Ok(()),
                result = files.fetch("latency-load.bin", &dir, move |received, _| { progress.store(received, Ordering::Relaxed); }) => result.map(|_| ()).map_err(|e| e.to_string()),
            }
        });
        Self {
            stop: Some(stop),
            job: Some(job),
            bytes,
        }
    }

    async fn stop(mut self) -> Result<(), String> {
        self.stop.take();
        let job = self.job.take().unwrap();
        crate::core::run(job).await.map_err(|e| e.to_string())?
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(job) = &self.job {
            job.abort();
        }
    }
}

pub(super) async fn run(
    window: &Window,
    picture: &gtk::Picture,
    files: &FileSender,
    root: &Path,
) -> Result<Vec<String>, String> {
    let controllers = picture.observe_controllers();
    let motion = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i))
        .find_map(|controller| controller.downcast::<gtk::EventControllerMotion>().ok())
        .ok_or("latency: no motion controller")?;
    let widget = window.widget();
    let clock = widget.frame_clock().ok_or("latency: no frame clock")?;
    let pending = Rc::new(Cell::new(None::<(u32, Instant)>));
    let samples = Rc::new(RefCell::new(Vec::<f64>::new()));
    let target = pending.clone();
    let results = samples.clone();
    let image = picture.clone();
    let renderer = widget.renderer().ok_or("latency: no renderer")?;
    let handler = clock.connect_after_paint(move |_| {
        let Some((code, sent)) = target.get() else {
            return;
        };
        let paintable = gtk::WidgetPaintable::new(Some(&image));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, image.width() as f64, image.height() as f64);
        let Some(node) = snapshot.to_node() else {
            return;
        };
        // Render just the centre pixel of the completed widget snapshot. A matching
        // unique colour proves this input has passed through the real encoder,
        // transport, decoder and GTK; an unrelated arriving frame is insufficient.
        let texture = renderer.render_texture(
            &node,
            Some(&gtk::graphene::Rect::new(
                image.width() as f32 / 2.0,
                image.height() as f32 / 2.0,
                1.0,
                1.0,
            )),
        );
        let mut pixel = [0u8; 4];
        texture.download(&mut pixel, 4);
        let rgb = [pixel[2], pixel[1], pixel[0]]; // GDK download is premultiplied BGRA.
        let expected = super::marker::color(code);
        if rgb.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 12) {
            results
                .borrow_mut()
                .push(sent.elapsed().as_secs_f64() * 1000.0);
            target.set(None);
        }
    });
    clock.begin_updating();
    let result = async {
        let mut reports = Vec::new();
        glib::timeout_future(Duration::from_millis(100)).await;
        for (name, impaired, transfer) in [("normal", false, false), ("file", false, true), ("impaired", true, false), ("impaired-file", true, true)] {
            if impaired { fs::write(root.join("impaired"), "on").map_err(|e| e.to_string())?; }
            else { let _ = fs::remove_file(root.join("impaired")); }
            wait_until("network profile applied", Duration::from_secs(2), || fs::read_to_string(root.join("network-state")).ok().as_deref() == Some(&format!("{impaired} false"))).await?;
            let load = transfer.then(|| Load::start(files.clone(), root));
            if let Some(load) = &load {
                wait_until("bulk file transfer has data", Duration::from_secs(5), || load.bytes.load(Ordering::Relaxed) > 0).await?;
            }
            samples.borrow_mut().clear();
        // The first five events warm up the pipeline; twenty unique colours follow.
        for code in 1..=25u32 {
            let width = picture.width() as f64;
            let height = picture.height() as f64;
            let scale = (width / 640.0).min(height / 360.0);
            let x = (width - 640.0 * scale) / 2.0 + code as f64 * 10.0 * scale;
            pending.set(Some((code, Instant::now())));
            motion.emit_by_name::<()>("motion", &[&x, &(height / 2.0)]);
            widget.queue_draw();
            wait_until(
                "causal input colour reaches GTK paint",
                Duration::from_secs(3),
                || pending.get().is_none(),
            )
            .await.map_err(|error| format!("{name}, marker {code}: {error}"))?;
            if load.as_ref().is_some_and(|load| load.job.as_ref().unwrap().is_finished()) {
                return Err(format!("{name}: bulk transfer ended before measurement completed"));
            }
        }
            let transferred = load.as_ref().map(|load| load.bytes.load(Ordering::Relaxed)).unwrap_or(0);
            if let Some(load) = load { load.stop().await?; }
            let mut values = samples.borrow()[5..].to_vec();
            values.sort_by(f64::total_cmp);
            let p50 = values[9];
            let p90 = values[17];
            let max = values[19];
            let report = format!("{name}: motion-to-GTK-paint, 20 samples after 5 warmup, p50={p50:.2}ms p90={p90:.2}ms max={max:.2}ms bulk_bytes={transferred}");
            fs::write(root.join(format!("latency-{name}.txt")), format!("{report}\nraw_ms={values:?}\n")).map_err(|e| e.to_string())?;
            reports.push(report);
        }
        // Drop the final movement completely during an outage. Its position must
        // eventually arrive after traffic resumes, without another user movement.
        fs::write(root.join("paused"), "on").map_err(|e| e.to_string())?;
        wait_until("network outage applied", Duration::from_secs(2), || fs::read_to_string(root.join("network-state")).ok().as_deref() == Some("true true")).await?;
        let code = 31u32;
        let w = picture.width() as f64;
        let h = picture.height() as f64;
        let scale = (w / 640.0).min(h / 360.0);
        let x = (w - 640.0*scale)/2.0 + code as f64*10.0*scale;
        pending.set(Some((code, Instant::now())));
        motion.emit_by_name::<()>("motion", &[&x, &(h/2.0)]);
        glib::timeout_future(Duration::from_millis(350)).await;
        fs::remove_file(root.join("paused")).map_err(|e| e.to_string())?;
        wait_until("stationary pointer recovers after outage without new input", Duration::from_secs(3), || pending.get().is_none()).await?;
        reports.push(format!("350ms outage: final pointer reaches GTK after {:.2}ms without another movement", samples.borrow().last().unwrap()));
        Ok::<_, String>(reports)
    }
    .await;
    clock.end_updating();
    clock.disconnect(handler);
    result
}
