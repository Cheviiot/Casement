//! The application shell. Home contains saved connections only; discovery, editing,
//! local access and sessions have their own navigation pages.

mod access;
mod password;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use casement_net::auth::{StoredPassword, TrustStore};
use casement_net::host::HostEvent;
use casement_net::DEFAULT_PORT;

use crate::core::{run, runtime, Core};
use crate::pages::{self, flat_icon, link_row};
use crate::store::{Computer, Store};
use crate::{computer, service, session};

struct Inner {
    window: adw::ApplicationWindow,
    navigation: adw::NavigationView,
    toasts: adw::ToastOverlay,
    store: Store,
    core: Rc<Core>,
    saved: gtk::ListBox,
    home_stack: gtk::Stack,
    add: gtk::Button,
    accept: adw::SwitchRow,
    accept_busy: Cell<bool>,
    password_row: adw::ActionRow,
    trusted_row: adw::ActionRow,
    trusted_list: gtk::ListBox,
    trusted_stack: gtk::Stack,
    access_status: gtk::Label,
    settings_page: RefCell<Option<adw::NavigationPage>>,
    trusted_page: RefCell<Option<adw::NavigationPage>>,
    // Keep the entire host alive, including its mDNS announcement.
    host: RefCell<Option<casement_media::service::RunningHost>>,
    service: bool,
    updating_accept: Cell<bool>,
    machine: bool,
    preview: bool,
}

const ACCEPT_OFF: &str = "accept-off";

fn has_password(dir: &std::path::Path) -> Result<bool, casement_net::auth::AuthError> {
    StoredPassword::load(dir).map(|password| password.is_some())
}

#[derive(Clone)]
pub struct Window(Rc<Inner>);

impl Window {
    pub fn new(app: &adw::Application, core: Rc<Core>) -> Self {
        let preview = std::env::var_os("CASEMENT_SNAPSHOT").is_some();
        let machine = !preview && service::machine();
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Casement")
            .default_width(
                std::env::var("CASEMENT_WINDOW_WIDTH")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(900),
            )
            .default_height(
                std::env::var("CASEMENT_WINDOW_HEIGHT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(660),
            )
            .width_request(360)
            .height_request(360)
            .build();
        let saved = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list", "device-list"])
            .build();
        let add = gtk::Button::builder()
            .label("Добавить")
            .tooltip_text("Добавить подключение")
            .build();
        let accept = adw::SwitchRow::builder()
            .title("Принимать подключения")
            .build();
        let password_row = if machine {
            adw::ActionRow::builder().title("Пароль доступа").build()
        } else {
            link_row("dialog-password-symbolic", "Пароль доступа", "")
        };
        let trusted_row = if machine {
            adw::ActionRow::builder()
                .title("Доверенные устройства")
                .build()
        } else {
            link_row("security-high-symbolic", "Доверенные устройства", "")
        };
        let trusted_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let navigation = adw::NavigationView::new();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&navigation));
        window.set_content(Some(&toasts));
        let this = Self(Rc::new(Inner {
            store: Store::open(&core.dir),
            window,
            navigation,
            toasts,
            core,
            saved,
            home_stack: gtk::Stack::new(),
            add,
            accept,
            accept_busy: Cell::new(false),
            password_row,
            trusted_row,
            trusted_list,
            trusted_stack: gtk::Stack::new(),
            access_status: gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["dim-label"])
                .build(),
            settings_page: RefCell::new(None),
            trusted_page: RefCell::new(None),
            host: RefCell::new(None),
            service: !preview && service::installed(),
            machine,
            updating_accept: Cell::new(false),
            preview,
        }));
        this.0.navigation.add(&this.build_home());
        this.connect_signals();
        this.rebuild_saved();
        this.initialize_access();
        this
    }

    pub fn widget(&self) -> adw::ApplicationWindow {
        self.0.window.clone()
    }
    pub fn navigation(&self) -> adw::NavigationView {
        self.0.navigation.clone()
    }
    pub fn store(&self) -> Store {
        self.0.store.clone()
    }
    pub fn core(&self) -> Rc<Core> {
        self.0.core.clone()
    }
    pub fn present(&self) {
        self.0.window.present();
    }

    pub fn toast(&self, text: &str) {
        self.0.toasts.add_toast(
            adw::Toast::builder()
                .title(glib::markup_escape_text(text).as_str())
                .timeout(4)
                .build(),
        );
    }

    pub fn toast_undo(&self, text: &str, undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(text).as_str())
            .button_label("Отменить")
            .timeout(6)
            .build();
        toast.connect_button_clicked(move |_| undo());
        self.0.toasts.add_toast(toast);
    }

    fn build_home(&self) -> adw::NavigationPage {
        let header = adw::HeaderBar::new();
        header.pack_start(&self.0.add);
        let settings = flat_icon("emblem-system-symbolic", "Настройки");
        let this = self.clone();
        settings.connect_clicked(move |_| this.open_settings());
        header.pack_end(&settings);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        column.append(
            &gtk::Label::builder()
                .label("Подключения")
                .xalign(0.0)
                .css_classes(["section-heading"])
                .build(),
        );
        column.append(&self.0.saved);
        self.0
            .home_stack
            .add_named(&pages::scrolled(&column, 840), Some("list"));
        let add = pages::pill("Добавить подключение", "suggested-action");
        let this = self.clone();
        add.connect_clicked(move |_| computer::open_new(&this));
        let empty = adw::StatusPage::builder()
            .icon_name("network-workgroup-symbolic")
            .title("Подключений пока нет")
            .css_classes(["empty-connections"])
            .child(&add)
            .build();
        self.0.home_stack.add_named(&empty, Some("empty"));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&self.0.home_stack));
        adw::NavigationPage::builder()
            .title("Casement")
            .tag("home")
            .child(&view)
            .build()
    }

    fn connect_signals(&self) {
        let this = self.clone();
        self.0
            .add
            .connect_clicked(move |_| computer::open_new(&this));
        let this = self.clone();
        self.0.saved.connect_row_activated(move |_, row| {
            if let Some(computer) = this.0.store.find(&row.widget_name()) {
                session::open(&this, computer);
            }
        });
        let this = self.clone();
        self.0.store.connect_changed(move || this.rebuild_saved());
        let this = self.clone();
        self.0.accept.connect_active_notify(move |row| {
            if !this.0.updating_accept.get() {
                this.toggle_host(row.is_active());
            }
        });
        let this = self.clone();
        self.0.password_row.connect_activated(move |_| {
            if this.0.machine {
                this.configure_machine();
            } else {
                this.open_password();
            }
        });
        let this = self.clone();
        self.0.trusted_row.connect_activated(move |_| {
            if this.0.machine {
                this.configure_machine();
            } else {
                this.open_trusted();
            }
        });
    }

    pub fn remember(&self, mut computer: Computer) {
        computer.mark_connected();
        if let Err(e) = self.0.store.put(computer) {
            self.toast(&format!("Не удалось сохранить: {e}"));
        }
    }

    fn rebuild_saved(&self) {
        self.0.saved.remove_all();
        let items = self.0.store.items();
        let empty = items.is_empty();
        self.0.add.set_visible(!empty);
        self.0
            .home_stack
            .set_visible_child_name(if empty { "empty" } else { "list" });
        for computer in items {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&computer.title()).as_str())
                .subtitle(if computer.name.trim().is_empty() {
                    String::new()
                } else {
                    computer.address()
                })
                .title_lines(1)
                .subtitle_lines(1)
                .activatable(true)
                .tooltip_text("Подключиться")
                .css_classes(["device-row"])
                .build();
            row.set_widget_name(&computer.id);
            row.add_prefix(&pages::icon_tile(
                if computer
                    .os_display_name
                    .to_ascii_lowercase()
                    .contains("windows")
                {
                    "computer-symbolic"
                } else if computer.os_display_name.is_empty() {
                    "computer-symbolic"
                } else {
                    "drive-harddisk-system-symbolic"
                },
            ));
            let edit = flat_icon("emblem-system-symbolic", "Параметры подключения");
            let (this, id) = (self.clone(), computer.id);
            edit.connect_clicked(move |_| computer::open(&this, &id));
            row.add_suffix(&edit);
            self.0.saved.append(&row);
        }
    }

    pub fn open_dev_page(&self, page: &str) {
        let first = self.0.store.items().into_iter().next();
        match page {
            "home" => {}
            "new-device" => computer::open_new(self),
            "address" => computer::open_address(self),
            "discovery" => crate::discovery::open(self),
            "computer" | "edit-first" => {
                if let Some(c) = first {
                    computer::open(self, &c.id);
                }
            }
            "settings" | "this-computer" => self.open_settings(),
            "set-password" => {
                self.open_settings();
                self.open_password();
            }
            "trusted" => {
                self.open_settings();
                self.open_trusted();
            }
            "about" => {
                self.open_settings();
                self.open_about();
            }
            "key-changed" | "password" | "connecting" | "connection-error" | "session"
            | "monitors" => session::preview(self, page),
            "local-files" => crate::local_files::open(self, |_| {}),
            "connect-first" | "connect-files" | "connect-monitors" | "connect-switch-monitor" => {
                if let Some(c) = first {
                    session::open(self, c);
                }
            }
            other => glib::g_warning!("Casement", "Unknown page {}", other),
        }
    }
}
