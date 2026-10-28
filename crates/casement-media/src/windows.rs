//! The host's screen on Windows: Desktop Duplication through `d3d11screencapturesrc`, a
//! hardware H.264 encoder (AMD AMF, Intel QSV, NVIDIA, Media Foundation) and input
//! through `SendInput`. Clipboard text both ways through the Win32 clipboard, watched by
//! its sequence number.

use crate::keyboard::scancode;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use casement_net::screen::{
    EncodedFrame, Monitor, ScreenControl, ScreenEvent, ScreenProvider, ScreenRequest, ScreenStream,
    StartFuture,
};
use casement_proto::{input, Input};

use crate::frames::{pull_frames, QUEUE};

const MONITORINFOF_PRIMARY: u32 = 1;
const WHEEL_DELTA: f64 = 120.0;

/// Encoders to try, best first; each with low-latency settings and, if those are not
/// accepted by this GStreamer version, bare.
const ENCODERS: &[(&str, &str)] = &[
    (
        "amfh264enc",
        "amfh264enc usage=ultra-low-latency rate-control=cbr bitrate=20000 gop-size=120",
    ),
    (
        "qsvh264enc",
        "qsvh264enc low-latency=true rate-control=cbr bitrate=20000 gop-size=120",
    ),
    (
        "nvd3d11h264enc",
        "nvd3d11h264enc preset=low-latency-hq bitrate=20000 gop-size=120 bframes=0",
    ),
    (
        "mfh264enc",
        "mfh264enc low-latency=true rc-mode=cbr bitrate=20000 gop-size=120 bframes=0",
    ),
    (
        "openh264enc",
        "openh264enc bitrate=12000000 rate-control=bitrate gop-size=120 complexity=low",
    ),
];

#[derive(Clone)]
struct Display {
    monitor: Monitor,
    handle: usize,
    rect: RECT,
}

/// The monitors attached to the desktop, in Windows' order.
fn displays() -> Vec<Display> {
    unsafe extern "system" fn each(
        handle: HMONITOR,
        _dc: HDC,
        _clip: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        let found = &mut *(data as *mut Vec<Display>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(0);
            let device = String::from_utf16_lossy(&info.szDevice[..len]);
            let rect = info.monitorInfo.rcMonitor;
            let id = device.trim_start_matches(r"\\.\").to_string();
            found.push(Display {
                monitor: Monitor {
                    name: id.clone(),
                    id,
                    width: (rect.right - rect.left).max(0) as u32,
                    height: (rect.bottom - rect.top).max(0) as u32,
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                },
                handle: handle as usize,
                rect,
            });
        }
        1
    }
    let mut found: Vec<Display> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(each),
            &mut found as *mut Vec<Display> as LPARAM,
        );
    }
    found
}

/// Shares a monitor of this Windows desktop (the primary one unless asked otherwise).
#[derive(Default)]
pub struct WindowsScreen;

impl WindowsScreen {
    /// Fails when GStreamer, the capture source or every H.264 encoder is missing.
    pub fn new() -> Result<Arc<Self>, String> {
        gst::init().map_err(|e| format!("GStreamer: {e}"))?;
        // Physical pixels everywhere: the capture is in them, and so must input be.
        unsafe {
            windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        gst::ElementFactory::find("d3d11screencapturesrc")
            .ok_or("no screen capture (d3d11screencapturesrc)")?;
        encoder_name().ok_or("no H.264 encoder (AMF, QSV, NVENC, Media Foundation)")?;
        Ok(Arc::new(Self))
    }
}

pub fn encoder_name() -> Option<&'static str> {
    ENCODERS
        .iter()
        .map(|(name, _)| *name)
        .find(|name| gst::ElementFactory::find(name).is_some())
}

impl ScreenProvider for WindowsScreen {
    fn start(&self, request: ScreenRequest) -> StartFuture {
        Box::pin(async move {
            let all = displays();
            let display = all
                .iter()
                .find(|d| d.monitor.id == request.monitor)
                .or_else(|| all.iter().find(|d| d.monitor.primary))
                .or_else(|| all.first())
                .cloned()
                .ok_or("no monitor is attached")?;
            let mut last_error = String::from("no H.264 encoder");
            for (name, tuned) in ENCODERS {
                if gst::ElementFactory::find(name).is_none() {
                    continue;
                }
                for encoder in [*tuned, *name] {
                    match start_with(&display, encoder).await {
                        Ok((frames, frame_overflow, control, width, height)) => {
                            let events = control.clipboard.watch();
                            tracing::info!("sharing {width}x{height} with {encoder}");
                            return Ok(ScreenStream {
                                width,
                                height,
                                codec: "h264".into(),
                                frames,
                                frame_overflow,
                                control: Box::new(control),
                                events: Some(events),
                                monitors: all.iter().map(|d| d.monitor.clone()).collect(),
                                monitor: display.monitor.id.clone(),
                            });
                        }
                        Err(e) => {
                            tracing::info!("capture with {encoder}: {e}");
                            last_error = e;
                        }
                    }
                }
            }
            Err(last_error)
        })
    }
}

async fn start_with(
    display: &Display,
    encoder: &str,
) -> Result<
    (
        tokio::sync::mpsc::Receiver<EncodedFrame>,
        Arc<AtomicBool>,
        Control,
        u32,
        u32,
    ),
    String,
> {
    let description = format!(
        "d3d11screencapturesrc monitor-handle={} show-cursor=true ! d3d11convert ! {encoder} \
         ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au \
         ! appsink name=sink sync=false max-buffers={} drop=false",
        display.handle, QUEUE
    );
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| format!("GStreamer pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline".to_string())?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or("no appsink")?;
    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(format!("cannot start capture: {e}"));
    }
    let (frames_tx, frames) = tokio::sync::mpsc::channel::<EncodedFrame>(QUEUE);
    let frame_overflow = Arc::new(AtomicBool::new(false));
    let (size_tx, size_rx) = tokio::sync::oneshot::channel::<(u32, u32)>();
    let puller = sink.clone();
    let overflow = frame_overflow.clone();
    std::thread::Builder::new()
        .name("casement-capture".into())
        .spawn(move || pull_frames(puller, frames_tx, overflow, size_tx))
        .map_err(|e| e.to_string())?;
    let (width, height) = match tokio::time::timeout(Duration::from_secs(10), size_rx).await {
        Ok(Ok(size)) => size,
        _ => {
            let _ = pipeline.set_state(gst::State::Null);
            return Err("no picture within 10 s".into());
        }
    };
    let control = Control {
        pipeline,
        sink,
        rect: display.rect,
        scroll: std::sync::Mutex::new((0.0, 0.0)),
        clipboard: Arc::new(Clipboard::default()),
    };
    Ok((frames, frame_overflow, control, width, height))
}

struct Control {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    /// The shared monitor on the virtual desktop, for absolute pointer positions.
    rect: RECT,
    /// Wheel remainders smaller than what is worth sending.
    scroll: std::sync::Mutex<(f64, f64)>,
    clipboard: Arc<Clipboard>,
}

// RECT is plain data; the pipeline and sink are thread-safe GObjects.
unsafe impl Send for Control {}
unsafe impl Sync for Control {}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self.clipboard
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

const CF_UNICODETEXT: u32 = 13;

/// The Windows clipboard for one shared screen: text the client sent is written, and a
/// watcher thread reports copies made on this computer.
#[derive(Default)]
struct Clipboard {
    stop: std::sync::atomic::AtomicBool,
    /// Sequence number after our own write, so the watcher does not echo it back.
    own: std::sync::atomic::AtomicU32,
}

impl Clipboard {
    fn watch(self: &Arc<Self>) -> tokio::sync::mpsc::UnboundedReceiver<ScreenEvent> {
        use std::sync::atomic::Ordering;
        use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let this = self.clone();
        let _ = std::thread::Builder::new()
            .name("casement-clipboard".into())
            .spawn(move || {
                let mut seen = unsafe { GetClipboardSequenceNumber() };
                let mut last = String::new();
                while !this.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(300));
                    let now = unsafe { GetClipboardSequenceNumber() };
                    if now == seen {
                        continue;
                    }
                    seen = now;
                    if now == this.own.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Some(text) = read_clipboard() {
                        if text != last && !text.is_empty() {
                            last = text.clone();
                            if tx.send(ScreenEvent::Clipboard(text)).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        rx
    }

    fn write(&self, text: &str) {
        use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
        if write_clipboard(text) {
            self.own.store(
                unsafe { GetClipboardSequenceNumber() },
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }
}

/// Another program may hold the clipboard for a moment: try a few times.
fn open_clipboard() -> bool {
    use windows_sys::Win32::System::DataExchange::OpenClipboard;
    for _ in 0..10 {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn read_clipboard() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 || !open_clipboard() {
            return None;
        }
        let mut text = None;
        let handle = GetClipboardData(CF_UNICODETEXT);
        if !handle.is_null() {
            let data = GlobalLock(handle) as *const u16;
            if !data.is_null() {
                let units = GlobalSize(handle) / 2;
                text = Some(crate::text::from_windows(std::slice::from_raw_parts(
                    data, units,
                )));
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
        text
    }
}

fn write_clipboard(text: &str) -> bool {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    let units = crate::text::to_windows(text);
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
        if memory.is_null() {
            return false;
        }
        let data = GlobalLock(memory) as *mut u16;
        if data.is_null() {
            GlobalFree(memory);
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), data, units.len());
        GlobalUnlock(memory);
        if !open_clipboard() {
            GlobalFree(memory);
            return false;
        }
        EmptyClipboard();
        // On success the clipboard owns the memory.
        let ok = !SetClipboardData(CF_UNICODETEXT, memory).is_null();
        if !ok {
            GlobalFree(memory);
        }
        CloseClipboard();
        ok
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

impl ScreenControl for Control {
    fn clipboard(&self, text: String) {
        let clipboard = self.clipboard.clone();
        // Opening the clipboard may wait on another program: not on the network task.
        std::thread::spawn(move || clipboard.write(&text));
    }

    fn request_keyframe(&self) {
        let event = gst_video::UpstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        self.sink.send_event(event);
    }

    fn input(&self, event: Input) {
        match event.kind {
            Some(input::Kind::Motion(m)) => {
                // Absolute positions are 0..=65535 across the whole virtual desktop.
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
                    )
                };
                let x = self.rect.left as f64 + m.x - vx as f64;
                let y = self.rect.top as f64 + m.y - vy as f64;
                let nx = (x * 65535.0 / (vw - 1) as f64).round() as i32;
                let ny = (y * 65535.0 / (vh - 1) as f64).round() as i32;
                send(&[mouse(
                    nx,
                    ny,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )]);
            }
            Some(input::Kind::Button(b)) => {
                let (down, up, data) = match b.button {
                    0x110 => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
                    0x111 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
                    0x112 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
                    0x113 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
                    0x114 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
                    _ => return,
                };
                send(&[mouse(0, 0, data, if b.pressed { down } else { up })]);
            }
            Some(input::Kind::Scroll(s)) => {
                let mut rest = self.scroll.lock().unwrap();
                // Down and right are positive from the client; Windows' wheel is positive up.
                rest.0 += s.dx * WHEEL_DELTA;
                rest.1 -= s.dy * WHEEL_DELTA;
                let (x, y) = (rest.0.trunc(), rest.1.trunc());
                rest.0 -= x;
                rest.1 -= y;
                let mut inputs = Vec::new();
                if y != 0.0 {
                    inputs.push(mouse(0, 0, y as i32, MOUSEEVENTF_WHEEL));
                }
                if x != 0.0 {
                    inputs.push(mouse(0, 0, x as i32, MOUSEEVENTF_HWHEEL));
                }
                send(&inputs);
            }
            Some(input::Kind::Key(k)) => {
                let Some((scan, extended)) = scancode(k.code) else {
                    return;
                };
                let mut flags = KEYEVENTF_SCANCODE;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !k.pressed {
                    flags |= KEYEVENTF_KEYUP;
                }
                send(&[INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: 0,
                            wScan: scan,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }]);
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_scancodes() {
        assert_eq!(scancode(30), Some((0x1E, false))); // A
        assert_eq!(scancode(28), Some((0x1C, false))); // Enter
        assert_eq!(scancode(96), Some((0x1C, true))); // keypad Enter
        assert_eq!(scancode(103), Some((0x48, true))); // Up
        assert_eq!(scancode(240), None);
    }
}
