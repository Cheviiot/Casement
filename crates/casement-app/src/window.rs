//! The main window: a navigation stack of pages. The home page connects by address and
//! lists saved and nearby computers; "Этот компьютер" and its password and trusted
//! devices are pages of their own; a computer's settings live on its page
//! (`computer.rs`), a connection on the session page (`session.rs`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use casement_net::auth::{StoredPassword, TrustStore};
use casement_net::discovery::{Announcement, Browser, DiscoveryEvent};
use casement_net::host::{Host, HostEvent};
use casement_net::DEFAULT_PORT;

use crate::computer;
use crate::core::{run, runtime, Core};
use crate::pages::{self, flat_icon, link_row, placeholder};
use crate::service;
use crate::session;
use crate::store::{parse_address, Computer, Store};

struct RunningHost {
    host: Arc<Host>,
    _announcement: Option<Announcement>,
}

struct Inner {
    window: adw::ApplicationWindow,
    navigation: adw::NavigationView,
    toasts: adw::ToastOverlay,
    store: Store,
    core: Rc<Core>,
    address: adw::EntryRow,
    saved: gtk::ListBox,
    nearby_list: gtk::ListBox,
    nearby: RefCell<BTreeMap<String, Computer>>,
    /// "Этот компьютер" on the home page, with a summary of its access.
    this_row: adw::ActionRow,
    accept: adw::SwitchRow,
    password_row: adw::ActionRow,
    trusted_row: adw::ActionRow,
    trusted_list: gtk::ListBox,
    /// The trusted devices page's content: the list, or an explanation when it is empty.
    trusted_stack: gtk::Stack,
    /// Built once, pushed again each time.
    this_page: RefCell<Option<adw::NavigationPage>>,
    trusted_page: RefCell<Option<adw::NavigationPage>>,
    host: RefCell<Option<RunningHost>>,
    /// The systemd user service is installed: the switch drives it instead of an
    /// in-process host, so the computer stays reachable without the window.
    service: bool,
    updating_accept: std::cell::Cell<bool>,
    /// Windows with the system service: the settings below are the machine's.
    machine: bool,
}

/// Without the background service the window accepts connections itself: on unless the
/// user switched it off, which leaves this marker.
const ACCEPT_OFF: &str = "accept-off";

fn accepting_in_app(dir: &std::path::Path) -> bool {
    !dir.join(ACCEPT_OFF).exists()
}

fn set_accepting_in_app(dir: &std::path::Path, on: bool) {
    let marker = dir.join(ACCEPT_OFF);
    let _ = if on {
        std::fs::remove_file(marker)
    } else {
        std::fs::write(marker, b"")
    };
}

fn has_password(dir: &std::path::Path) -> bool {
    matches!(StoredPassword::load(dir), Ok(Some(_)))
}

#[derive(Clone)]
pub struct Window(Rc<Inner>);

impl Window {
    pub fn new(app: &adw::Application, core: Rc<Core>) -> Self {
        let store = Store::open(&core.dir);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Casement")
            .default_width(1100)
            .default_height(720)
            .width_request(360)
            .height_request(360)
            .build();

        let address = adw::EntryRow::builder()
            .title("Адрес или имя компьютера")
            .show_apply_button(true)
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        let saved = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        saved.set_placeholder(Some(&placeholder("Компьютеров пока нет")));
        let nearby_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        nearby_list.set_placeholder(Some(&placeholder("Поиск…")));
        let this_row = link_row("computer-symbolic", &core.name, "");
        let accept = adw::SwitchRow::builder()
            .title("Принимать подключения")
            .build();
        let password_row = link_row("dialog-password-symbolic", "Пароль доступа", "");
        let trusted_row = link_row("security-high-symbolic", "Доверенные устройства", "");
        let trusted_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();

        let trusted_stack = gtk::Stack::new();

        let navigation = adw::NavigationView::new();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&navigation));
        window.set_content(Some(&toasts));

        let this = Self(Rc::new(Inner {
            window,
            navigation,
            toasts,
            store,
            core,
            address,
            saved,
            nearby_list,
            nearby: RefCell::new(BTreeMap::new()),
            this_row,
            accept,
            password_row,
            trusted_row,
            trusted_list,
            trusted_stack,
            this_page: RefCell::new(None),
            trusted_page: RefCell::new(None),
            host: RefCell::new(None),
            service: service::installed(),
            updating_accept: std::cell::Cell::new(false),
            machine: service::machine(),
        }));
        this.0.navigation.add(&this.build_home());
        this.connect_signals();
        this.rebuild_saved();
        this.refresh_access();
        this.start_discovery();
        if this.0.service {
            let this2 = this.clone();
            glib::spawn_future_local(async move {
                service::ensure_running().await;
                let active = service::enabled().await;
                this2.set_accept_quietly(active);
                this2.refresh_access();
            });
        } else if accepting_in_app(&this.0.core.dir) {
            // Without the service the window itself accepts, on by default.
            this.set_accept_quietly(true);
            if let Err(e) = this.start_host() {
                this.set_accept_quietly(false);
                this.toast(&format!("Не удалось принимать подключения: {e}"));
            }
            this.refresh_access();
        }
        this
    }

    fn set_accept_quietly(&self, active: bool) {
        self.0.updating_accept.set(true);
        self.0.accept.set_active(active);
        self.0.updating_accept.set(false);
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

    pub fn toast(&self, text: &str) {
        self.0.toasts.add_toast(
            adw::Toast::builder()
                .title(glib::markup_escape_text(text).as_str())
                .timeout(4)
                .build(),
        );
    }

    /// A toast with "Отменить": the way to take back a removal instead of a question first.
    pub fn toast_undo(&self, text: &str, undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(text).as_str())
            .button_label("Отменить")
            .timeout(6)
            .build();
        toast.connect_button_clicked(move |_| undo());
        self.0.toasts.add_toast(toast);
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    /* Home */

    fn build_home(&self) -> adw::NavigationPage {
        let header = adw::HeaderBar::new();
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Добавить компьютер")
            .build();
        let this = self.clone();
        add.connect_clicked(move |_| computer::open_new(&this));
        header.pack_start(&add);
        let menu = gtk::gio::Menu::new();
        menu.append(Some("О программе"), Some("app.about"));
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .tooltip_text("Меню")
                .menu_model(&menu)
                .primary(true)
                .build(),
        );

        let quick = adw::PreferencesGroup::builder()
            .title("Подключиться")
            .build();
        quick.add(&self.0.address);

        let saved = adw::PreferencesGroup::builder().title("Компьютеры").build();
        saved.add(&self.0.saved);

        let nearby = adw::PreferencesGroup::builder()
            .title("В этой сети")
            .build();
        nearby.add(&self.0.nearby_list);

        let this_group = adw::PreferencesGroup::builder()
            .title("Этот компьютер")
            .build();
        this_group.add(&self.0.this_row);

        let page = adw::PreferencesPage::new();
        page.add(&quick);
        page.add(&saved);
        page.add(&nearby);
        page.add(&this_group);

        let view = adw::ToolbarView::new();
        view.set_content(Some(&page));
        view.add_top_bar(&header);
        adw::NavigationPage::builder()
            .child(&view)
            .title("Casement")
            .tag("home")
            .build()
    }

    fn connect_signals(&self) {
        let this = self.clone();
        self.0
            .address
            .connect_apply(move |row| this.connect_address(&row.text()));
        let this = self.clone();
        self.0
            .address
            .connect_entry_activated(move |row| this.connect_address(&row.text()));
        let address = self.0.address.clone();
        self.0.address.connect_changed(move |_| {
            address.remove_css_class("error");
        });
        let this = self.clone();
        self.0.saved.connect_row_activated(move |_, row| {
            computer::open(&this, &row.widget_name());
        });
        let this = self.clone();
        self.0.nearby_list.connect_row_activated(move |_, row| {
            let found = this
                .0
                .nearby
                .borrow()
                .get(row.widget_name().as_str())
                .cloned();
            if let Some(found) = found {
                // Its key is remembered on the first connection, as for any new computer.
                let computer = this
                    .0
                    .store
                    .find_by_fingerprint(&found.fingerprint)
                    .unwrap_or(Computer {
                        fingerprint: String::new(),
                        ..found
                    });
                session::open(&this, computer);
            }
        });
        let this = self.clone();
        self.0.store.connect_changed(move || {
            this.rebuild_saved();
            this.rebuild_nearby();
        });
        let this = self.clone();
        self.0.accept.connect_active_notify(move |row| {
            this.toggle_host(row.is_active());
            this.refresh_access();
        });
        let this = self.clone();
        self.0
            .this_row
            .connect_activated(move |_| this.open_this_computer());
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

    fn connect_address(&self, text: &str) {
        let Some((host, port)) = parse_address(text) else {
            self.0.address.add_css_class("error");
            self.toast("Не похоже на адрес компьютера");
            return;
        };
        self.0.address.remove_css_class("error");
        let computer = self
            .0
            .store
            .find_by_address(&host, port)
            .unwrap_or_else(|| Computer::new(&host, port));
        session::open(self, computer);
    }

    /// After a successful login: keep the computer and its confirmed key.
    pub fn remember(&self, computer: Computer) {
        let known = self.0.store.find(&computer.id).is_some();
        let title = computer.title();
        if let Err(e) = self.0.store.put(computer) {
            self.toast(&format!("Не удалось сохранить: {e}"));
        } else if !known {
            self.toast(&format!("«{title}» добавлен в список компьютеров"));
        }
    }

    fn rebuild_saved(&self) {
        let list = &self.0.saved;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for computer in self.0.store.items() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&computer.title()).as_str())
                .subtitle(glib::markup_escape_text(&computer::summary(&computer)).as_str())
                .activatable(true)
                .build();
            row.set_widget_name(&computer.id);
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            let connect = gtk::Button::builder()
                .label("Подключиться")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let this = self.clone();
            let id = computer.id.clone();
            connect.connect_clicked(move |_| {
                if let Some(computer) = this.0.store.find(&id) {
                    session::open(&this, computer);
                }
            });
            row.add_suffix(&connect);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            list.append(&row);
        }
    }

    fn rebuild_nearby(&self) {
        let list = &self.0.nearby_list;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for (id, found) in self.0.nearby.borrow().iter() {
            let saved = self.0.store.find_by_fingerprint(&found.fingerprint);
            let title = saved
                .as_ref()
                .map(|c| c.title())
                .unwrap_or_else(|| found.title());
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&title).as_str())
                .subtitle(format!(
                    "{}{}",
                    found.address(),
                    if saved.is_some() {
                        " · в списке"
                    } else {
                        ""
                    }
                ))
                .activatable(true)
                .tooltip_text("Подключиться")
                .build();
            row.set_widget_name(id);
            row.add_prefix(&gtk::Image::from_icon_name("network-wireless-symbolic"));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            list.append(&row);
        }
    }

    fn start_discovery(&self) {
        let own = self.0.core.identity.fingerprint();
        let (tx, rx) = async_channel::unbounded::<DiscoveryEvent>();
        runtime().spawn(async move {
            let Ok(browser) = Browser::start() else {
                return;
            };
            while let Some(event) = browser.next().await {
                if matches!(&event, DiscoveryEvent::Found(peer) if peer.fingerprint == own) {
                    continue;
                }
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    DiscoveryEvent::Found(peer) => {
                        let Some(address) = peer.addresses.first() else {
                            continue;
                        };
                        let mut computer = Computer::new(&address.to_string(), peer.port);
                        computer.name = peer.name;
                        computer.fingerprint = peer.fingerprint.to_hex();
                        this.0.nearby.borrow_mut().insert(peer.id, computer);
                    }
                    DiscoveryEvent::Lost { id } => {
                        this.0.nearby.borrow_mut().remove(&id);
                    }
                }
                this.rebuild_nearby();
            }
        });
    }

    /* This computer */

    fn open_this_computer(&self) {
        let page = self
            .0
            .this_page
            .borrow_mut()
            .get_or_insert_with(|| self.build_this_computer())
            .clone();
        self.0.navigation.push(&page);
    }

    fn build_this_computer(&self) -> adw::NavigationPage {
        let page = adw::PreferencesPage::new();
        let (hero, _, _) = pages::hero("computer-symbolic", &self.0.core.name, "");
        page.add(&pages::bare(&hero));

        let fingerprint = if self.0.machine {
            service::machine_access().map_or_else(|| "—".to_string(), |m| m.fingerprint)
        } else {
            self.0.core.identity.fingerprint().display()
        };
        let key = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        key.append(&pages::fingerprint_label(&fingerprint));
        let copy = pages::pill("Скопировать отпечаток", "flat");
        let this = self.clone();
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&fingerprint);
            this.toast("Отпечаток скопирован");
        });
        key.append(&copy);
        page.add(&pages::bare(&key));

        let access = adw::PreferencesGroup::builder().title("Доступ").build();
        access.add(&self.0.accept);
        access.add(&self.0.password_row);
        access.add(&self.0.trusted_row);
        page.add(&access);

        let files = adw::PreferencesGroup::builder().title("Файлы").build();
        let (downloads, shared) = if self.0.machine {
            // The service works before anyone logs in: the shared public folders.
            let public = std::env::var_os("PUBLIC")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| r"C:\Users\Public".into());
            (public.join("Downloads"), public)
        } else {
            (
                glib::user_special_dir(glib::UserDirectory::Downloads)
                    .unwrap_or_else(|| glib::home_dir().join("Downloads")),
                glib::home_dir(),
            )
        };
        for (title, path) in [
            ("Полученные файлы", downloads),
            ("Доступно для скачивания", shared),
        ] {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(glib::markup_escape_text(&path.display().to_string()).as_str())
                .build();
            row.add_css_class("property");
            files.add(&row);
        }
        page.add(&files);
        pages::preferences("Этот компьютер", "this-computer", &page, None)
    }

    /// An access password is set for connections to this computer.
    fn password_set(&self) -> bool {
        if self.0.machine {
            service::machine_access().is_some_and(|m| m.password)
        } else {
            has_password(&self.0.core.dir)
        }
    }

    /// Windows with the system service: its settings change in the setup wizard, run as
    /// administrator; the service publishes the result within a few seconds.
    fn configure_machine(&self) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = service::configure_machine().await {
                this.toast(&format!("Не удалось: {e}"));
            }
            this.refresh_access();
            glib::timeout_future_seconds(4).await;
            this.refresh_access();
        });
    }

    fn refresh_access(&self) {
        let dir = &self.0.core.dir;
        let password = self.password_set();
        self.0.password_row.set_subtitle(if password {
            "Задан"
        } else {
            "Не задан"
        });
        let store = if self.0.machine {
            TrustStore::default()
        } else {
            TrustStore::load(dir).unwrap_or_default()
        };
        let count = if self.0.machine {
            service::machine_access().map_or(0, |m| m.trusted)
        } else {
            store.devices().len()
        };
        self.0.trusted_row.set_subtitle(&if count == 0 {
            "Нет".to_string()
        } else {
            count.to_string()
        });
        self.0.this_row.set_subtitle(if self.0.accept.is_active() {
            "Принимает подключения"
        } else {
            "Не принимает подключения"
        });

        // The page is built on first use; until then there is nothing to switch.
        let stack = &self.0.trusted_stack;
        let shown = if count == 0 { "empty" } else { "list" };
        if stack.child_by_name(shown).is_some() {
            stack.set_visible_child_name(shown);
        }
        let list = &self.0.trusted_list;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for (fingerprint, name) in store.devices() {
            let name = if name.is_empty() {
                "Без имени".to_string()
            } else {
                name.clone()
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .subtitle(fingerprint.display())
                .build();
            row.add_css_class("property");
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            let remove = flat_icon("user-trash-symbolic", "Больше не доверять");
            let this = self.clone();
            let fingerprint = *fingerprint;
            remove.connect_clicked(move |_| this.distrust(fingerprint, name.clone()));
            row.add_suffix(&remove);
            list.append(&row);
        }
    }

    fn distrust(&self, fingerprint: casement_net::Fingerprint, name: String) {
        let dir = self.0.core.dir.clone();
        if let Ok(mut store) = TrustStore::load(&dir) {
            if let Err(e) = store.remove(&fingerprint) {
                return self.toast(&format!("Не удалось: {e}"));
            }
        }
        self.refresh_access();
        let this = self.clone();
        let again = name.clone();
        self.toast_undo(
            &format!("«{name}» снова будет вводить пароль"),
            move || {
                if let Ok(mut store) = TrustStore::load(&dir) {
                    let _ = store.add(fingerprint, &again);
                }
                this.refresh_access();
            },
        );
    }

    fn open_trusted(&self) {
        let page = self
            .0
            .trusted_page
            .borrow_mut()
            .get_or_insert_with(|| {
                let list = adw::PreferencesPage::new();
                let group = adw::PreferencesGroup::new();
                group.add(&self.0.trusted_list);
                list.add(&group);
                let empty = adw::StatusPage::builder()
                    .icon_name("security-high-symbolic")
                    .title("Доверенных устройств нет")
                    .build();
                let stack = &self.0.trusted_stack;
                stack.add_named(&list, Some("list"));
                stack.add_named(&empty, Some("empty"));
                let count = TrustStore::load(&self.0.core.dir)
                    .map(|t| t.devices().len())
                    .unwrap_or(0);
                stack.set_visible_child_name(if count == 0 { "empty" } else { "list" });
                let header = adw::HeaderBar::new();
                let view = adw::ToolbarView::new();
                view.add_top_bar(&header);
                view.set_content(Some(stack));
                adw::NavigationPage::builder()
                    .title("Доверенные устройства")
                    .tag("trusted")
                    .child(&view)
                    .build()
            })
            .clone();
        self.0.navigation.push(&page);
    }

    /// This computer's access password: at least 8 characters, typed twice.
    fn open_password(&self) {
        let set = has_password(&self.0.core.dir);
        let page = adw::PreferencesPage::new();
        let (hero, _, _) = pages::hero(
            "dialog-password-symbolic",
            if set {
                "Новый пароль доступа"
            } else {
                "Пароль доступа"
            },
            "",
        );
        page.add(&pages::bare(&hero));
        let first = adw::PasswordEntryRow::builder()
            .title("Пароль, не короче 8 символов")
            .build();
        let second = adw::PasswordEntryRow::builder().title("Ещё раз").build();
        let entries = adw::PreferencesGroup::new();
        entries.add(&first);
        entries.add(&second);
        page.add(&entries);
        let save = pages::pill("Сохранить пароль", "suggested-action");
        save.set_sensitive(false);
        page.add(&pages::bare(&save));
        let check = {
            let (first, second, save) = (first.clone(), second.clone(), save.clone());
            move || {
                let long = first.text().chars().count() >= 8;
                let same = first.text() == second.text();
                save.set_sensitive(long && same);
                if !second.text().is_empty() && !same {
                    second.add_css_class("error");
                } else {
                    second.remove_css_class("error");
                }
            }
        };
        let c = check.clone();
        first.connect_changed(move |_| c());
        second.connect_changed(move |_| check());
        let s = save.clone();
        second.connect_entry_activated(move |_| {
            if s.is_sensitive() {
                s.emit_clicked();
            }
        });
        if set {
            let remove = adw::PreferencesGroup::new();
            let button = pages::pill("Убрать пароль", "destructive-action");
            remove.add(&button);
            page.add(&remove);
            let this = self.clone();
            button.connect_clicked(move |_| {
                let _ = StoredPassword::remove(&this.0.core.dir);
                this.refresh_access();
                this.0.navigation.pop();
                this.toast("Пароль убран: подключаться могут только доверенные устройства");
            });
        }
        let nav = pages::preferences("Пароль доступа", "password", &page, None);
        let this = self.clone();
        save.connect_clicked(move |_| {
            let password = first.text().to_string();
            first.set_text("");
            second.set_text("");
            glib::spawn_future_local(this.clone().set_password(password));
        });
        self.0.navigation.push(&nav);
    }

    fn toggle_host(&self, active: bool) {
        if self.0.updating_accept.get() {
            return;
        }
        if self.0.service {
            let this = self.clone();
            glib::spawn_future_local(async move {
                let result = service::set_enabled(active).await;
                match result {
                    Ok(_) if active && !this.password_set() => {
                        this.toast("Задайте пароль доступа, иначе новые устройства не смогут войти")
                    }
                    Ok(_) => {}
                    Err(e) => {
                        this.set_accept_quietly(!active);
                        this.toast(&format!("Не удалось: {e}"));
                    }
                }
            });
            return;
        }
        set_accepting_in_app(&self.0.core.dir, active);
        if !active {
            if let Some(running) = self.0.host.take() {
                running.host.close();
            }
            return;
        }
        if self.0.host.borrow().is_some() {
            return;
        }
        match self.start_host() {
            Ok(()) => {
                if !matches!(StoredPassword::load(&self.0.core.dir), Ok(Some(_))) {
                    self.toast("Задайте пароль доступа, иначе новые устройства не смогут войти");
                }
            }
            Err(e) => {
                self.0.accept.set_active(false);
                self.toast(&format!("Не удалось принимать подключения: {e}"));
            }
        }
    }

    fn start_host(&self) -> Result<(), String> {
        let core = &self.0.core;
        let _guard = runtime().enter();
        let running = casement_media::service::start_host(
            &core.dir,
            &core.name,
            DEFAULT_PORT,
            true,
            false,
            casement_net::transfer::HostFiles {
                incoming: glib::user_special_dir(glib::UserDirectory::Downloads),
                shared: Some(glib::home_dir()),
            },
        )
        .map_err(|e| {
            e.replace("UDP port", "UDP-порт").replace(
                "is already used by another program",
                "уже занят другой программой",
            )
        })?;
        if let Some(e) = &running.screen_error {
            self.toast(&format!("Экран показать не получится: {e}"));
        }
        let host = running.host.clone();
        let runner = host.clone();
        runtime().spawn(async move { runner.run().await });
        let mut events = running.events;
        let (tx, rx) = async_channel::unbounded::<String>();
        runtime().spawn(async move {
            while let Some(event) = events.recv().await {
                let text = match event {
                    HostEvent::LoggedIn { name, trusted, .. } => format!(
                        "К этому компьютеру подключился «{name}»{}",
                        if trusted {
                            ""
                        } else {
                            " (по паролю, теперь доверенный)"
                        }
                    ),
                    HostEvent::FileReceived { path, .. } => format!(
                        "Получен файл «{}» — в папке «Загрузки»",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                    _ => continue,
                };
                if tx.send(text).await.is_err() {
                    return;
                }
            }
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(text) = rx.recv().await {
                this.toast(&text);
                this.refresh_access();
            }
        });
        self.0.host.replace(Some(RunningHost {
            host,
            _announcement: None,
        }));
        Ok(())
    }

    async fn set_password(self, password: String) {
        let dir = self.0.core.dir.clone();
        let result = run(async move {
            tokio::task::spawn_blocking(move || {
                StoredPassword::create(&password).and_then(|p| p.save(&dir))
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()))
        })
        .await;
        match result {
            Ok(()) => {
                self.refresh_access();
                let visible = self.0.navigation.visible_page().and_then(|p| p.tag());
                if visible.as_deref() == Some("password") {
                    self.0.navigation.pop();
                }
                self.toast("Пароль доступа сохранён");
            }
            Err(e) => self.toast(&format!("Пароль не сохранён: {e}")),
        }
    }

    /// For UI checks (`CASEMENT_OPEN`): open a page or a connection without clicks.
    pub fn open_dev_page(&self, page: &str) {
        let first = self.0.store.items().into_iter().next();
        match page {
            "new-device" => computer::open_new(self),
            "edit-first" | "computer" => {
                if let Some(computer) = first {
                    computer::open(self, &computer.id);
                }
            }
            "this-computer" => self.open_this_computer(),
            "set-password" => {
                self.open_this_computer();
                self.open_password();
            }
            "trusted" => {
                self.open_this_computer();
                self.open_trusted();
            }
            "key-changed" | "password" => session::preview(self, page),
            "connect-first" | "connect-files" | "connect-monitors" | "connect-switch-monitor" => {
                if let Some(computer) = first {
                    session::open(self, computer);
                }
            }
            other => glib::g_warning!("Casement", "Unknown page {}", other),
        }
    }
}
