use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;

use casement_net::auth::StoredPassword;

use super::{has_password, Window};
use crate::core;
use crate::pages;

pub(super) fn open(window: &Window) {
    let remove_password = match has_password(&window.0.core.dir) {
        Ok(has_password) => has_password,
        Err(_) => {
            window.toast("Не удалось прочитать пароль доступа");
            return;
        }
    };
    let content = adw::PreferencesPage::new();
    let first = adw::PasswordEntryRow::builder()
        .title("Новый пароль")
        .build();
    let second = adw::PasswordEntryRow::builder()
        .title("Повторите пароль")
        .build();
    let entries = adw::PreferencesGroup::new();
    entries.add(&first);
    entries.add(&second);
    content.add(&entries);

    let validation = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["validation", "dim-label"])
        .build();
    content.add(&pages::bare(&validation));
    let save = gtk::Button::builder()
        .label("Сохранить")
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    let page = pages::preferences(
        "Пароль доступа",
        "password",
        &content,
        Some(save.upcast_ref()),
    );

    let remove = if remove_password {
        let remove = pages::pill("Убрать пароль", "destructive-action");
        content.add(&pages::bare(&remove));
        Some(remove)
    } else {
        None
    };

    let controller = Rc::new(PasswordPage {
        window: window.clone(),
        nav: window.navigation(),
        page,
        first,
        second,
        validation,
        save,
        remove,
        busy: Cell::new(false),
        closed: Cell::new(false),
        popped_handler: RefCell::new(None),
    });
    controller.connect_signals();
    let keep_alive = controller.clone();
    let handler = controller.nav.connect_popped(move |nav, popped| {
        if popped == &keep_alive.page {
            keep_alive.close();
            if let Some(handler) = keep_alive.popped_handler.borrow_mut().take() {
                nav.disconnect(handler);
            }
        }
    });
    controller.popped_handler.replace(Some(handler));
    controller.validate();
    controller.nav.push(&controller.page);
    controller.first.grab_focus();
}

struct PasswordPage {
    window: Window,
    nav: adw::NavigationView,
    page: adw::NavigationPage,
    first: adw::PasswordEntryRow,
    second: adw::PasswordEntryRow,
    validation: gtk::Label,
    save: gtk::Button,
    remove: Option<gtk::Button>,
    busy: Cell<bool>,
    closed: Cell<bool>,
    popped_handler: RefCell<Option<glib::SignalHandlerId>>,
}

impl PasswordPage {
    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.first.connect_changed(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.validate();
            }
        });

        let weak = Rc::downgrade(self);
        self.second.connect_changed(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.validate();
            }
        });

        let weak = Rc::downgrade(self);
        self.second.connect_entry_activated(move |_| {
            if let Some(controller) = weak.upgrade() {
                if controller.save.is_sensitive() {
                    controller.save.emit_clicked();
                }
            }
        });

        let weak = Rc::downgrade(self);
        self.save.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.save_password();
            }
        });

        if let Some(remove) = &self.remove {
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(controller) = weak.upgrade() {
                    controller.remove_password();
                }
            });
        }
    }

    fn validate(&self) {
        if self.closed.get() {
            self.save.set_sensitive(false);
            return;
        }
        let text = self.first.text();
        let length = text.chars().count();
        let same = text == self.second.text();
        let valid = length >= 8 && text.len() <= casement_net::auth::MAX_PASSWORD_BYTES && same;
        self.save.set_sensitive(valid && !self.busy.get());
        let message = if text.is_empty() || length < 8 {
            "От 8 символов"
        } else if text.len() > casement_net::auth::MAX_PASSWORD_BYTES {
            "Пароль слишком длинный"
        } else if !self.second.text().is_empty() && !same {
            "Пароли не совпадают"
        } else {
            ""
        };
        self.validation.set_label(message);
        if !self.second.text().is_empty() && !same {
            self.second.add_css_class("error");
        } else {
            self.second.remove_css_class("error");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.first.set_sensitive(!busy && !self.closed.get());
        self.second.set_sensitive(!busy && !self.closed.get());
        if busy {
            self.save.set_sensitive(false);
        } else {
            self.validate();
        }
        if let Some(remove) = &self.remove {
            remove.set_sensitive(!busy && !self.closed.get());
        }
    }

    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.first.set_text("");
        self.second.set_text("");
        self.first.set_sensitive(false);
        self.second.set_sensitive(false);
        self.save.set_sensitive(false);
        if let Some(remove) = &self.remove {
            remove.set_sensitive(false);
        }
        self.validation.set_label("");
        self.validation.remove_css_class("error");
        if let Some(handler) = self.popped_handler.borrow_mut().take() {
            self.nav.disconnect(handler);
        }
    }

    fn save_password(self: &Rc<Self>) {
        if self.closed.get() || self.busy.get() {
            return;
        }
        self.validate();
        if !self.save.is_sensitive() {
            return;
        }
        self.set_busy(true);
        self.save.set_label("Сохранение…");

        let password = self.first.text().to_string();
        let dir = self.window.0.core.dir.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let created = core::run(async move {
                tokio::task::spawn_blocking(move || StoredPassword::create(&password))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()))
            })
            .await;

            let Some(controller) = weak.upgrade() else {
                return;
            };
            if controller.closed.get() {
                return;
            }
            match created {
                Err(error) => controller.save_failed(error),
                Ok(password) => {
                    drop(controller);
                    let result = core::run(async move {
                        tokio::task::spawn_blocking(move || password.save(&dir))
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| result.map_err(|error| error.to_string()))
                    })
                    .await;
                    if let Some(controller) = weak.upgrade() {
                        if !controller.closed.get() {
                            match result {
                                Ok(()) => controller.save_succeeded(),
                                Err(error) => controller.save_failed(error),
                            }
                        }
                    }
                }
            }
        });
    }

    fn save_failed(&self, error: String) {
        if self.closed.get() {
            return;
        }
        self.set_busy(false);
        self.save.set_label("Сохранить");
        self.window.toast(&format!("Пароль не сохранён: {error}"));
    }

    fn save_succeeded(&self) {
        if self.closed.get() {
            return;
        }
        self.set_busy(false);
        self.save.set_label("Сохранить");
        self.first.set_text("");
        self.second.set_text("");
        self.window.refresh_access();
        if self.nav.visible_page().as_ref() == Some(&self.page) {
            self.nav.pop();
        }
        self.window.toast("Пароль сохранён");
    }

    fn remove_password(self: &Rc<Self>) {
        if self.closed.get() || self.busy.get() {
            return;
        }
        self.set_busy(true);
        let dir = self.window.0.core.dir.clone();
        let weak = Rc::downgrade(self);
        let weak_window = Rc::downgrade(&self.window.0);
        glib::spawn_future_local(async move {
            let removed = core::run(async move {
                tokio::task::spawn_blocking(move || {
                    let previous = StoredPassword::load(&dir).map_err(|error| error.to_string())?;
                    StoredPassword::remove(&dir).map_err(|error| error.to_string())?;
                    Ok::<_, String>(previous)
                })
                .await
                .map_err(|error| error.to_string())?
            })
            .await;

            if let Some(controller) = weak.upgrade() {
                if !controller.closed.get() {
                    controller.set_busy(false);
                    if let Err(error) = &removed {
                        controller
                            .window
                            .toast(&format!("Не удалось убрать пароль: {error}"));
                        return;
                    }
                }
            }

            match removed {
                Ok(previous) => {
                    let Some(inner) = weak_window.upgrade() else {
                        return;
                    };
                    let window = Window(inner);
                    window.refresh_access();
                    if let Some(controller) = weak.upgrade() {
                        if !controller.closed.get()
                            && controller.nav.visible_page().as_ref() == Some(&controller.page)
                        {
                            controller.nav.pop();
                        }
                    }
                    if let Some(previous) = previous {
                        let weak_window = Rc::downgrade(&window.0);
                        let dir = window.0.core.dir.clone();
                        window.toast_undo("Пароль убран", move || {
                            restore_password(weak_window.clone(), dir.clone(), previous.clone());
                        });
                    } else {
                        window.toast("Пароль убран");
                    }
                }
                Err(error) => {
                    if let Some(inner) = weak_window.upgrade() {
                        Window(inner).toast(&format!("Не удалось убрать пароль: {error}"));
                    }
                }
            }
        });
    }
}

fn restore_password(window: Weak<super::Inner>, dir: std::path::PathBuf, previous: StoredPassword) {
    glib::spawn_future_local(async move {
        let restored = core::run(async move {
            tokio::task::spawn_blocking(move || {
                if StoredPassword::load(&dir)
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    return Ok::<_, String>(false);
                }
                previous
                    .save(&dir)
                    .map(|()| true)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())?
        })
        .await;

        if let Some(inner) = window.upgrade() {
            let window = Window(inner);
            match restored {
                Ok(true) => window.refresh_access(),
                Ok(false) => {}
                Err(error) => {
                    window.toast(&format!("Не удалось восстановить пароль: {error}"));
                }
            }
        }
    });
}
