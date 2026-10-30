//! Adding and editing a connection are separate from starting a session.

use crate::pages;
use crate::store::{parse_address, Computer, VideoQuality};
use crate::window::Window;
use adw::prelude::*;
use casement_net::Fingerprint;

/// A deliberate choice: scan the LAN on demand, or enter an address.
pub fn open_new(window: &Window) {
    let content = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let nearby = pages::link_row("network-wireless-symbolic", "В локальной сети", "");
    let this = window.clone();
    nearby.connect_activated(move |_| crate::discovery::open(&this));
    group.add(&nearby);
    let address = pages::link_row("network-server-symbolic", "По адресу", "");
    let this = window.clone();
    address.connect_activated(move |_| open_address(&this));
    group.add(&address);
    content.add(&group);
    window.navigation().push(&pages::preferences(
        "Добавить подключение",
        "add",
        &content,
        None,
    ));
}

pub fn open_address(window: &Window) {
    editor(window, None);
}

pub fn open(window: &Window, id: &str) {
    if let Some(computer) = window.store().find(id) {
        editor(window, Some(computer));
    }
}

fn editor(window: &Window, original: Option<Computer>) {
    let is_new = original.is_none();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let address = adw::EntryRow::builder()
        .title("Адрес")
        .input_purpose(gtk::InputPurpose::Url)
        .text(original.as_ref().map(Computer::address).unwrap_or_default())
        .build();
    let name = adw::EntryRow::builder()
        .title("Название")
        .text(original.as_ref().map(|c| c.name.as_str()).unwrap_or(""))
        .build();
    group.add(&address);
    group.add(&name);
    page.add(&group);
    let virtual_screen = adw::SwitchRow::builder()
        .title("Виртуальный экран")
        .active(original.as_ref().is_some_and(|c| c.virtual_monitor))
        .build();
    let quality = adw::ComboRow::builder()
        .title("Качество")
        .model(&gtk::StringList::new(&VideoQuality::LABELS))
        .build();
    quality.set_selected(
        original
            .as_ref()
            .map(|computer| computer.video_quality.index())
            .unwrap_or_else(|| VideoQuality::default().index()),
    );
    if !is_new {
        let display = adw::PreferencesGroup::builder().title("Экран").build();
        display.add(&virtual_screen);
        display.add(&quality);
        page.add(&display);
    }
    let save = gtk::Button::builder()
        .label(if is_new {
            "Добавить"
        } else {
            "Сохранить"
        })
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let nav = pages::preferences(
        if is_new {
            "По адресу"
        } else {
            "Параметры подключения"
        },
        if is_new { "address" } else { "computer" },
        &page,
        Some(save.upcast_ref()),
    );
    let validation = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .css_classes(["validation", "error"])
        .visible(false)
        .build();
    page.add(&pages::bare(&validation));
    let update = {
        let (address, name, virtual_screen, quality, save, original, validation) = (
            address.clone(),
            name.clone(),
            virtual_screen.clone(),
            quality.clone(),
            save.clone(),
            original.clone(),
            validation.clone(),
        );
        move || {
            let parsed = parse_address(&address.text());
            let invalid = parsed.is_none() && !address.text().trim().is_empty();
            if invalid {
                address.add_css_class("error");
            } else {
                address.remove_css_class("error");
            }
            validation.set_visible(invalid);
            validation.set_label(if invalid {
                "Проверьте адрес и порт"
            } else {
                ""
            });
            let changed = original.as_ref().is_none_or(|c| {
                parsed
                    .as_ref()
                    .is_some_and(|(host, port)| host != &c.host || *port != c.port)
                    || name.text().trim() != c.name
                    || virtual_screen.is_active() != c.virtual_monitor
                    || VideoQuality::from_index(quality.selected()) != c.video_quality
            });
            save.set_sensitive(parsed.is_some() && changed);
        }
    };
    let f = update.clone();
    address.connect_changed(move |_| f());
    let f = update.clone();
    name.connect_changed(move |_| f());
    let f = update.clone();
    virtual_screen.connect_active_notify(move |_| f());
    quality.connect_selected_notify(move |_| update());
    let button = save.clone();
    address.connect_entry_activated(move |_| {
        if button.is_sensitive() {
            button.emit_clicked();
        }
    });
    let button = save.clone();
    name.connect_entry_activated(move |_| {
        if button.is_sensitive() {
            button.emit_clicked();
        }
    });

    if let Some(computer) = &original {
        let identity = adw::PreferencesGroup::builder().title("Соединение").build();
        let key = pages::link_row(
            "dialog-password-symbolic",
            "Ключ компьютера",
            if computer.fingerprint.is_empty() {
                "Не получен"
            } else {
                "Запомнен"
            },
        );
        key.set_sensitive(!computer.fingerprint.is_empty());
        let (window2, id) = (window.clone(), computer.id.clone());
        key.connect_activated(move |_| open_key(&window2, &id));
        identity.add(&key);
        if computer.last_connected > 0 {
            if let Ok(time) = glib::DateTime::from_unix_local(computer.last_connected) {
                if let Ok(time) = time.format("%d.%m.%Y %H:%M") {
                    identity.add(
                        &adw::ActionRow::builder()
                            .title("Последнее подключение")
                            .subtitle(time)
                            .build(),
                    );
                }
            }
        }
        page.add(&identity);
        let remove = pages::pill("Удалить подключение", "destructive-action");
        let (window2, id) = (window.clone(), computer.id.clone());
        remove.connect_clicked(move |_| {
            let Some(computer) = window2.store().find(&id) else {
                return;
            };
            if let Err(e) = window2.store().remove(&id) {
                return window2.toast(&format!("Не удалось удалить: {e}"));
            }
            window2.navigation().pop_to_tag("home");
            let restore = window2.clone();
            window2.toast_undo("Подключение удалено", move || {
                if let Err(e) = restore.store().put(computer.clone()) {
                    restore.toast(&format!("Не удалось восстановить: {e}"));
                }
            });
        });
        page.add(&pages::bare(&remove));
    }
    let window2 = window.clone();
    let address_focus = address.clone();
    save.connect_clicked(move |_| {
        let Some((host, port)) = parse_address(&address.text()) else {
            return;
        };
        if let Some(other) = window2.store().find_by_address(&host, port) {
            if original.as_ref().is_none_or(|c| c.id != other.id) {
                return window2.toast("Это подключение уже добавлено");
            }
        }
        let mut computer = original
            .clone()
            .unwrap_or_else(|| Computer::new(&host, port));
        if computer.host != host || computer.port != port {
            computer.fingerprint.clear();
            computer.routes.clear();
        }
        computer.host = host;
        computer.port = port;
        computer.name = name.text().trim().to_string();
        computer.virtual_monitor = virtual_screen.is_active();
        computer.video_quality = VideoQuality::from_index(quality.selected());
        match window2.store().put(computer) {
            Ok(()) => {
                window2.navigation().pop_to_tag("home");
            }
            Err(e) => window2.toast(&format!("Не удалось сохранить: {e}")),
        }
    });
    window.navigation().push(&nav);
    if is_new {
        // Focus after the transition, without synthesizing keyboard input.
        glib::idle_add_local_once(move || {
            address_focus.grab_focus();
        });
    }
}

fn open_key(window: &Window, id: &str) {
    let Some(computer) = window.store().find(id) else {
        return;
    };
    let Some(key) = Fingerprint::from_hex(&computer.fingerprint) else {
        return;
    };
    let page = adw::PreferencesPage::new();
    page.add(&pages::bare(&pages::fingerprint_label(&key.display())));
    let copy = pages::pill("Скопировать", "suggested-action");
    let display = key.display();
    let this = window.clone();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&display);
        this.toast("Ключ скопирован");
    });
    page.add(&pages::bare(&copy));
    let forget = pages::pill("Сбросить ключ", "destructive-action");
    let this = window.clone();
    forget.connect_clicked(move |_| {
        let Some(mut current) = this.store().find(&computer.id) else {
            return;
        };
        current.fingerprint.clear();
        if let Err(e) = this.store().put(current) {
            return this.toast(&format!("Не удалось сбросить ключ: {e}"));
        }
        this.navigation().pop();
        let restore = this.clone();
        let original = computer.clone();
        this.toast_undo("Ключ сброшен", move || {
            if let Some(mut current) = restore.store().find(&original.id) {
                if current.host == original.host
                    && current.port == original.port
                    && current.fingerprint.is_empty()
                {
                    current.fingerprint = original.fingerprint.clone();
                    if let Err(e) = restore.store().put(current) {
                        restore.toast(&format!("Не удалось восстановить ключ: {e}"));
                    }
                }
            }
        });
    });
    page.add(&pages::bare(&forget));
    window.navigation().push(&pages::preferences(
        "Ключ компьютера",
        "computer-key",
        &page,
        None,
    ));
}
