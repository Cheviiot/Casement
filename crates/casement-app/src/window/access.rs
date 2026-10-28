//! Local access belongs to Settings, separate from the connection list.

use super::*;

impl Window {
    pub(super) fn initialize_access(&self) {
        if self.0.preview {
            self.set_accept_quietly(!self.0.core.dir.join(ACCEPT_OFF).exists());
            return;
        }
        if self.0.service {
            self.0.accept.set_sensitive(false);
            let this = self.clone();
            glib::spawn_future_local(async move {
                service::ensure_running().await;
                this.set_accept_quietly(service::enabled().await);
                this.0.accept.set_sensitive(true);
            });
        } else if !self.0.core.dir.join(ACCEPT_OFF).exists() {
            self.toggle_host(true);
        } else {
            self.refresh_access();
        }
    }

    fn set_accept_quietly(&self, active: bool) {
        self.0.updating_accept.set(true);
        self.0.accept.set_active(active);
        self.0.updating_accept.set(false);
        self.refresh_access();
    }

    pub(super) fn open_settings(&self) {
        self.refresh_access();
        let page = self
            .0
            .settings_page
            .borrow_mut()
            .get_or_insert_with(|| self.build_settings())
            .clone();
        self.0.navigation.push(&page);
    }

    fn build_settings(&self) -> adw::NavigationPage {
        let page = adw::PreferencesPage::new();
        let (identity, _, status) = pages::hero("computer-symbolic", &self.0.core.name, "");
        status.set_visible(true);
        self.0
            .access_status
            .bind_property("label", &status, "label")
            .sync_create()
            .build();
        page.add(&pages::bare(&identity));

        let access = adw::PreferencesGroup::builder().title("Доступ").build();
        access.add(&self.0.accept);
        access.add(&self.0.password_row);
        access.add(&self.0.trusted_row);
        if self.0.machine {
            let configure = link_row(
                "emblem-system-symbolic",
                "Настроить доступ",
                "От имени администратора",
            );
            let this = self.clone();
            configure.connect_activated(move |_| this.configure_machine());
            access.add(&configure);
        }
        page.add(&access);

        let identity = adw::PreferencesGroup::builder().title("Компьютер").build();
        let fingerprint = self.local_fingerprint();
        let hex = fingerprint.replace('-', "").to_lowercase();
        if hex.len() >= 8 {
            let address = format!("casement-{}.local", &hex[..8]);
            let row = adw::ActionRow::builder()
                .title("Адрес в локальной сети")
                .subtitle(&address)
                .subtitle_lines(1)
                .build();
            let copy = flat_icon("edit-copy-symbolic", "Скопировать адрес");
            let this = self.clone();
            copy.connect_clicked(move |button| {
                button.clipboard().set_text(&address);
                this.toast("Адрес скопирован");
            });
            row.add_suffix(&copy);
            identity.add(&row);
        }
        let key = link_row("dialog-password-symbolic", "Ключ устройства", "");
        let this = self.clone();
        key.connect_activated(move |_| this.open_local_key());
        identity.add(&key);
        page.add(&identity);

        let files = adw::PreferencesGroup::builder().title("Файлы").build();
        let (downloads, shared) = if self.0.machine {
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
        for (title, path) in [("Полученные файлы", downloads), ("Открытая папка", shared)]
        {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(glib::markup_escape_text(&path.display().to_string()).as_str())
                .subtitle_lines(1)
                .build();
            row.add_css_class("property");
            files.add(&row);
        }
        page.add(&files);
        let other = adw::PreferencesGroup::new();
        let about = link_row("help-about-symbolic", "О программе", "");
        let this = self.clone();
        about.connect_activated(move |_| this.open_about());
        other.add(&about);
        page.add(&other);
        pages::preferences("Настройки", "settings", &page, None)
    }

    fn local_fingerprint(&self) -> String {
        if self.0.machine {
            service::machine_access().map_or_else(|| "—".into(), |m| m.fingerprint)
        } else {
            self.0.core.identity.fingerprint().display()
        }
    }

    fn open_local_key(&self) {
        let page = adw::PreferencesPage::new();
        let fingerprint = self.local_fingerprint();
        page.add(&pages::bare(&pages::fingerprint_label(&fingerprint)));
        let copy = pages::pill("Скопировать", "suggested-action");
        let this = self.clone();
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&fingerprint);
            this.toast("Ключ скопирован");
        });
        page.add(&pages::bare(&copy));
        self.0.navigation.push(&pages::preferences(
            "Ключ устройства",
            "local-key",
            &page,
            None,
        ));
    }

    fn password_set(&self) -> bool {
        if self.0.machine {
            service::machine_access().is_some_and(|m| m.password)
        } else {
            has_password(&self.0.core.dir)
        }
    }

    pub(super) fn refresh_access(&self) {
        let password = self.password_set();
        self.0.password_row.set_subtitle(if password {
            "Задан"
        } else {
            "Не задан"
        });
        let store = if self.0.machine {
            TrustStore::default()
        } else {
            TrustStore::load(&self.0.core.dir).unwrap_or_default()
        };
        let count = if self.0.machine {
            service::machine_access().map_or(0, |m| m.trusted)
        } else {
            store.devices().len()
        };
        self.0.trusted_row.set_subtitle(&if count == 0 {
            "Нет устройств".into()
        } else {
            count.to_string()
        });
        self.0
            .access_status
            .set_label(if !self.0.accept.is_active() {
                "Приём выключен"
            } else if !password {
                "Только доверенные устройства"
            } else {
                "Приём включён"
            });
        if self.0.trusted_stack.child_by_name("list").is_some() {
            self.0
                .trusted_stack
                .set_visible_child_name(if count == 0 { "empty" } else { "list" });
        }
        self.0.trusted_list.remove_all();
        for (fingerprint, name) in store.devices() {
            let name = if name.is_empty() {
                "Без имени".to_string()
            } else {
                name.clone()
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .subtitle(fingerprint.display())
                .title_lines(1)
                .subtitle_lines(1)
                .build();
            row.add_prefix(&pages::icon_tile("computer-symbolic"));
            let remove = flat_icon("user-trash-symbolic", "Отозвать доверие");
            let (this, fingerprint) = (self.clone(), *fingerprint);
            remove.connect_clicked(move |_| this.distrust(fingerprint, name.clone()));
            row.add_suffix(&remove);
            self.0.trusted_list.append(&row);
        }
    }

    fn distrust(&self, fingerprint: casement_net::Fingerprint, name: String) {
        let dir = self.0.core.dir.clone();
        let result = TrustStore::load(&dir).and_then(|mut store| store.remove(&fingerprint));
        if let Err(e) = result {
            return self.toast(&format!("Не удалось отозвать доверие: {e}"));
        }
        self.refresh_access();
        let this = self.clone();
        self.toast_undo(
            &format!("Доверие к «{name}» отозвано"),
            move || match TrustStore::load(&dir).and_then(|mut s| s.add(fingerprint, &name)) {
                Ok(()) => this.refresh_access(),
                Err(e) => this.toast(&format!("Не удалось восстановить доверие: {e}")),
            },
        );
    }

    pub(super) fn open_trusted(&self) {
        let page = self
            .0
            .trusted_page
            .borrow_mut()
            .get_or_insert_with(|| {
                self.0
                    .trusted_stack
                    .add_named(&pages::scrolled(&self.0.trusted_list, 720), Some("list"));
                self.0.trusted_stack.add_named(
                    &adw::StatusPage::builder()
                        .icon_name("security-high-symbolic")
                        .title("Доверенных устройств нет")
                        .build(),
                    Some("empty"),
                );
                pages::frame(
                    "Доверенные устройства",
                    "trusted",
                    &self.0.trusted_stack,
                    None,
                )
            })
            .clone();
        self.refresh_access();
        self.0.navigation.push(&page);
    }

    pub(super) fn open_password(&self) {
        super::password::open(self);
    }

    pub(super) fn configure_machine(&self) {
        if self.0.accept_busy.replace(true) {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = service::configure_machine().await {
                this.toast(&format!("Не удалось настроить доступ: {e}"));
            }
            this.refresh_access();
            glib::timeout_future_seconds(3).await;
            this.refresh_access();
            this.0.accept_busy.set(false);
        });
    }

    pub(super) fn toggle_host(&self, active: bool) {
        if self.0.accept_busy.replace(true) {
            return;
        }
        self.0.accept.set_sensitive(false);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = if this.0.preview {
                Ok(())
            } else if this.0.service {
                service::set_enabled(active).await
            } else {
                this.set_in_app_access(active).await
            };
            match result {
                Ok(()) => this.set_accept_quietly(active),
                Err(e) => {
                    this.set_accept_quietly(!active);
                    this.toast(&format!("Не удалось изменить доступ: {e}"));
                }
            }
            this.0.accept_busy.set(false);
            this.0.accept.set_sensitive(true);
        });
    }

    async fn set_in_app_access(&self, active: bool) -> Result<(), String> {
        if active && self.0.host.borrow().is_none() {
            self.start_host().await?;
        }
        let marker = self.0.core.dir.join(ACCEPT_OFF);
        let result = if active {
            match std::fs::remove_file(&marker) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        } else {
            std::fs::write(&marker, b"")
        };
        if let Err(e) = result {
            if active {
                if let Some(running) = self.0.host.take() {
                    running.host.close();
                }
            }
            return Err(e.to_string());
        }
        if !active {
            if let Some(running) = self.0.host.take() {
                running.host.close();
            }
        }
        Ok(())
    }

    async fn start_host(&self) -> Result<(), String> {
        let (dir, name) = (self.0.core.dir.clone(), self.0.core.name.clone());
        let files = casement_net::transfer::HostFiles {
            incoming: Some(
                glib::user_special_dir(glib::UserDirectory::Downloads)
                    .unwrap_or_else(|| glib::home_dir().join("Downloads")),
            ),
            shared: Some(glib::home_dir()),
        };
        let mut running = run(async move {
            tokio::task::spawn_blocking(move || {
                casement_media::service::start_host(&dir, &name, DEFAULT_PORT, true, false, files)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        })
        .await?;
        if let Some(error) = &running.screen_error {
            self.toast(&format!("Экран недоступен: {error}"));
        }
        let runner = running.host.clone();
        runtime().spawn(async move { runner.run().await });
        let mut events = std::mem::replace(
            &mut running.events,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        self.0.host.replace(Some(running));
        let (tx, rx) = async_channel::bounded::<String>(16);
        runtime().spawn(async move {
            while let Some(event) = events.recv().await {
                let text = match event {
                    HostEvent::LoggedIn { name, .. } => format!("Подключился «{name}»"),
                    HostEvent::FileReceived { path, .. } => format!(
                        "Получен «{}»",
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
        Ok(())
    }

    pub(super) fn open_about(&self) {
        let page = adw::PreferencesPage::new();
        let (identity, _, _) = pages::hero(
            "network-workgroup-symbolic",
            "Casement",
            env!("CARGO_PKG_VERSION"),
        );
        page.add(&pages::bare(&identity));
        let group = adw::PreferencesGroup::new();
        for (name, value) in [("Разработчик", "cheviiot"), ("Лицензия", "MIT")] {
            group.add(
                &adw::ActionRow::builder()
                    .title(name)
                    .subtitle(value)
                    .build(),
            );
        }
        page.add(&group);
        page.add(&pages::bare(&gtk::LinkButton::with_label(
            "https://github.com/Cheviiot/Casement",
            "Исходный код",
        )));
        self.0
            .navigation
            .push(&pages::preferences("О программе", "about", &page, None));
    }
}
