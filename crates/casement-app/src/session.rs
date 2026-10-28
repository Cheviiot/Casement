//! One connection: the flow from address to picture, the remote screen and input.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use adw::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use casement_net::client::{self, ClientInfo, FileSender, Handshake, InputSender};
use casement_net::{Fingerprint, NetError};
use casement_proto::{input, Input, Key, PointerButton, PointerMotion, Scroll};

use crate::core::{describe, run, runtime};
use crate::pages;
use crate::store::Computer;
use crate::window::Window;

/// Linux evdev button codes for GDK buttons 1, 2, 3, 8, 9.
fn evdev_button(button: u32) -> Option<u32> {
    match button {
        1 => Some(0x110),
        2 => Some(0x112),
        3 => Some(0x111),
        8 => Some(0x113),
        9 => Some(0x114),
        _ => None,
    }
}

/// Decoding and colour conversion up to the paintable sink. With VA-API the colours are
/// converted on the GPU too (`vapostproc`): measured with `casement-cli decode-bench` on
/// GEM12's 1080p stream, 0.4 ms of CPU per frame instead of 4 ms with `videoconvert`.
fn decoder() -> &'static str {
    let has = |name: &str| gst::ElementFactory::find(name).is_some();
    if has("vah264dec") && has("vapostproc") {
        "vah264dec ! vapostproc ! video/x-raw,format=BGRA"
    } else if has("vah264dec") {
        "vah264dec ! videoconvert"
    } else if has("d3d11h264dec") && has("d3d11convert") && has("d3d11download") {
        // Windows: Direct3D 11 decoding, colour conversion and scaling to the size the
        // picture is shown at (the caps of `fit`, see Remote::refit).
        "d3d11h264dec name=decoder ! d3d11convert add-borders=false \
         ! capsfilter name=fit caps=video/x-raw(memory:D3D11Memory),format=BGRA ! d3d11download"
    } else if has("d3d11h264dec") {
        "d3d11h264dec ! videoconvert"
    } else if has("avdec_h264") {
        "avdec_h264 ! videoconvert"
    } else if has("openh264dec") {
        "openh264dec ! videoconvert"
    } else {
        "decodebin ! videoconvert"
    }
}

/// The picture of the remote screen and the input that goes back.
struct Remote {
    picture: gtk::Picture,
    width: Cell<u32>,
    height: Cell<u32>,
    /// The decoded picture's size, when the pipeline scales it to fit (`fit`): width in
    /// the high half, height in the low one.
    source: Arc<AtomicU64>,
    fit: RefCell<Option<gst::Element>>,
    /// The size asked of `fit`, and a new one waiting for the window to stop resizing.
    fitted: Cell<(i32, i32)>,
    pending: Cell<Option<((i32, i32), i64)>>,
    input: RefCell<Option<InputSender>>,
    pressed: RefCell<HashSet<u32>>,
    buttons: RefCell<HashSet<u32>>,
    pointer: Cell<Option<(f64, f64)>>,
    hidden_cursor: gtk::gdk::Cursor,
    cursor_hidden: Cell<bool>,
}

impl Remote {
    fn new() -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .focusable(true)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["remote-screen"])
            .build();
        // An explicit alpha texture also works on Windows, independently of whether
        // the cursor theme implements the CSS name "none".
        let transparent = gtk::gdk::MemoryTexture::new(
            1,
            1,
            gtk::gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_static(&[0, 0, 0, 0]),
            4,
        );
        let remote = Rc::new(Self {
            picture,
            width: Cell::new(0),
            height: Cell::new(0),
            source: Arc::new(AtomicU64::new(0)),
            fit: RefCell::new(None),
            fitted: Cell::new((0, 0)),
            pending: Cell::new(None),
            input: RefCell::new(None),
            pressed: RefCell::new(HashSet::new()),
            buttons: RefCell::new(HashSet::new()),
            pointer: Cell::new(None),
            hidden_cursor: gtk::gdk::Cursor::from_texture(&transparent, 0, 0, None),
            cursor_hidden: Cell::new(false),
        });
        remote.attach_controllers();
        let weak = Rc::downgrade(&remote);
        remote.picture.connect_paintable_notify(move |_| {
            if let Some(remote) = weak.upgrade() {
                remote.sync_cursor();
            }
        });
        let weak = Rc::downgrade(&remote);
        remote.picture.add_tick_callback(move |_, clock| {
            if let Some(remote) = weak.upgrade() {
                // Covers a first frame, monitor switch and resize while the pointer
                // is stationary, as well as ordinary pointer events.
                remote.sync_cursor();
                remote.refit(clock.frame_time());
            }
            glib::ControlFlow::Continue
        });
        remote
    }

    fn source_size(&self) -> Option<(u32, u32)> {
        let packed = self.source.load(Ordering::Relaxed);
        let (w, h) = ((packed >> 32) as u32, packed as u32);
        (w > 0 && h > 0).then_some((w, h))
    }

    /// Asks the pipeline for the picture at the size it is shown at, in device pixels,
    /// once the size has held for 100 ms (not on every step of a window resize).
    fn refit(&self, now: i64) {
        let Some(fit) = self.fit.borrow().clone() else {
            return;
        };
        let Some((sw, sh)) = self.source_size() else {
            return;
        };
        let scale = self.picture.scale_factor() as f64;
        let (ww, wh) = (
            self.picture.width() as f64 * scale,
            self.picture.height() as f64 * scale,
        );
        if ww < 2.0 || wh < 2.0 {
            return;
        }
        let ratio = (ww / sw as f64).min(wh / sh as f64);
        let even = |v: f64| ((v.round() as i32) & !1).max(2);
        let size = (even(sw as f64 * ratio), even(sh as f64 * ratio));
        if size == self.fitted.get() {
            self.pending.set(None);
            return;
        }
        match self.pending.get() {
            Some((waiting, since)) if waiting == size => {
                if now - since < 100_000 {
                    return;
                }
            }
            _ => {
                self.pending.set(Some((size, now)));
                return;
            }
        }
        self.pending.set(None);
        self.fitted.set(size);
        let caps = gst::Caps::builder("video/x-raw")
            .features(["memory:D3D11Memory"])
            .field("format", "BGRA")
            .field("width", size.0)
            .field("height", size.1)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        fit.set_property("caps", caps);
    }

    fn send(&self, kind: input::Kind) {
        if let Some(sender) = self.input.borrow().as_ref() {
            let _ = sender.send(&Input { kind: Some(kind) });
        }
    }

    fn sync_cursor(&self) {
        let inside = self
            .pointer
            .get()
            .is_none_or(|point| self.viewport().is_some_and(|v| v.contains(point)));
        let hide = inside && self.picture.paintable().is_some();
        if self.cursor_hidden.replace(hide) != hide {
            self.picture.set_cursor(if hide {
                Some(&self.hidden_cursor)
            } else {
                None
            });
        }
    }

    fn viewport(&self) -> Option<crate::viewport::Viewport> {
        let source = if let Some((w, h)) = self.source_size() {
            (w as f64, h as f64)
        } else if let Some(paintable) = self.picture.paintable() {
            let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
            if w > 0 && h > 0 {
                (w as f64, h as f64)
            } else {
                (self.width.get() as f64, self.height.get() as f64)
            }
        } else {
            (self.width.get() as f64, self.height.get() as f64)
        };
        crate::viewport::Viewport::new(
            (self.picture.width() as f64, self.picture.height() as f64),
            source,
        )
    }

    fn to_remote(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        self.viewport()?.remote((x, y))
    }

    fn release_keys(&self) {
        let keys: Vec<u32> = self.pressed.borrow_mut().drain().collect();
        for code in keys {
            self.send(input::Kind::Key(Key {
                code,
                pressed: false,
            }));
        }
        let buttons: Vec<_> = self.buttons.borrow_mut().drain().collect();
        for button in buttons {
            self.send(input::Kind::Button(PointerButton {
                button,
                pressed: false,
            }));
        }
    }

    fn attach_controllers(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        let this = Rc::downgrade(self);
        motion.connect_enter(move |_, x, y| {
            if let Some(this) = this.upgrade() {
                this.pointer.set(Some((x, y)));
                this.sync_cursor();
                if let Some((x, y)) = this.to_remote(x, y) {
                    this.send(input::Kind::Motion(PointerMotion { x, y }));
                }
            }
        });
        let this = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            if let Some(this) = this.upgrade() {
                this.pointer.set(Some((x, y)));
                this.sync_cursor();
                if let Some((x, y)) = this.to_remote(x, y) {
                    this.send(input::Kind::Motion(PointerMotion { x, y }));
                }
            }
        });
        let this = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(this) = this.upgrade() {
                this.pointer.set(None);
            }
        });
        self.picture.add_controller(motion);

        let click = gtk::GestureClick::builder().button(0).build();
        let this = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(this) = this.upgrade() else { return };
            this.picture.grab_focus();
            if let Some((x, y)) = this.to_remote(x, y) {
                this.send(input::Kind::Motion(PointerMotion { x, y }));
            }
            if let Some(button) = evdev_button(gesture.current_button()) {
                this.buttons.borrow_mut().insert(button);
                this.send(input::Kind::Button(PointerButton {
                    button,
                    pressed: true,
                }));
            }
        });
        let this = Rc::downgrade(self);
        click.connect_released(move |gesture, _, _, _| {
            let Some(this) = this.upgrade() else { return };
            if let Some(button) = evdev_button(gesture.current_button()) {
                this.buttons.borrow_mut().remove(&button);
                this.send(input::Kind::Button(PointerButton {
                    button,
                    pressed: false,
                }));
            }
        });
        self.picture.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let this = Rc::downgrade(self);
        scroll.connect_scroll(move |_, dx, dy| {
            if let Some(this) = this.upgrade() {
                this.send(input::Kind::Scroll(Scroll { dx, dy }));
            }
            glib::Propagation::Stop
        });
        self.picture.add_controller(scroll);

        // Physical keys: GDK hardware keycodes are evdev codes + 8 on Linux.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, _, keycode, _| {
            let Some(this) = this.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if this.input.borrow().is_none() || keycode < 8 {
                return glib::Propagation::Proceed;
            }
            let code = keycode - 8;
            this.pressed.borrow_mut().insert(code);
            this.send(input::Kind::Key(Key {
                code,
                pressed: true,
            }));
            glib::Propagation::Stop
        });
        let this = Rc::downgrade(self);
        keys.connect_key_released(move |_, _, keycode, _| {
            let Some(this) = this.upgrade() else { return };
            if keycode >= 8 {
                let code = keycode - 8;
                this.pressed.borrow_mut().remove(&code);
                this.send(input::Kind::Key(Key {
                    code,
                    pressed: false,
                }));
            }
        });
        self.picture.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let this = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(this) = this.upgrade() {
                this.release_keys();
            }
        });
        self.picture.add_controller(focus);
    }
}

pub struct SessionPage {
    pub page: adw::NavigationPage,
    stack: gtk::Stack,
    progress: gtk::Label,
    result: adw::StatusPage,
    retry: gtk::Button,
    remote: Rc<Remote>,
    /// Ends the network side of the session when dropped or sent.
    stop: RefCell<Option<tokio::sync::oneshot::Sender<()>>>,
    /// Cancels the GTK-side connection flow as soon as its page is left or restarted.
    connecting: RefCell<Option<tokio::sync::oneshot::Sender<()>>>,
    generation: Cell<u64>,
    /// Asks the host for a keyframe (after a decoder error).
    keyframe: RefCell<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
    files_wired: Cell<bool>,
    /// Browsing and downloading the host's files while connected.
    browse_files: gtk::Button,
    remote_files: RefCell<Option<FileSender>>,
    host_name: RefCell<String>,
    /// The user left the page: no more automatic reconnects.
    gone: Cell<bool>,
    /// Automatic reconnect attempt in progress (0 = none).
    reconnects: Cell<u32>,
    /// New window sizes for a virtual monitor.
    resize_out: RefCell<Option<tokio::sync::mpsc::UnboundedSender<(u32, u32)>>>,
    resize_watch: RefCell<Option<glib::SourceId>>,
    /// Local clipboard text for the host.
    clipboard_out: RefCell<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    clipboard_watch: RefCell<Option<glib::SignalHandlerId>>,
    pipeline: RefCell<Option<gst::Pipeline>>,
    bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    toolbar: adw::ToolbarView,
    controls: gtk::Box,
    disconnect: gtk::Button,
    window: adw::ApplicationWindow,
    fullscreen: gtk::Button,
    /// The host's sound, when it sends any; the toggle mutes it on this side.
    sound: gtk::ToggleButton,
    sound_pipeline: RefCell<Option<gst::Pipeline>>,
    /// The host's monitor to show ("" = its primary one) and the button to pick another.
    monitor: RefCell<String>,
    monitors_button: gtk::Button,
    monitors_wired: Cell<bool>,
    /// The host's monitors as last reported, for the monitor page.
    monitors: RefCell<(Vec<casement_net::screen::Monitor>, String)>,
    /// The question shown on the page, answered by its buttons; leaving the page answers
    /// "no".
    pending: RefCell<Option<tokio::sync::oneshot::Sender<Option<String>>>>,
}

/// A question asked on the session page itself while connecting.
struct Question<'a> {
    icon: &'a str,
    title: &'a str,
    text: &'a str,
    /// Shown large, for comparing with the other computer.
    fingerprint: Option<&'a str>,
    /// Ask for a password; the answer carries it.
    password: bool,
    yes: &'a str,
    /// "suggested-action" or "destructive-action".
    yes_style: &'a str,
    no: &'a str,
}

impl SessionPage {
    fn new(computer: &Computer, window: adw::ApplicationWindow) -> Rc<Self> {
        let spinner = adw::Spinner::builder()
            .width_request(48)
            .height_request(48)
            .build();
        let progress = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-4"])
            .build();
        let cancel = gtk::Button::builder()
            .label("Отмена")
            .halign(gtk::Align::Center)
            .css_classes(["primary-action"])
            .build();
        let busy = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        busy.append(&spinner);
        busy.append(&progress);
        busy.append(&cancel);

        let retry = gtk::Button::builder()
            .label("Подключиться снова")
            .halign(gtk::Align::Center)
            .css_classes(["primary-action", "suggested-action"])
            .build();
        let result = adw::StatusPage::builder().child(&retry).build();
        let remote = Remote::new();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&busy, Some("progress"));
        stack.add_named(&result, Some("result"));
        stack.add_named(&remote.picture, Some("screen"));

        let header = adw::HeaderBar::new();
        let fullscreen = gtk::Button::builder()
            .icon_name("view-fullscreen-symbolic")
            .tooltip_text("Во весь экран")
            .visible(false)
            .build();
        let disconnect = gtk::Button::builder()
            .icon_name("network-offline-symbolic")
            .tooltip_text("Завершить сеанс")
            .visible(false)
            .build();
        let browse_files = gtk::Button::builder()
            .icon_name("folder-remote-symbolic")
            .tooltip_text("Файлы")
            .visible(false)
            .build();
        header.pack_end(&disconnect);
        let sound = gtk::ToggleButton::builder()
            .icon_name("audio-volume-high-symbolic")
            .tooltip_text("Выключить звук того компьютера")
            .visible(false)
            .build();
        let monitors_button = gtk::Button::builder()
            .icon_name("video-display-symbolic")
            .tooltip_text("Монитор того компьютера")
            .visible(false)
            .build();
        let controls = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .css_classes(["session-tools"])
            .visible(false)
            .build();
        controls.append(&browse_files);
        controls.append(&monitors_button);
        controls.append(&sound);
        controls.append(&fullscreen);
        let view = adw::ToolbarView::builder()
            .top_bar_style(adw::ToolbarStyle::Raised)
            .build();
        view.set_content(Some(&stack));
        view.add_top_bar(&header);
        view.add_bottom_bar(&controls);
        let page = adw::NavigationPage::builder()
            .child(&view)
            .title(computer.title())
            .tag("session")
            .build();

        let this = Rc::new(Self {
            page,
            stack,
            progress,
            result,
            retry,
            remote,
            stop: RefCell::new(None),
            connecting: RefCell::new(None),
            generation: Cell::new(0),
            keyframe: RefCell::new(None),
            clipboard_out: RefCell::new(None),
            resize_out: RefCell::new(None),
            files_wired: Cell::new(false),
            browse_files,
            remote_files: RefCell::new(None),
            host_name: RefCell::new(computer.title()),
            gone: Cell::new(false),
            reconnects: Cell::new(0),
            resize_watch: RefCell::new(None),
            clipboard_watch: RefCell::new(None),
            pipeline: RefCell::new(None),
            bus_watch: RefCell::new(None),
            toolbar: view,
            controls,
            disconnect: disconnect.clone(),
            window,
            fullscreen,
            sound,
            sound_pipeline: RefCell::new(None),
            monitor: RefCell::new(String::new()),
            monitors_button,
            monitors_wired: Cell::new(false),
            monitors: RefCell::new((Vec::new(), String::new())),
            pending: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        this.sound.connect_toggled(move |button| {
            let muted = button.is_active();
            button.set_icon_name(if muted {
                "audio-volume-muted-symbolic"
            } else {
                "audio-volume-high-symbolic"
            });
            button.set_tooltip_text(Some(if muted {
                "Включить звук того компьютера"
            } else {
                "Выключить звук того компьютера"
            }));
            if let Some(this) = weak.upgrade() {
                this.apply_mute();
            }
        });
        let weak = Rc::downgrade(&this);
        this.fullscreen.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.set_fullscreen(!this.window.is_fullscreen());
            }
        });
        let weak = Rc::downgrade(&this);
        disconnect.connect_clicked(move |button| {
            if let Some(this) = weak.upgrade() {
                this.shutdown();
                if let Some(nav) = button.ancestor(adw::NavigationView::static_type()) {
                    nav.downcast::<adw::NavigationView>().unwrap().pop();
                }
            }
        });
        // In fullscreen the bar hides; touching the top edge brings it back.
        let edge = gtk::EventControllerMotion::new();
        edge.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this);
        edge.connect_motion(move |_, _, y| {
            if let Some(this) = weak.upgrade() {
                if this.window.is_fullscreen() {
                    if y <= 2.0 {
                        this.toolbar.set_reveal_top_bars(true);
                        this.toolbar.set_reveal_bottom_bars(true);
                    } else if y > 80.0 {
                        this.toolbar.set_reveal_top_bars(false);
                        this.toolbar
                            .set_reveal_bottom_bars(y >= (this.toolbar.height() - 60) as f64);
                    }
                }
            }
        });
        this.toolbar.add_controller(edge);
        let weak = Rc::downgrade(&this);
        cancel.connect_clicked(move |button| {
            if let Some(this) = weak.upgrade() {
                this.shutdown();
                if let Some(nav) = button.ancestor(adw::NavigationView::static_type()) {
                    nav.downcast::<adw::NavigationView>().unwrap().pop();
                }
            }
        });
        this
    }

    /// The page left the navigation stack (back, "Отключиться", "Отмена"): end the session.
    /// Pages pushed on top of it (files, monitors) only cover it and keep it running.
    fn leave(&self) {
        self.gone.set(true);
        self.pending.take();
        self.shutdown();
    }

    /// Shows `question` in place of the progress and waits: Some(text) for yes (the
    /// password, or "" without one), None for no or when the page is left.
    async fn ask(self: &Rc<Self>, question: Question<'_>) -> Option<String> {
        let (hero, _, _) = pages::hero(question.icon, question.title, question.text);
        hero.set_margin_top(24);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&hero);
        if let Some(fingerprint) = question.fingerprint {
            column.append(&pages::fingerprint_label(fingerprint));
        }
        let entry = question.password.then(|| {
            let row = adw::PasswordEntryRow::builder().title("Пароль").build();
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            list.append(&row);
            column.append(&list);
            row
        });
        let yes = pages::pill(question.yes, question.yes_style);
        let no = pages::pill(question.no, "flat");
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        buttons.append(&yes);
        buttons.append(&no);
        column.append(&buttons);
        let clamp = adw::Clamp::builder()
            .maximum_size(480)
            .child(&column)
            .valign(gtk::Align::Center)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        if let Some(old) = self.stack.child_by_name("question") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&scrolled, Some("question"));
        self.stack.set_visible_child_name("question");

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.replace(Some(tx));
        let weak = Rc::downgrade(self);
        let entry2 = entry.clone();
        yes.connect_clicked(move |button| {
            button.set_sensitive(false);
            let text = entry2
                .as_ref()
                .map(|e| e.text().to_string())
                .unwrap_or_default();
            if let Some(e) = &entry2 {
                e.set_text("");
            }
            if let Some(tx) = weak.upgrade().and_then(|page| page.pending.take()) {
                let _ = tx.send(Some(text));
            }
        });
        let weak = Rc::downgrade(self);
        no.connect_clicked(move |_| {
            if let Some(tx) = weak.upgrade().and_then(|page| page.pending.take()) {
                let _ = tx.send(None);
            }
        });
        if let Some(entry) = &entry {
            yes.set_sensitive(false);
            let button = yes.clone();
            entry.connect_changed(move |entry| {
                button.set_sensitive(
                    !entry.text().is_empty()
                        && entry.text().len() <= casement_net::auth::MAX_PASSWORD_BYTES,
                );
            });
            let y = yes.clone();
            entry.connect_entry_activated(move |_| {
                if y.is_sensitive() {
                    y.emit_clicked();
                }
            });
            entry.grab_focus();
        } else {
            if question.yes_style == "destructive-action" {
                no.grab_focus();
            } else {
                yes.grab_focus();
            }
        }
        rx.await.ok().flatten()
    }

    fn show_progress(&self, text: &str) {
        self.progress.set_label(text);
        self.stack.set_visible_child_name("progress");
    }

    fn show_result(&self, icon: &str, heading: &str, message: Option<&str>, retry: bool) {
        self.result.set_icon_name(Some(icon));
        self.result.set_title(heading);
        self.result
            .set_description(message.map(glib::markup_escape_text).as_deref());
        self.retry.set_visible(retry);
        self.stack.set_visible_child_name("result");
    }

    /// Files live in one page; dropping onto the screen opens it with those files queued.
    fn enable_files(self: &Rc<Self>, window: &Window) {
        if self.files_wired.replace(true) {
            return;
        }
        let (weak, owner) = (Rc::downgrade(self), window.clone());
        self.browse_files.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.open_remote_files(&owner);
            }
        });
        let drop = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );
        let (weak, owner) = (Rc::downgrade(self), window.clone());
        drop.connect_drop(move |_, value, _, _| {
            let (Some(this), Ok(list)) = (weak.upgrade(), value.get::<gtk::gdk::FileList>()) else {
                return false;
            };
            let paths: Vec<_> = list
                .files()
                .iter()
                .filter_map(|file| file.path())
                .filter(|path| path.is_file())
                .collect();
            if paths.is_empty() {
                return false;
            }
            this.open_files_with(&owner, paths);
            true
        });
        self.remote.picture.add_controller(drop);
    }

    fn open_remote_files(self: &Rc<Self>, window: &Window) {
        self.open_files_with(window, Vec::new());
    }

    fn open_files_with(self: &Rc<Self>, window: &Window, uploads: Vec<std::path::PathBuf>) {
        if let Some(files) = self.remote_files.borrow().clone() {
            self.remote.release_keys();
            crate::remote_files::open(window, files, &self.host_name.borrow(), uploads);
        }
    }

    /// A virtual monitor follows the window: when the picture area keeps a new size for a
    /// moment, ask the host for it.
    fn follow_window_size(&self, width: u32, height: u32) {
        let Some(out) = self.resize_out.borrow().clone() else {
            return;
        };
        let picture = self.remote.picture.clone();
        let sent = std::cell::Cell::new((width, height));
        let seen = std::cell::Cell::new((width, height));
        let id = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            let now = (
                picture.width().max(0) as u32,
                picture.height().max(0) as u32,
            );
            if now.0 >= 640 && now.1 >= 480 && now == seen.get() && now != sent.get() {
                sent.set(now);
                // Never Break here: shutdown() removes this source and must find it.
                let _ = out.send(now);
            }
            seen.set(now);
            glib::ControlFlow::Continue
        });
        self.resize_watch.replace(Some(id));
    }

    /// Text clipboard both ways while the screen is shown. Our own writes come back as
    /// "changed" with a local owner and are not echoed.
    fn share_clipboard(&self, incoming: async_channel::Receiver<String>) {
        let clipboard = self.remote.picture.clipboard();
        let Some(out) = self.clipboard_out.borrow().clone() else {
            return;
        };
        let last = Rc::new(RefCell::new(String::new()));
        let send_current = {
            let (out, last) = (out.clone(), last.clone());
            move |clipboard: &gtk::gdk::Clipboard| {
                if clipboard.is_local() {
                    return;
                }
                let (out, last) = (out.clone(), last.clone());
                let clipboard = clipboard.clone();
                glib::spawn_future_local(async move {
                    if let Ok(Some(text)) = clipboard.read_text_future().await {
                        let text = casement_net::client::clip_text(&text).to_string();
                        if !text.is_empty() && *last.borrow() != text {
                            *last.borrow_mut() = text.clone();
                            let _ = out.send(text);
                        }
                    }
                });
            }
        };
        send_current(&clipboard);
        let id = clipboard.connect_changed(move |clipboard| send_current(clipboard));
        self.clipboard_watch.replace(Some(id));
        glib::spawn_future_local(async move {
            while let Ok(text) = incoming.recv().await {
                *last.borrow_mut() = text.clone();
                clipboard.set_text(&text);
            }
        });
    }

    fn set_fullscreen(&self, on: bool) {
        let toplevel = self
            .window
            .surface()
            .and_then(|s| s.downcast::<gtk::gdk::Toplevel>().ok());
        if on {
            self.window.fullscreen();
            self.toolbar.set_reveal_top_bars(false);
            self.toolbar.set_reveal_bottom_bars(false);
            self.toolbar.set_extend_content_to_top_edge(true);
            self.toolbar.set_extend_content_to_bottom_edge(true);
            // Alt+Tab, Super and friends go to the remote computer while in fullscreen.
            if let Some(toplevel) = &toplevel {
                toplevel.inhibit_system_shortcuts(None::<&gtk::gdk::Event>);
            }
            self.remote.picture.grab_focus();
        } else {
            self.window.unfullscreen();
            self.toolbar.set_reveal_top_bars(true);
            self.toolbar.set_reveal_bottom_bars(true);
            self.toolbar.set_extend_content_to_top_edge(false);
            self.toolbar.set_extend_content_to_bottom_edge(false);
            if let Some(toplevel) = &toplevel {
                toplevel.restore_system_shortcuts();
            }
        }
        self.fullscreen.set_icon_name(if on {
            "view-restore-symbolic"
        } else {
            "view-fullscreen-symbolic"
        });
        self.fullscreen.set_tooltip_text(Some(if on {
            "Выйти из полноэкранного режима"
        } else {
            "Во весь экран"
        }));
    }

    fn shutdown(&self) {
        if self.window.is_fullscreen() {
            self.set_fullscreen(false);
        }
        self.fullscreen.set_visible(false);
        self.controls.set_visible(false);
        self.disconnect.set_visible(false);
        self.connecting.take();
        self.bus_watch.replace(None);
        self.keyframe.replace(None);
        self.clipboard_out.replace(None);
        self.resize_out.replace(None);
        self.remote_files.replace(None);
        self.browse_files.set_visible(false);
        if let Some(id) = self.resize_watch.take() {
            id.remove();
        }
        if let Some(id) = self.clipboard_watch.take() {
            self.remote.picture.clipboard().disconnect(id);
        }
        self.remote.release_keys();
        self.remote.input.replace(None);
        self.remote
            .picture
            .set_paintable(None::<&gtk::gdk::Paintable>);
        self.remote.sync_cursor();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.remote.fit.take();
        self.remote.source.store(0, Ordering::Relaxed);
        if let Some(pipeline) = self.sound_pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.sound.set_visible(false);
        self.monitors_button.set_visible(false);
    }

    /// With more than one monitor on the host, a button that opens the monitor page.
    fn offer_monitors(
        self: &Rc<Self>,
        window: &Window,
        computer: &Computer,
        monitors: &[casement_net::screen::Monitor],
        shown: &str,
    ) {
        self.monitors
            .replace((monitors.to_vec(), shown.to_string()));
        if monitors.len() < 2 || computer.virtual_monitor {
            self.monitors_button.set_visible(false);
            return;
        }
        self.monitors_button.set_visible(true);
        if !self.monitors_wired.replace(true) {
            let (weak, window, id) = (Rc::downgrade(self), window.clone(), computer.id.clone());
            let fallback = computer.clone();
            self.monitors_button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    let computer = window.store().find(&id).unwrap_or_else(|| fallback.clone());
                    page.open_monitors(&window, &computer);
                }
            });
        }
        match std::env::var("CASEMENT_OPEN").as_deref() {
            Ok("connect-monitors") => self.open_monitors(window, computer),
            // Once: pick the first monitor not shown, as on the monitor page.
            Ok("connect-switch-monitor") if self.monitor.borrow().is_empty() => {
                if let Some(other) = monitors.iter().find(|m| m.id != shown) {
                    self.switch_monitor(window, computer, other.id.clone());
                }
            }
            _ => {}
        }
    }

    /// The host's monitors as a page; picking one starts the screen again on it.
    fn open_monitors(self: &Rc<Self>, window: &Window, computer: &Computer) {
        let (monitors, shown) = self.monitors.borrow().clone();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        let mut first: Option<gtk::CheckButton> = None;
        for monitor in &monitors {
            // Mutter's virtual monitors (another client's, or a leftover) have no real name.
            let name = if monitor.id.starts_with("Meta-") {
                "Виртуальный экран"
            } else {
                monitor.name.as_str()
            };
            let mut details = format!("{}×{} · {}", monitor.width, monitor.height, monitor.id);
            if monitor.primary {
                details.push_str(" · основной");
            }
            let check = gtk::CheckButton::new();
            check.set_group(first.as_ref());
            first.get_or_insert_with(|| check.clone());
            check.set_active(monitor.id == shown);
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(name).as_str())
                .subtitle(details)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            let (weak, window2, computer2, id) = (
                Rc::downgrade(self),
                window.clone(),
                computer.clone(),
                monitor.id.clone(),
            );
            let current = shown.clone();
            check.connect_toggled(move |check| {
                if !check.is_active() || id == current {
                    return;
                }
                if let Some(page) = weak.upgrade() {
                    window2.navigation().pop();
                    page.switch_monitor(&window2, &computer2, id.clone());
                }
            });
            group.add(&row);
        }
        page.add(&group);
        window
            .navigation()
            .push(&pages::preferences("Монитор", "monitors", &page, None));
    }

    fn switch_monitor(self: &Rc<Self>, window: &Window, computer: &Computer, id: String) {
        self.monitor.replace(id);
        self.shutdown();
        self.show_progress("Переключение на другой монитор…");
        glib::spawn_future_local(flow(window.clone(), self.clone(), computer.clone()));
    }

    fn apply_mute(&self) {
        let volume = self
            .sound_pipeline
            .borrow()
            .as_ref()
            .and_then(|p| p.by_name("volume"));
        if let Some(volume) = volume {
            volume.set_property("mute", self.sound.is_active());
        }
    }

    /// Plays the host's Opus packets; returns the appsrc to push them into. The sink does
    /// not wait for timestamps: late sound is played at once rather than piling up.
    fn start_sound(
        &self,
        format: &casement_net::audio::AudioFormat,
    ) -> Result<gst_app::AppSrc, String> {
        // A short output buffer and a leaky queue keep the sound close to the picture:
        // when packets bunch up, the oldest are dropped instead of adding delay. Without a
        // reachable Pulse server, any sink the system offers.
        let mut sinks = vec!["autoaudiosink sync=false"];
        if gst::ElementFactory::find("pulsesink").is_some() {
            sinks.insert(
                0,
                "pulsesink buffer-time=80000 latency-time=10000 sync=false",
            );
        }
        let mut last_error = String::new();
        let mut started = None;
        // Windows: the packets are decoded with libopus before the appsrc (no opusdec in
        // Fedora's mingw GStreamer), so it takes raw samples.
        let (caps, decode) = if cfg!(windows) {
            ("audio/x-raw,format=F32LE,layout=interleaved", "")
        } else {
            (
                "audio/x-opus,channel-mapping-family=0",
                "! opusdec plc=true",
            )
        };
        for sink in sinks {
            let description = format!(
                "appsrc name=src is-live=true format=time do-timestamp=true \
                 caps={caps},rate={},channels={} \
                 ! queue leaky=downstream max-size-buffers=0 max-size-bytes=0 \
                   max-size-time=60000000 \
                 {decode} ! audioconvert ! audioresample ! volume name=volume ! {sink}",
                format.rate, format.channels
            );
            let pipeline = gst::parse::launch(&description)
                .map_err(|e| e.to_string())?
                .downcast::<gst::Pipeline>()
                .map_err(|_| "not a pipeline".to_string())?;
            match pipeline.set_state(gst::State::Playing) {
                Ok(_) => {
                    started = Some(pipeline);
                    break;
                }
                Err(e) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    last_error = format!("{sink}: {e}");
                }
            }
        }
        let pipeline = started.ok_or(last_error)?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or("no appsrc")?;
        self.sound_pipeline.replace(Some(pipeline));
        self.apply_mute();
        self.sound.set_visible(true);
        Ok(src)
    }

    /// When the pipeline scales to fit (`fit`), follows the decoded size, which pointer
    /// positions are mapped to.
    fn watch_fit(&self, pipeline: &gst::Pipeline) {
        let remote = &self.remote;
        remote.source.store(0, Ordering::Relaxed);
        remote.fitted.set((0, 0));
        remote.pending.set(None);
        let fit = pipeline.by_name("fit");
        let pad = pipeline
            .by_name("decoder")
            .and_then(|decoder| decoder.static_pad("src"));
        if let (Some(_), Some(pad)) = (&fit, pad) {
            let source = remote.source.clone();
            pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                if let Some(gst::PadProbeData::Event(event)) = &info.data {
                    if let gst::EventView::Caps(caps) = event.view() {
                        if let Some(s) = caps.caps().structure(0) {
                            let w = s.get::<i32>("width").unwrap_or(0).max(0) as u64;
                            let h = s.get::<i32>("height").unwrap_or(0).max(0) as u64;
                            source.store(w << 32 | h, Ordering::Relaxed);
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        }
        remote.fit.replace(fit);
    }

    /// Decoder pipeline whose sink paints into the picture; returns its appsrc.
    fn start_decoder(&self) -> Result<gst_app::AppSrc, String> {
        // $CASEMENT_DECODER replaces the decoding part, for comparing pipelines.
        let decoding = std::env::var("CASEMENT_DECODER").unwrap_or_else(|_| decoder().into());
        let description = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true max-buffers=3 max-bytes=0 \
             caps=video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse ! {decoding} ! gtk4paintablesink name=sink sync=false"
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;
        let sink = pipeline.by_name("sink").ok_or("no sink")?;
        let paintable = sink.property::<gtk::gdk::Paintable>("paintable");
        self.remote.picture.set_paintable(Some(&paintable));
        self.watch_fit(&pipeline);
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or("no appsrc")?;
        // A decoder error (lost or damaged frames) restarts decoding from a fresh keyframe.
        let keyframe = self.keyframe.borrow().clone();
        let restart = pipeline.downgrade();
        let watch = pipeline
            .bus()
            .ok_or("no bus")?
            .add_watch_local(move |_, message| {
                if let gst::MessageView::Error(err) = message.view() {
                    glib::g_warning!("Casement", "decoder: {}", err.error());
                    if let Some(pipeline) = restart.upgrade() {
                        let _ = pipeline.set_state(gst::State::Null);
                        let _ = pipeline.set_state(gst::State::Playing);
                    }
                    if let Some(keyframe) = &keyframe {
                        let _ = keyframe.send(());
                    }
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| e.to_string())?;
        self.bus_watch.replace(Some(watch));
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| e.to_string())?;
        self.pipeline.replace(Some(pipeline));
        self.fullscreen.set_visible(true);
        Ok(src)
    }
}

/// The trusted key changed: maybe a reinstall, maybe someone in the middle.
async fn ask_key_changed(page: &Rc<SessionPage>, title: &str, fingerprint: &str) -> Option<String> {
    page.page.set_title(title);
    page.ask(Question {
        icon: "dialog-warning-symbolic",
        title: "Ключ компьютера изменился",
        text: "",
        fingerprint: Some(fingerprint),
        password: false,
        yes: "Доверять новому ключу",
        yes_style: "destructive-action",
        no: "Не подключаться",
    })
    .await
}

async fn ask_password(page: &Rc<SessionPage>, name: &str, retry: bool) -> Option<String> {
    page.page.set_title(name);
    page.ask(Question {
        icon: if retry {
            "dialog-error-symbolic"
        } else {
            "dialog-password-symbolic"
        },
        title: if retry {
            "Неверный пароль"
        } else {
            "Пароль доступа"
        },
        text: "",
        fingerprint: None,
        password: true,
        yes: "Войти",
        yes_style: "suggested-action",
        no: "Отмена",
    })
    .await
}

/// For UI checks: a session page showing one of the questions, without a connection.
pub async fn check_session_lifetime(window: &Window) -> Result<Vec<String>, String> {
    let mut computer = Computer::new("\0", 47999);
    computer.name = "Проверка повторного подключения".into();
    let page = session_page(window, &computer);
    page.show_result("network-offline-symbolic", "Нет соединения", None, true);
    let weak = Rc::downgrade(&page);
    drop(page); // The completed connection task no longer owns the controller.
    if weak.upgrade().is_none() {
        return Err("session: controller dropped on the error page".into());
    }
    let cover = pages::frame(
        "Проверка навигации",
        "session-cover",
        &gtk::Label::new(Some("Проверка")),
        None,
    );
    window.navigation().push(&cover);
    if weak.upgrade().is_none_or(|p| p.gone.get()) {
        return Err("session: covering the page ended the session".into());
    }
    window.navigation().pop();
    weak.upgrade()
        .ok_or("session: lost on uncover")?
        .retry
        .emit_clicked();
    let started = std::time::Instant::now();
    while weak.upgrade().is_some_and(|p| p.generation.get() == 0) {
        if started.elapsed().as_secs() > 1 {
            return Err("session: retry did not start after an error".into());
        }
        glib::timeout_future(std::time::Duration::from_millis(10)).await;
    }
    window.navigation().pop();
    let started = std::time::Instant::now();
    while weak.upgrade().is_some() {
        if started.elapsed().as_secs() > 1 {
            return Err("session: controller stayed alive after removal".into());
        }
        glib::timeout_future(std::time::Duration::from_millis(10)).await;
    }
    Ok(vec![
        "session controller survives a completed error flow".into(),
        "covered session remains alive".into(),
        "retry works after an error".into(),
        "session controller is released after leaving".into(),
    ])
}

pub async fn check_cursor(window: &Window) -> Result<Vec<String>, String> {
    let remote = Remote::new();
    remote.width.set(160);
    remote.height.set(90);
    let nav = pages::frame("Проверка курсора", "cursor-check", &remote.picture, None);
    window.navigation().push(&nav);
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    // With no browser acknowledging Broadway frames, a dynamically pushed page has
    // no allocation yet. Use a deterministic private viewport for this cursor check.
    remote.picture.allocate(800, 600, -1, None);
    let center = (
        remote.picture.width() as f64 / 2.0,
        remote.picture.height() as f64 / 2.0,
    );
    remote.pointer.set(Some(center));
    remote.sync_cursor();
    if remote.cursor_hidden.get() {
        return Err("cursor: hidden before the first frame".into());
    }
    let pixels = glib::Bytes::from_owned(vec![200u8; 160 * 90 * 4]);
    let frame =
        gtk::gdk::MemoryTexture::new(160, 90, gtk::gdk::MemoryFormat::R8g8b8a8, &pixels, 160 * 4);
    remote.picture.set_paintable(Some(&frame));
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    if !remote.cursor_hidden.get() {
        return Err(format!("cursor: first frame did not hide the local pointer without motion (widget {}x{}, pointer {:?})",
            remote.picture.width(), remote.picture.height(), remote.pointer.get()));
    }
    let texture = remote
        .picture
        .cursor()
        .and_then(|c| c.texture())
        .ok_or("cursor: no explicit cursor texture")?;
    let mut pixel = [255u8; 4];
    texture.download(&mut pixel, 4);
    if texture.width() != 1 || texture.height() != 1 || pixel[3] != 0 {
        return Err("cursor: texture is not transparent".into());
    }
    remote.pointer.set(Some((1.0, center.1)));
    remote.sync_cursor();
    if !remote.cursor_hidden.get() {
        return Err("cursor: pointer inside the image was not hidden".into());
    }
    let pixels = glib::Bytes::from_owned(vec![200u8; 160 * 320 * 4]);
    let portrait =
        gtk::gdk::MemoryTexture::new(160, 320, gtk::gdk::MemoryFormat::R8g8b8a8, &pixels, 160 * 4);
    remote.picture.set_paintable(Some(&portrait));
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    if remote.cursor_hidden.get() {
        return Err("cursor: stationary pointer over a new margin stayed hidden".into());
    }
    remote.picture.set_paintable(None::<&gtk::gdk::Paintable>);
    glib::timeout_future(std::time::Duration::from_millis(30)).await;
    if remote.picture.cursor().is_some() {
        return Err("cursor: not restored after the picture ended".into());
    }
    window.navigation().pop();
    Ok(vec![
        "cursor hides on first frame without motion".into(),
        "cursor is an explicit transparent texture".into(),
        "cursor follows image geometry while stationary".into(),
        "cursor restores when the picture ends".into(),
    ])
}

pub fn preview(window: &Window, which: &str) {
    const FP: &str = "3F2A-91C4-0B7E-55D1-AE20-6C93-1F48-B70D";
    let mut computer = Computer::new("127.0.0.1", 47999);
    computer.name = "GEM12".into();
    let page = SessionPage::new(&computer, window.widget());
    watch_leave(window, &page);
    window.navigation().push(&page.page);
    match which {
        "connecting" => {
            page.show_progress("Подключение…");
            return;
        }
        "connection-error" => {
            page.show_result(
                "network-offline-symbolic",
                "Нет соединения",
                Some("Компьютер не ответил"),
                true,
            );
            return;
        }
        "session" | "monitors" => {
            let pixels = glib::Bytes::from_owned([38u8, 44, 54, 255].repeat(640 * 360));
            let frame = gtk::gdk::MemoryTexture::new(
                640,
                360,
                gtk::gdk::MemoryFormat::R8g8b8a8,
                &pixels,
                640 * 4,
            );
            page.remote.picture.set_paintable(Some(&frame));
            page.remote.width.set(640);
            page.remote.height.set(360);
            page.stack.set_visible_child_name("screen");
            page.controls.set_visible(true);
            page.disconnect.set_visible(true);
            page.browse_files.set_visible(true);
            page.fullscreen.set_visible(true);
            page.sound.set_visible(true);
            let monitors = vec![
                casement_net::screen::Monitor {
                    id: "DP-1".into(),
                    name: "Основной экран".into(),
                    width: 1920,
                    height: 1080,
                    primary: true,
                },
                casement_net::screen::Monitor {
                    id: "HDMI-1".into(),
                    name: "Второй экран".into(),
                    width: 2560,
                    height: 1440,
                    primary: false,
                },
            ];
            page.offer_monitors(window, &computer, &monitors, "DP-1");
            if which == "monitors" {
                page.open_monitors(window, &computer);
            }
            return;
        }
        _ => {}
    }
    let which = which.to_string();
    glib::spawn_future_local(async move {
        match which.as_str() {
            "key-changed" => ask_key_changed(&page, "GEM12", FP).await,
            _ => ask_password(&page, "gem12", false).await,
        };
    });
}

/// Closes the session page if it is still the one on screen: when the user already left
/// it, popping again would take away the page they went back to.
fn close_page(window: &Window, page: &SessionPage) {
    let navigation = window.navigation();
    if !page.gone.get() && navigation.visible_page().as_ref() == Some(&page.page) {
        navigation.pop();
    }
}

/// Ends the session when its page leaves the stack, not when another page covers it.
fn watch_leave(window: &Window, page: &Rc<SessionPage>) {
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    // A failed flow ends, but the page and its retry action must remain alive.
    let keep = RefCell::new(Some(page.clone()));
    let (target, handler2) = (page.page.clone(), handler.clone());
    let id = window.navigation().connect_popped(move |nav, popped| {
        if popped == &target {
            if let Some(page) = keep.take() {
                page.leave();
            }
            if let Some(id) = handler2.take() {
                nav.disconnect(id);
            }
        }
    });
    handler.replace(Some(id));
}

/// What the network side reports once the picture is on its way.
struct Started {
    width: u32,
    height: u32,
    input: InputSender,
    files: FileSender,
    sound: Option<casement_net::audio::AudioFormat>,
    monitors: Vec<casement_net::screen::Monitor>,
    monitor: String,
}

async fn resolve(host: String, port: u16) -> Result<SocketAddr, NetError> {
    let not_found = || NetError::Io(std::io::ErrorKind::NotFound.into());
    run(async move {
        tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| not_found())?
            .min_by_key(|a| a.is_ipv6())
            .ok_or_else(not_found)
    })
    .await
}

/// Opens a session page and runs the whole connection on the main loop.
pub fn open(window: &Window, computer: Computer) {
    let page = session_page(window, &computer);
    glib::spawn_future_local(flow(window.clone(), page, computer));
}

fn session_page(window: &Window, computer: &Computer) -> Rc<SessionPage> {
    let page = SessionPage::new(computer, window.widget());
    watch_leave(window, &page);
    window.navigation().push(&page.page);
    let window = window.clone();
    let retry_page = Rc::downgrade(&page);
    let retry_window = window.clone();
    let retry_computer = computer.clone();
    page.retry.connect_clicked(move |_| {
        if let Some(page) = retry_page.upgrade() {
            let latest = retry_window
                .store()
                .find(&retry_computer.id)
                .unwrap_or_else(|| retry_computer.clone());
            glib::spawn_future_local(flow(retry_window.clone(), page, latest));
        }
    });
    page
}

async fn flow(window: Window, page: Rc<SessionPage>, computer: Computer) {
    if page.gone.get() {
        return;
    }
    let generation = page.generation.get().wrapping_add(1);
    page.generation.set(generation);
    let (stop, cancel) = tokio::sync::oneshot::channel();
    page.connecting.replace(Some(stop));
    tokio::select! {
        _ = cancel => {},
        _ = flow_inner(window, page.clone(), computer) => {},
    }
    if page.generation.get() == generation {
        page.connecting.take();
    }
}

async fn flow_inner(window: Window, page: Rc<SessionPage>, mut computer: Computer) {
    let core = window.core();
    page.show_progress("Подключение…");
    let addr = match resolve(computer.host.clone(), computer.port).await {
        Ok(addr) => addr,
        Err(e) => {
            return retry_or_show(
                &window,
                &page,
                &computer,
                "Не удалось подключиться",
                &describe(&e),
            )
        }
    };
    let info = ClientInfo {
        name: core.name.clone(),
        os: casement_net::os_name().into(),
        remember: true,
    };
    let mut retry = false;
    let session = loop {
        let expected = Fingerprint::from_hex(&computer.fingerprint);
        let identity = core.identity.clone();
        let info2 = ClientInfo {
            name: info.name.clone(),
            os: info.os.clone(),
            remember: true,
        };
        let handshake: Result<Handshake, NetError> =
            run(async move { client::connect(&identity, addr, expected, &info2).await }).await;
        let handshake = match handshake {
            Ok(h) => h,
            Err(NetError::KeyChanged { seen, .. }) => {
                if ask_key_changed(&page, &computer.title(), &seen.display())
                    .await
                    .is_some()
                {
                    // Approval is for this exact key, including the next handshake.
                    computer.fingerprint = seen.to_hex();
                    page.show_progress("Проверка нового ключа…");
                    continue;
                }
                close_page(&window, &page);
                return;
            }
            Err(e) => {
                return retry_or_show(
                    &window,
                    &page,
                    &computer,
                    "Не удалось подключиться",
                    &describe(&e),
                )
            }
        };
        if expected.is_none() {
            // First contact: the key is remembered without asking (owner's decision
            // 2026-10-28: on a home network the comparison only got in the way). A later
            // change of this key still stops the connection until the user decides.
            computer.fingerprint = handshake.host_fingerprint.to_hex();
        }
        let password = if handshake.needs_password() {
            match ask_password(&page, &handshake.host_name, retry).await {
                Some(p) => Some(p),
                None => {
                    handshake.close();
                    close_page(&window, &page);
                    return;
                }
            }
        } else {
            None
        };
        page.show_progress("Вход…");
        match run(async move { handshake.login(password.as_deref()).await }).await {
            Ok(session) => break session,
            Err(NetError::AuthFailed) => {
                retry = true;
                continue;
            }
            Err(e) => {
                return retry_or_show(&window, &page, &computer, "Не удалось войти", &describe(&e))
            }
        }
    };

    if computer.name.is_empty() {
        computer.name = session.host_name.clone();
    }
    window.remember(computer.clone());
    page.page.set_title(&computer.title());
    page.host_name.replace(computer.title());
    page.show_progress("Ожидание экрана…");

    // The network side lives on Tokio: it starts the screen, pumps frames into the decoder
    // and keeps the session until the page goes away.
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    page.stop.replace(Some(stop_tx));
    let (keyframe_tx, mut keyframe_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    page.keyframe.replace(Some(keyframe_tx));
    // A virtual monitor gets the size of the area the picture will fill.
    let request = casement_net::screen::ScreenRequest {
        max_width: page.stack.width().max(640) as u32,
        max_height: page.stack.height().max(480) as u32,
        virtual_monitor: computer.virtual_monitor,
        monitor: page.monitor.borrow().clone(),
    };
    let (screen_tx, screen_rx) = async_channel::bounded::<Result<Started, String>>(1);
    let (src_tx, src_rx) = tokio::sync::oneshot::channel::<gst_app::AppSrc>();
    let (sound_src_tx, sound_src_rx) = tokio::sync::oneshot::channel::<gst_app::AppSrc>();
    let (ended_tx, ended_rx) = async_channel::bounded::<Option<String>>(1);
    let (resize_tx, mut resize_rx) = tokio::sync::mpsc::unbounded_channel::<(u32, u32)>();
    if computer.virtual_monitor {
        page.resize_out.replace(Some(resize_tx));
    }
    let (clip_out_tx, mut clip_out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    page.clipboard_out.replace(Some(clip_out_tx));
    let (clip_in_tx, clip_in_rx) = async_channel::unbounded::<String>();
    runtime().spawn(async move {
        let mut session = session;
        let screen = match session.start_screen_with(request).await {
            Ok(screen) => screen,
            Err(e) => {
                let _ = screen_tx.send(Err(describe(&e))).await;
                session.close();
                return;
            }
        };
        // Sound is a bonus: without it the session goes on silently.
        let sound = match session.start_audio().await {
            Ok(format) if format.codec == "opus" => Some(format),
            _ => None,
        };
        let (mut control, mut messages, handle) = session.split();
        let _ = screen_tx
            .send(Ok(Started {
                width: screen.width,
                height: screen.height,
                input: handle.input_sender(),
                files: handle.file_sender(),
                sound: sound.clone(),
                monitors: screen.monitors,
                monitor: screen.monitor,
            }))
            .await;
        let Ok(src) = src_rx.await else { return };
        let player = sound_src_rx.await.ok().map(|sound_src| {
            let receiver = handle.audio_receiver();
            #[cfg(windows)]
            let mut decoder = sound
                .as_ref()
                .and_then(|f| casement_media::OpusDecoder::new(f.rate, f.channels).ok());
            tokio::spawn(async move {
                while let Ok(packet) = receiver.next().await {
                    #[cfg(windows)]
                    let data = match decoder.as_mut() {
                        Some(decoder) => decoder
                            .decode(&packet.data)
                            .iter()
                            .flat_map(|s| s.to_le_bytes())
                            .collect::<Vec<u8>>(),
                        None => return,
                    };
                    #[cfg(not(windows))]
                    let data = packet.data;
                    if sound_src.push_buffer(gst::Buffer::from_slice(data)).is_err() {
                        return;
                    }
                }
            })
        });
        // Each stream is read by its own task: reads are not cancellation-safe in select!.
        let (frames_tx, mut frames) = tokio::sync::mpsc::channel(2);
        let mut video = screen.video;
        let reader = tokio::spawn(async move {
            loop {
                let frame = casement_net::screen::read_frame(&mut video).await;
                let last = !matches!(frame, Ok(Some(_)));
                if frames_tx.send(frame).await.is_err() || last {
                    return;
                }
            }
        });
        let listener = tokio::spawn(async move {
            while let Ok(Some(message)) = messages.next().await {
                let casement_net::client::HostMessage::Clipboard(text) = message;
                if clip_in_tx.send(text).await.is_err() {
                    return;
                }
            }
        });
        let mut stop_rx = stop_rx;
        let mut budget = casement_net::video::VideoBudget::default();
        let mut recovery = tokio::time::interval(std::time::Duration::from_millis(100));
        recovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let reason = loop {
            tokio::select! {
                frame = frames.recv() => match frame {
                    Some(Ok(Some(frame))) => {
                        if !budget.accept(frame.keyframe, src.property::<u64>("current-level-buffers")) {
                            continue;
                        }
                        let mut buffer = gst::Buffer::from_slice(frame.data);
                        if !frame.keyframe {
                            buffer.get_mut().unwrap().set_flags(gst::BufferFlags::DELTA_UNIT);
                        }
                        if src.push_buffer(buffer).is_err() {
                            break None;
                        }
                    }
                    Some(Ok(None)) | None => break Some("Тот компьютер прекратил показ экрана".to_string()),
                    Some(Err(e)) => break Some(describe(&e)),
                },
                Some(()) = keyframe_rx.recv() => {
                    if control.request_keyframe().await.is_err() {
                        break Some("Соединение прервалось".to_string());
                    }
                }
                _ = recovery.tick(), if budget.recovering => {
                    if control.request_keyframe().await.is_err() {
                        break Some("Соединение прервалось".to_string());
                    }
                }
                Some((width, height)) = resize_rx.recv() => {
                    if control.resize(width, height).await.is_err() {
                        break Some("Соединение прервалось".to_string());
                    }
                }
                Some(text) = clip_out_rx.recv() => {
                    if control.clipboard(&text).await.is_err() {
                        break Some("Соединение прервалось".to_string());
                    }
                }
                _ = &mut stop_rx => break None,
            }
        };
        reader.abort();
        listener.abort();
        if let Some(player) = player {
            player.abort();
        }
        handle.close();
        let _ = ended_tx.send(reason).await;
    });

    match screen_rx.recv().await {
        Ok(Ok(Started {
            width,
            height,
            input,
            files,
            sound,
            monitors,
            monitor,
        })) => {
            page.remote.width.set(width);
            page.remote.height.set(height);
            match page.start_decoder() {
                Ok(src) => {
                    let player = sound.and_then(|format| {
                        page.start_sound(&format)
                            .map_err(|e| glib::g_warning!("Casement", "sound: {}", e))
                            .ok()
                    });
                    // The network side waits for this answer: without a player it must see
                    // the sender go away, or it would never start the picture.
                    match player {
                        Some(sound_src) => {
                            let _ = sound_src_tx.send(sound_src);
                        }
                        None => drop(sound_src_tx),
                    }
                    let _ = src_tx.send(src);
                    page.remote.input.replace(Some(input));
                    page.share_clipboard(clip_in_rx.clone());
                    page.follow_window_size(width, height);
                    page.enable_files(&window);
                    page.controls.set_visible(true);
                    page.disconnect.set_visible(true);
                    page.remote_files.replace(Some(files));
                    page.browse_files.set_visible(true);
                    page.offer_monitors(&window, &computer, &monitors, &monitor);
                    if std::env::var("CASEMENT_OPEN").as_deref() == Ok("connect-files") {
                        page.open_remote_files(&window);
                    }
                    page.reconnects.set(0);
                    page.stack.set_visible_child_name("screen");
                    page.remote.picture.grab_focus();
                }
                Err(e) => {
                    page.shutdown();
                    return page.show_result(
                        "dialog-error-symbolic",
                        "Не удалось показать экран",
                        Some(&e),
                        true,
                    );
                }
            }
        }
        Ok(Err(message)) => {
            return page.show_result(
                "network-offline-symbolic",
                &format!("Подключено к «{}»", computer.title()),
                Some(&message),
                true,
            );
        }
        Err(_) => return,
    }

    if let Ok(Some(reason)) = ended_rx.recv().await {
        page.shutdown();
        // The picture was there: try to get it back on our own first.
        page.reconnects.set(1);
        schedule_reconnect(&window, &page, &computer, &reason);
    }
}

const RECONNECT_DELAYS: [u64; 5] = [2, 4, 8, 15, 30];

/// During automatic reconnects a failure schedules the next attempt; otherwise (or after
/// the last one) it is shown with a button to try again.
fn retry_or_show(
    window: &Window,
    page: &Rc<SessionPage>,
    computer: &Computer,
    heading: &str,
    message: &str,
) {
    let attempt = page.reconnects.get();
    if attempt > 0 && (attempt as usize) < RECONNECT_DELAYS.len() && !page.gone.get() {
        page.reconnects.set(attempt + 1);
        schedule_reconnect(window, page, computer, message);
    } else {
        page.reconnects.set(0);
        page.show_result("network-offline-symbolic", heading, Some(message), true);
    }
}

fn schedule_reconnect(window: &Window, page: &Rc<SessionPage>, computer: &Computer, _reason: &str) {
    let attempt = page.reconnects.get();
    let delay = RECONNECT_DELAYS[(attempt as usize)
        .saturating_sub(1)
        .min(RECONNECT_DELAYS.len() - 1)];
    page.show_progress(&format!(
        "Переподключение через {delay} с · {attempt} из {}",
        RECONNECT_DELAYS.len()
    ));
    let (window, page, id) = (window.clone(), page.clone(), computer.id.clone());
    let fallback = computer.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(delay), move || {
        if page.gone.get() {
            return;
        }
        let computer = window.store().find(&id).unwrap_or(fallback);
        glib::spawn_future_local(flow(window, page, computer));
    });
}
