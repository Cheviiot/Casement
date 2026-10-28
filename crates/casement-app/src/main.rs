//! Casement: your own remote desktop for the LAN and Tailscale.

// A window application on Windows: no console behind it.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod computer;
mod core;
mod pages;
mod remote_files;
mod service;
mod session;
mod store;
mod window;

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;

const APP_ID: &str = "io.github.cheviiot.Casement";

fn main() -> glib::ExitCode {
    casement_media::use_bundled_gstreamer();
    if let Err(e) = gstreamer::init() {
        eprintln!("GStreamer: {e}");
    }
    // The paintable sink is built into Casement, so the system needs no gtk4 GStreamer plugin.
    let _ = gstgtk4::plugin_register_static();

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|app| {
        let about = gtk::gio::SimpleAction::new("about", None);
        about.connect_activate(glib::clone!(
            #[weak]
            app,
            move |_, _| show_about(&app)
        ));
        app.add_action(&about);
        let quit = gtk::gio::SimpleAction::new("quit", None);
        quit.connect_activate(glib::clone!(
            #[weak]
            app,
            move |_, _| app.quit()
        ));
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        let css = gtk::CssProvider::new();
        css.load_from_string(".remote-screen { background: black; }");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
    app.connect_activate(activate);
    app.run()
}

fn activate(app: &adw::Application) {
    let core = match core::Core::open(core::config_dir()) {
        Ok(core) => Rc::new(core),
        Err(e) => {
            let dialog = adw::AlertDialog::new(
                Some("Casement не запустился"),
                Some(&format!("Ключ устройства недоступен: {e}")),
            );
            dialog.add_response("close", "Закрыть");
            let window = adw::ApplicationWindow::builder().application(app).build();
            window.present();
            dialog.present(Some(&window));
            return;
        }
    };
    // Broadway without a viewer never advances transitions: pages pushed for a snapshot
    // must not animate.
    if std::env::var_os("CASEMENT_SNAPSHOT").is_some() {
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_enable_animations(false);
        }
    }
    let window = window::Window::new(app, core);
    window.present();
    if let Ok(page) = std::env::var("CASEMENT_OPEN") {
        if !page.is_empty() {
            window.open_dev_page(&page);
        }
    }
    if let Ok(path) = std::env::var("CASEMENT_SNAPSHOT") {
        let delay = std::env::var("CASEMENT_SNAPSHOT_DELAY")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(2500u64);
        let widget = window.widget();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            snapshot_after_paint(widget, path, app)
        });
    }
}

/// Saves the window as PNG right after a paint (a redrawing widget has no picture
/// between paints), then quits. Development checks only.
fn snapshot_after_paint(window: adw::ApplicationWindow, path: String, app: adw::Application) {
    let Some(clock) = window.frame_clock() else {
        return;
    };
    let attempts = Rc::new(std::cell::Cell::new(0));
    let handler: Rc<std::cell::RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let handler2 = handler.clone();
    let id = clock.connect_after_paint(glib::clone!(
        #[weak]
        window,
        move |clock| {
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let (w, h) = (window.width(), window.height());
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, w as f64, h as f64);
            let done = match snapshot.to_node() {
                Some(node) => {
                    if let Some(renderer) = window.renderer() {
                        let texture = renderer.render_texture(
                            &node,
                            Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)),
                        );
                        let _ = texture.save_to_png(&path);
                    }
                    true
                }
                None => {
                    attempts.set(attempts.get() + 1);
                    attempts.get() >= 30
                }
            };
            if done {
                if let Some(id) = handler2.borrow_mut().take() {
                    clock.disconnect(id);
                }
                let app = app.clone();
                glib::idle_add_local_once(move || app.quit());
            } else {
                window.queue_draw();
            }
        }
    ));
    handler.replace(Some(id));
    window.queue_draw();
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::builder()
        .application_name("Casement")
        .application_icon(APP_ID)
        .developer_name("cheviiot")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Свой удалённый рабочий стол для локальной сети и Tailscale")
        .debug_info(format!("Протокол {}", casement_net::PROTOCOL_VERSION))
        .build();
    about.present(app.active_window().as_ref());
}
