//! A saved computer's page (connect, its settings, its key, removal) and the page that
//! adds a new one. Changes are saved as they are made; a removal can be undone from the
//! toast that reports it.

use adw::prelude::*;

use casement_net::Fingerprint;

use crate::pages;
use crate::session;
use crate::store::{parse_address, Computer};
use crate::window::Window;

/// One line about a computer for lists: "ещё не подключались" until the first
/// connection, then its address when the title is a name.
pub fn summary(computer: &Computer) -> String {
    if computer.fingerprint.is_empty() {
        "ещё не подключались".to_string()
    } else if computer.name.trim().is_empty() {
        String::new()
    } else {
        computer.address()
    }
}

/// A list subtitle used as a sentence of its own.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn virtual_row() -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .title("Отдельный экран под размер окна")
        .build()
}

/// Marks an address row as wrong while its text is not an address.
fn check_address(row: &adw::EntryRow) -> Option<(String, u16)> {
    let parsed = parse_address(&row.text());
    if parsed.is_some() || row.text().trim().is_empty() {
        row.remove_css_class("error");
    } else {
        row.add_css_class("error");
    }
    parsed
}

pub fn open(window: &Window, id: &str) {
    let Some(computer) = window.store().find(id) else {
        return;
    };
    let page = adw::PreferencesPage::new();

    let (hero, heading, line) = pages::hero(
        "computer-symbolic",
        &computer.title(),
        &capitalized(&summary(&computer)),
    );
    let connect = pages::pill("Подключиться", "suggested-action");
    connect.set_margin_top(12);
    hero.append(&connect);
    page.add(&pages::bare(&hero));

    let settings = adw::PreferencesGroup::builder().title("Настройки").build();
    let name = adw::EntryRow::builder()
        .title("Название")
        .text(&computer.name)
        .show_apply_button(true)
        .build();
    let address = adw::EntryRow::builder()
        .title("Адрес")
        .text(computer.address())
        .show_apply_button(true)
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    let virtual_monitor = virtual_row();
    virtual_monitor.set_active(computer.virtual_monitor);
    settings.add(&name);
    settings.add(&address);
    settings.add(&virtual_monitor);
    page.add(&settings);

    let key = adw::PreferencesGroup::builder()
        .title("Ключ компьютера")
        .build();
    let key_row = adw::ActionRow::builder().title("Отпечаток").build();
    key_row.add_css_class("property");
    let forget = gtk::Button::builder()
        .label("Забыть")
        .valign(gtk::Align::Center)
        .build();
    key_row.add_suffix(&forget);
    key.add(&key_row);
    page.add(&key);

    let remove = pages::pill("Удалить компьютер", "destructive-action");
    page.add(&pages::bare(&remove));

    let nav = pages::preferences(&computer.title(), "computer", &page, None);

    // Everything below reads the stored computer afresh: the page may outlive edits.
    let id = computer.id.clone();
    let show = {
        let (window, id) = (window.clone(), id.clone());
        let (nav, heading, line, key_row, forget) = (
            nav.clone(),
            heading.clone(),
            line.clone(),
            key_row.clone(),
            forget.clone(),
        );
        move || {
            let Some(computer) = window.store().find(&id) else {
                return;
            };
            nav.set_title(&computer.title());
            heading.set_label(&computer.title());
            // Without a name the title already is the address.
            let text = capitalized(&summary(&computer));
            line.set_visible(!text.is_empty());
            line.set_label(&text);
            match Fingerprint::from_hex(&computer.fingerprint) {
                Some(fp) => {
                    key_row.set_subtitle(&fp.display());
                    forget.set_visible(true);
                }
                None => {
                    key_row.set_subtitle("Ещё не известен");
                    forget.set_visible(false);
                }
            }
        }
    };
    show();
    let save = {
        let (window, id, show) = (window.clone(), id.clone(), show.clone());
        move |change: &dyn Fn(&mut Computer)| {
            let Some(mut computer) = window.store().find(&id) else {
                return;
            };
            change(&mut computer);
            if let Err(e) = window.store().put(computer) {
                window.toast(&format!("Не удалось сохранить: {e}"));
            }
            show();
        }
    };

    let (window2, id2) = (window.clone(), id.clone());
    connect.connect_clicked(move |_| {
        if let Some(computer) = window2.store().find(&id2) {
            session::open(&window2, computer);
        }
    });
    let s = save.clone();
    name.connect_apply(move |row| {
        let text = row.text().trim().to_string();
        s(&|c| c.name = text.clone());
    });
    address.connect_changed(|row| {
        check_address(row);
    });
    let (s, window2) = (save.clone(), window.clone());
    address.connect_apply(move |row| {
        let Some((host, port)) = check_address(row) else {
            window2.toast("Не похоже на адрес компьютера");
            return;
        };
        s(&|c| {
            // Another machine may answer at a new address: its key is confirmed again.
            if c.host != host || c.port != port {
                c.fingerprint.clear();
            }
            c.host = host.clone();
            c.port = port;
        });
    });
    let s = save.clone();
    virtual_monitor.connect_active_notify(move |row| {
        let active = row.is_active();
        s(&|c| c.virtual_monitor = active);
    });
    let (s, window2) = (save.clone(), window.clone());
    forget.connect_clicked(move |_| {
        s(&|c| c.fingerprint.clear());
        window2.toast("Отпечаток забыт");
    });
    let window2 = window.clone();
    remove.connect_clicked(move |_| {
        let Some(computer) = window2.store().find(&id) else {
            return;
        };
        let title = computer.title();
        if let Err(e) = window2.store().remove(&computer.id) {
            return window2.toast(&format!("Не удалось удалить: {e}"));
        }
        window2.navigation().pop();
        let back = window2.clone();
        window2.toast_undo(&format!("«{title}» удалён"), move || {
            let _ = back.store().put(computer.clone());
        });
    });
    window.navigation().push(&nav);
}

/// Adds a computer by address; afterwards its own page opens.
pub fn open_new(window: &Window) {
    let page = adw::PreferencesPage::new();
    let (hero, _, _) = pages::hero("list-add-symbolic", "Новый компьютер", "");
    page.add(&pages::bare(&hero));
    let group = adw::PreferencesGroup::new();
    let address = adw::EntryRow::builder()
        .title("Адрес или имя")
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    let name = adw::EntryRow::builder()
        .title("Название (необязательно)")
        .build();
    let virtual_monitor = virtual_row();
    group.add(&address);
    group.add(&name);
    group.add(&virtual_monitor);
    page.add(&group);

    let add = gtk::Button::builder()
        .label("Добавить")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let nav = pages::preferences(
        "Новый компьютер",
        "new-computer",
        &page,
        Some(add.upcast_ref()),
    );
    let a = add.clone();
    address.connect_changed(move |row| {
        a.set_sensitive(check_address(row).is_some());
    });
    let a = add.clone();
    address.connect_entry_activated(move |_| {
        if a.is_sensitive() {
            a.emit_clicked();
        }
    });
    let a = add.clone();
    name.connect_entry_activated(move |_| {
        if a.is_sensitive() {
            a.emit_clicked();
        }
    });
    let (window2, address2) = (window.clone(), address.clone());
    add.connect_clicked(move |_| {
        let Some((host, port)) = parse_address(&address2.text()) else {
            return;
        };
        if let Some(existing) = window2.store().find_by_address(&host, port) {
            window2.navigation().pop();
            window2.toast(&format!("«{}» уже есть в списке", existing.title()));
            return open(&window2, &existing.id);
        }
        let mut computer = Computer::new(&host, port);
        computer.name = name.text().trim().to_string();
        computer.virtual_monitor = virtual_monitor.is_active();
        let id = computer.id.clone();
        if let Err(e) = window2.store().put(computer) {
            return window2.toast(&format!("Не удалось сохранить: {e}"));
        }
        window2.navigation().pop();
        open(&window2, &id);
    });
    window.navigation().push(&nav);
    address.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;
    use casement_net::DEFAULT_PORT;

    #[test]
    fn summaries() {
        let mut computer = Computer::new("10.0.0.5", DEFAULT_PORT);
        assert_eq!(summary(&computer), "ещё не подключались");
        computer.name = "Офис".into();
        computer.fingerprint = "a".repeat(64);
        assert_eq!(summary(&computer), "10.0.0.5");
        computer.name.clear();
        assert_eq!(summary(&computer), "");
        assert_eq!(capitalized("ещё не подключались"), "Ещё не подключались");
        assert_eq!(capitalized(""), "");
    }
}
