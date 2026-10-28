//! Casement: your own remote desktop for the LAN and Tailscale.

// A window application on Windows: no console behind it.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod computer;
mod core;
mod discovery;
mod local_files;
mod pages;
mod remote_files;
mod service;
mod session;
mod store;
mod ui_check;
mod viewport;
mod window;

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;

const APP_ID: &str = "io.github.cheviiot.Casement";

fn main() -> glib::ExitCode {
    casement_media::use_bundled_gstreamer();
    // Windows: GTK's GL and Vulkan renderers paint the window's shadow black (GTK issue
    // 7567; from 4.23 GTK itself falls back to Cairo there). The remote screen comes
    // scaled to fit on the GPU, so Cairo only copies it.
    if cfg!(windows) && std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
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
        let quit = gtk::gio::SimpleAction::new("quit", None);
        quit.connect_activate(glib::clone!(
            #[weak]
            app,
            move |_, _| app.quit()
        ));
        app.add_action(&quit);
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("style.css"));
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
            eprintln!("Casement: {e}");
            let close = pages::pill("Закрыть", "suggested-action");
            let app2 = app.clone();
            close.connect_clicked(move |_| app2.quit());
            let status = adw::StatusPage::builder()
                .icon_name("dialog-password-symbolic")
                .title("Ключ устройства недоступен")
                .child(&close)
                .build();
            let navigation = adw::NavigationView::new();
            navigation.add(&pages::frame("Casement", "startup-error", &status, None));
            let window = adw::ApplicationWindow::builder()
                .application(app)
                .default_width(600)
                .default_height(460)
                .content(&navigation)
                .build();
            window.present();
            schedule_snapshot(window, app.clone());
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
    if let (Ok(check_path), Ok(snapshot)) = (
        std::env::var("CASEMENT_UI_CHECK"),
        std::env::var("CASEMENT_SNAPSHOT"),
    ) {
        if !check_path.is_empty() {
            let app = app.clone();
            glib::spawn_future_local(async move {
                let result = ui_check::run(&window).await;
                let report = match result {
                    Ok(checks) => format!("PASS\n{}\n", checks.join("\n")),
                    Err(error) => format!("FAIL\n{error}\n"),
                };
                eprintln!("{report}");
                let _ = std::fs::write(check_path, report);
                snapshot_after_paint(window.widget(), snapshot, app);
            });
            return;
        }
    }
    if let Ok(page) = std::env::var("CASEMENT_OPEN") {
        if !page.is_empty() {
            window.open_dev_page(&page);
        }
    }
    schedule_snapshot(window.widget(), app.clone());
}

fn schedule_snapshot(window: adw::ApplicationWindow, app: adw::Application) {
    if let Ok(path) = std::env::var("CASEMENT_SNAPSHOT") {
        let delay = std::env::var("CASEMENT_SNAPSHOT_DELAY")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(2500u64);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            snapshot_after_paint(window, path, app)
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
