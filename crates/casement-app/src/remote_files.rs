//! "Файлы того компьютера": browse the host's shared folder and download into Downloads.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use casement_net::client::FileSender;
use casement_proto::{FileEntry, FileListing};

use crate::core::{describe, run};
use crate::pages;
use crate::window::Window;

/// Where downloads go on this computer.
fn downloads_dir() -> std::path::PathBuf {
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .unwrap_or_else(|| glib::home_dir().join("Downloads"))
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

fn parent(folder: &str) -> String {
    folder
        .rsplit_once('/')
        .map(|(up, _)| up.to_string())
        .unwrap_or_default()
}

fn size_text(size: u64) -> String {
    glib::format_size(size).to_string()
}

fn modified_text(seconds: u64) -> Option<String> {
    let time = glib::DateTime::from_unix_local(i64::try_from(seconds).ok()?).ok()?;
    time.format("%d.%m.%Y %H:%M").ok().map(|s| s.to_string())
}

fn normalize_folder(text: &str) -> Option<String> {
    if text.chars().any(char::is_control) || text.contains('\\') {
        return None;
    }
    let text = text.trim();
    let text = if text == "~" {
        ""
    } else {
        text.strip_prefix("~/").unwrap_or(text)
    };
    let mut parts = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

enum Download {
    Active { id: u64, received: u64, total: u64 },
    Complete(String),
    Failed(String),
}

struct FileWidgets {
    row: adw::ActionRow,
    subtitle: String,
    button: gtk::Button,
    progress: gtk::ProgressBar,
    stack: gtk::Stack,
}

#[derive(Default)]
struct UploadQueue {
    active: Option<PathBuf>,
    pending: VecDeque<PathBuf>,
}

impl UploadQueue {
    fn append(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if self.active.as_ref() != Some(&path) && !self.pending.contains(&path) {
                self.pending.push_back(path);
            }
        }
    }

    fn start_next(&mut self) -> Option<PathBuf> {
        if self.active.is_some() {
            return None;
        }
        let path = self.pending.pop_front()?;
        self.active = Some(path.clone());
        Some(path)
    }

    fn cancel(&mut self) {
        self.active = None;
        self.pending.clear();
    }
}

struct UploadWidgets {
    panel: gtk::Box,
    name: gtk::Label,
    status: gtk::Label,
    waiting: gtk::Label,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
}

struct Browser {
    files: FileSender,
    window: Window,
    path: gtk::Entry,
    up: gtk::Button,
    stack: gtk::Stack,
    list: gtk::ListBox,
    status: adw::StatusPage,
    retry: gtk::Button,
    show_hidden: gtk::ToggleButton,
    folder: RefCell<String>,
    listing: RefCell<Option<FileListing>>,
    downloads: RefCell<BTreeMap<String, Download>>,
    rows: RefCell<BTreeMap<String, FileWidgets>>,
    /// Bumped on every navigation so a slow answer for an old folder is dropped.
    generation: Cell<u64>,
    gone: Cell<bool>,
    next_download: Cell<u64>,
    uploads: RefCell<UploadQueue>,
    upload_widgets: UploadWidgets,
    upload_cancel: RefCell<Option<tokio::sync::oneshot::Sender<()>>>,
    upload_id: Cell<u64>,
    uploaded: Cell<usize>,
    upload_failed: Cell<usize>,
    upload_error: RefCell<Option<String>>,
}

/// Opens the browser as a page over the session; `host` names the other computer.
pub fn open(window: &Window, files: FileSender, host: &str, initial_uploads: Vec<PathBuf>) {
    let title = adw::WindowTitle::new("Файлы", host);
    let home = pages::flat_icon("user-home-symbolic", "Домашняя папка");
    let up = pages::flat_icon("go-up-symbolic", "На уровень выше");
    up.set_sensitive(false);
    let refresh = pages::flat_icon("view-refresh-symbolic", "Обновить");
    let show_hidden = gtk::ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text("Скрытые файлы")
        .build();
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let send = gtk::Button::builder()
        .label("Отправить")
        .css_classes(["suggested-action"])
        .build();
    header.pack_end(&send);
    header.pack_end(&show_hidden);
    let path = gtk::Entry::builder()
        .placeholder_text("Домашняя папка")
        .hexpand(true)
        .width_chars(6)
        .build();
    let location = gtk::Box::builder()
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    location.append(&home);
    location.append(&up);
    location.append(&path);
    location.append(&refresh);

    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .valign(gtk::Align::Start)
        .build();
    let clamp = adw::Clamp::builder()
        .maximum_size(840)
        .tightening_threshold(560)
        .child(&list)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build();
    let retry = gtk::Button::builder()
        .label("Повторить")
        .halign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    let status = adw::StatusPage::builder().child(&retry).build();
    let stack = gtk::Stack::builder().vexpand(true).build();
    stack.add_named(&spinner, Some("loading"));
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&status, Some("status"));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&location);
    content.append(&stack);
    view.set_content(Some(&content));
    let upload_widgets = upload_panel();
    view.add_bottom_bar(&upload_widgets.panel);
    let nav = adw::NavigationPage::builder()
        .title("Файлы")
        .tag("remote-files")
        .child(&view)
        .build();

    let browser = Rc::new(Browser {
        files,
        window: window.clone(),
        path: path.clone(),
        up: up.clone(),
        stack,
        list,
        status,
        retry: retry.clone(),
        show_hidden: show_hidden.clone(),
        folder: RefCell::new(String::new()),
        listing: RefCell::new(None),
        downloads: RefCell::new(BTreeMap::new()),
        rows: RefCell::new(BTreeMap::new()),
        generation: Cell::new(0),
        gone: Cell::new(false),
        next_download: Cell::new(0),
        uploads: RefCell::new(UploadQueue::default()),
        upload_widgets,
        upload_cancel: RefCell::new(None),
        upload_id: Cell::new(0),
        uploaded: Cell::new(0),
        upload_failed: Cell::new(0),
        upload_error: RefCell::new(None),
    });
    let weak = Rc::downgrade(&browser);
    send.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            let weak = Rc::downgrade(&browser);
            crate::local_files::open(&browser.window, move |paths| {
                if let Some(browser) = weak.upgrade() {
                    browser.enqueue(paths);
                }
            });
        }
    });
    let weak = Rc::downgrade(&browser);
    browser.upload_widgets.cancel.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            browser.cancel_uploads();
        }
    });
    let weak = Rc::downgrade(&browser);
    home.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            browser.open_folder(String::new());
        }
    });
    let weak = Rc::downgrade(&browser);
    up.connect_clicked(move |_| {
        if let Some(b) = weak.upgrade() {
            let up = parent(&b.folder.borrow());
            b.open_folder(up);
        }
    });
    for button in [&refresh, &retry] {
        let weak = Rc::downgrade(&browser);
        button.connect_clicked(move |_| {
            if let Some(browser) = weak.upgrade() {
                let folder = browser.folder.borrow().clone();
                browser.open_folder(folder);
            }
        });
    }
    let weak = Rc::downgrade(&browser);
    path.connect_activate(move |entry| {
        if let Some(browser) = weak.upgrade() {
            match normalize_folder(&entry.text()) {
                Some(folder) => browser.open_folder(folder),
                None => {
                    entry.add_css_class("error");
                    browser.window.toast("Неверный путь");
                }
            }
        }
    });
    path.connect_changed(|entry| entry.remove_css_class("error"));
    let weak = Rc::downgrade(&browser);
    show_hidden.connect_toggled(move |_| {
        if let Some(b) = weak.upgrade() {
            b.fill();
        }
    });
    // Covering the page preserves its state. Only removing it releases the owner.
    let keep = RefCell::new(Some(browser.clone()));
    let target = nav.clone();
    let handler = Rc::new(RefCell::new(None));
    let handler2 = handler.clone();
    let id = window
        .navigation()
        .connect_popped(move |navigation, popped| {
            if popped == &target {
                if let Some(browser) = keep.take() {
                    browser.gone.set(true);
                    browser.generation.set(browser.generation.get() + 1);
                }
                if let Some(id) = handler2.take() {
                    navigation.disconnect(id);
                }
            }
        });
    handler.replace(Some(id));
    browser.open_folder(String::new());
    window.navigation().push(&nav);
    browser.enqueue(initial_uploads);
}

fn upload_panel() -> UploadWidgets {
    let name = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["heading"])
        .build();
    let cancel = pages::flat_icon("process-stop-symbolic", "Отменить отправку");
    let heading = gtk::Box::builder().spacing(12).build();
    heading.append(&name);
    heading.append(&cancel);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["session-status"])
        .build();
    let waiting = gtk::Label::builder()
        .visible(false)
        .css_classes(["session-status"])
        .build();
    let details = gtk::Box::builder().spacing(12).build();
    details.append(&status);
    details.append(&waiting);
    let progress = gtk::ProgressBar::builder()
        .css_classes(["transfer-progress"])
        .build();
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(18)
        .margin_end(18)
        .visible(false)
        .css_classes(["transfer-row"])
        .build();
    panel.append(&heading);
    panel.append(&details);
    panel.append(&progress);
    UploadWidgets {
        panel,
        name,
        status,
        waiting,
        progress,
        cancel,
    }
}

impl Browser {
    fn enqueue(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.gone.get() {
            return;
        }
        let idle = {
            let queue = self.uploads.borrow();
            queue.active.is_none() && queue.pending.is_empty()
        };
        if idle {
            self.uploaded.set(0);
            self.upload_failed.set(0);
            self.upload_error.take();
        }
        self.uploads.borrow_mut().append(paths);
        self.update_waiting();
        if self.uploads.borrow().active.is_none() {
            self.start_next_upload();
        }
    }

    fn update_waiting(&self) {
        let count = self.uploads.borrow().pending.len();
        self.upload_widgets
            .waiting
            .set_label(&format!("В очереди: {count}"));
        self.upload_widgets.waiting.set_visible(count > 0);
    }

    fn start_next_upload(self: &Rc<Self>) {
        let path = self.uploads.borrow_mut().start_next();
        let Some(path) = path else {
            if self.uploads.borrow().active.is_none() {
                let (sent, failed) = (self.uploaded.get(), self.upload_failed.get());
                if sent + failed > 0 {
                    let title = if failed == 0 {
                        format!("Отправлено файлов: {sent}")
                    } else {
                        format!("Отправлено: {sent} · Ошибок: {failed}")
                    };
                    let error = self.upload_error.borrow().clone().unwrap_or_default();
                    self.show_upload_result(&title, &error);
                }
            }
            return;
        };
        let id = self.upload_id.get().wrapping_add(1);
        self.upload_id.set(id);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.upload_widgets.name.set_label(&name);
        self.upload_widgets
            .name
            .set_tooltip_text(Some(&path.display().to_string()));
        self.upload_widgets.status.set_label("Отправка…");
        self.upload_widgets.status.set_tooltip_text(None);
        self.upload_widgets.status.set_visible(true);
        self.upload_widgets.progress.set_fraction(0.0);
        self.upload_widgets.progress.set_visible(true);
        self.upload_widgets.cancel.set_visible(true);
        self.upload_widgets.panel.set_visible(true);
        self.update_waiting();

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        self.upload_cancel.replace(Some(cancel_tx));
        let (tx, rx) = async_channel::bounded::<(u64, u64)>(1);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok((sent, total)) = rx.recv().await {
                let Some(browser) = weak.upgrade() else {
                    return;
                };
                if browser.gone.get()
                    || browser.upload_id.get() != id
                    || browser.uploads.borrow().active.is_none()
                {
                    continue;
                }
                let fraction = if total == 0 {
                    0.0
                } else {
                    sent as f64 / total as f64
                };
                browser
                    .upload_widgets
                    .progress
                    .set_fraction(fraction.clamp(0.0, 1.0));
                browser.upload_widgets.status.set_label(&format!(
                    "{} / {} · {}%",
                    size_text(sent),
                    size_text(total),
                    (fraction * 100.0) as u32,
                ));
            }
        });
        let files = self.files.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = run(async move {
                let mut last = std::time::Instant::now();
                tokio::select! {
                    biased;
                    _ = cancel_rx => None,
                    result = files.send(&path, move |sent, total| {
                        if last.elapsed().as_millis() >= 100 || sent == total {
                            last = std::time::Instant::now();
                            let _ = tx.try_send((sent, total));
                        }
                    }) => Some(result),
                }
            })
            .await;
            if this.upload_id.get() != id {
                return;
            }
            this.upload_cancel.take();
            this.uploads.borrow_mut().active = None;
            match result {
                Some(Ok(_)) => this.uploaded.set(this.uploaded.get() + 1),
                Some(Err(error)) => {
                    this.upload_failed.set(this.upload_failed.get() + 1);
                    if this.upload_error.borrow().is_none() {
                        this.upload_error
                            .replace(Some(format!("«{name}»: {}", describe(&error))));
                    }
                }
                None => {
                    this.cancel_uploads();
                    return;
                }
            }
            this.start_next_upload();
        });
    }

    fn cancel_uploads(self: &Rc<Self>) {
        self.uploads.borrow_mut().cancel();
        self.upload_id.set(self.upload_id.get().wrapping_add(1));
        if let Some(cancel) = self.upload_cancel.take() {
            let _ = cancel.send(());
        }
        let detail = if self.uploaded.get() > 0 {
            format!("Отправлено файлов: {}", self.uploaded.get())
        } else {
            String::new()
        };
        self.show_upload_result("Отправка отменена", &detail);
    }

    fn show_upload_result(self: &Rc<Self>, title: &str, detail: &str) {
        if self.gone.get() {
            self.window.toast(title);
            return;
        }
        self.upload_widgets.name.set_label(title);
        self.upload_widgets.name.set_tooltip_text(None);
        self.upload_widgets.status.set_label(detail);
        self.upload_widgets.status.set_tooltip_text(Some(detail));
        self.upload_widgets.status.set_visible(!detail.is_empty());
        self.upload_widgets.waiting.set_visible(false);
        self.upload_widgets.progress.set_visible(false);
        self.upload_widgets.cancel.set_visible(false);
        self.upload_widgets.panel.set_visible(true);
        let id = self.upload_id.get();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || {
            if let Some(browser) = weak.upgrade() {
                if browser.upload_id.get() == id && browser.uploads.borrow().active.is_none() {
                    browser.upload_widgets.panel.set_visible(false);
                }
            }
        });
    }

    fn open_folder(self: &Rc<Self>, folder: String) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.folder.replace(folder.clone());
        self.listing.replace(None);
        self.rows.borrow_mut().clear();
        self.up.set_sensitive(!folder.is_empty());
        self.path.set_text(&if folder.is_empty() {
            String::new()
        } else {
            format!("~/{folder}")
        });
        self.stack.set_visible_child_name("loading");
        let files = self.files.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let path = folder.clone();
            let result = run(async move { files.list(&path).await }).await;
            if this.gone.get() || this.generation.get() != generation {
                return;
            }
            match result {
                Ok(mut listing) => {
                    sort_entries(&mut listing.entries);
                    this.listing.replace(Some(listing));
                    this.fill();
                }
                Err(e) => {
                    this.listing.replace(None);
                    this.show_status(
                        "dialog-error-symbolic",
                        "Папка недоступна",
                        &describe(&e),
                        true,
                    );
                }
            }
        });
    }

    fn show_status(&self, icon: &str, title: &str, description: &str, retry: bool) {
        self.status.set_icon_name(Some(icon));
        self.status.set_title(title);
        self.status
            .set_description(Some(glib::markup_escape_text(description).as_str()));
        self.retry.set_visible(retry);
        self.stack.set_visible_child_name("status");
    }

    fn fill(self: &Rc<Self>) {
        let listing = self.listing.borrow();
        let Some(listing) = listing.as_ref() else {
            return;
        };
        self.rows.borrow_mut().clear();
        self.list.remove_all();
        let hidden = self.show_hidden.is_active();
        let mut shown = 0;
        for entry in &listing.entries {
            if !hidden && entry.name.starts_with('.') {
                continue;
            }
            self.list.append(&self.row(entry));
            shown += 1;
        }
        if listing.truncated {
            let more = adw::ActionRow::builder()
                .title("Показаны не все файлы")
                .css_classes(["dim-label"])
                .build();
            self.list.append(&more);
        }
        if shown == 0 && !listing.truncated {
            self.show_status("folder-symbolic", "Папка пуста", "", false);
        } else {
            self.stack.set_visible_child_name("list");
        }
    }

    fn row(self: &Rc<Self>, entry: &FileEntry) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&entry.name).as_str())
            .title_lines(1)
            .subtitle_lines(1)
            .activatable(true)
            .css_classes(["file-row"])
            .build();
        let path = join(&self.folder.borrow(), &entry.name);
        row.add_prefix(&pages::icon_tile(if entry.is_dir {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        }));
        if entry.is_dir {
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.open_folder(path.clone());
                }
            });
        } else {
            let mut subtitle = size_text(entry.size);
            if let Some(when) = modified_text(entry.modified) {
                subtitle = format!("{subtitle} · {when}");
            }
            let button = pages::flat_icon("folder-download-symbolic", "Скачать");
            let progress = gtk::ProgressBar::builder()
                .width_request(64)
                .valign(gtk::Align::Center)
                .build();
            let stack = gtk::Stack::builder().valign(gtk::Align::Center).build();
            stack.add_named(&button, Some("action"));
            stack.add_named(&progress, Some("progress"));
            row.add_suffix(&stack);
            row.set_activatable_widget(Some(&button));
            let weak = Rc::downgrade(self);
            let download_path = path.clone();
            let name = entry.name.clone();
            button.connect_clicked(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.download(download_path.clone(), name.clone());
                }
            });
            self.rows.borrow_mut().insert(
                path.clone(),
                FileWidgets {
                    row: row.clone(),
                    subtitle,
                    button,
                    progress,
                    stack,
                },
            );
            self.update_download(&path);
        }
        row
    }

    fn update_download(&self, path: &str) {
        let rows = self.rows.borrow();
        let Some(widgets) = rows.get(path) else {
            return;
        };
        widgets.button.remove_css_class("error");
        match self.downloads.borrow().get(path) {
            Some(Download::Active {
                received, total, ..
            }) => {
                let fraction = if *total == 0 {
                    0.0
                } else {
                    *received as f64 / *total as f64
                };
                widgets.progress.set_fraction(fraction.clamp(0.0, 1.0));
                widgets.row.set_subtitle(&format!(
                    "{} · {}%",
                    widgets.subtitle,
                    (fraction * 100.0) as u32
                ));
                widgets.button.set_sensitive(false);
                widgets.stack.set_visible_child_name("progress");
            }
            Some(Download::Complete(saved)) => {
                widgets
                    .row
                    .set_subtitle(&format!("{} · Скачано", widgets.subtitle));
                widgets.button.set_icon_name("emblem-ok-symbolic");
                widgets
                    .button
                    .set_tooltip_text(Some(&format!("{saved} · Скачать ещё раз")));
                widgets.button.set_sensitive(true);
                widgets.stack.set_visible_child_name("action");
            }
            Some(Download::Failed(message)) => {
                widgets
                    .row
                    .set_subtitle(&format!("{} · Ошибка", widgets.subtitle));
                widgets.button.set_icon_name("view-refresh-symbolic");
                widgets.button.set_tooltip_text(Some(message));
                widgets.button.add_css_class("error");
                widgets.button.set_sensitive(true);
                widgets.stack.set_visible_child_name("action");
            }
            None => {
                widgets.row.set_subtitle(&widgets.subtitle);
                widgets.button.set_icon_name("folder-download-symbolic");
                widgets.button.set_tooltip_text(Some("Скачать"));
                widgets.button.set_sensitive(true);
                widgets.stack.set_visible_child_name("action");
            }
        }
    }

    fn download(self: &Rc<Self>, path: String, name: String) {
        if matches!(
            self.downloads.borrow().get(&path),
            Some(Download::Active { .. })
        ) {
            return;
        }
        let id = self.next_download.get().wrapping_add(1);
        self.next_download.set(id);
        self.downloads.borrow_mut().insert(
            path.clone(),
            Download::Active {
                id,
                received: 0,
                total: 0,
            },
        );
        self.update_download(&path);
        let (tx, rx) = async_channel::bounded::<(u64, u64)>(1);
        let weak = Rc::downgrade(self);
        let progress_path = path.clone();
        glib::spawn_future_local(async move {
            while let Ok((received, total)) = rx.recv().await {
                let Some(browser) = weak.upgrade() else {
                    return;
                };
                // A queued final progress update must not replace the completed state.
                let mut downloads = browser.downloads.borrow_mut();
                if matches!(downloads.get(&progress_path), Some(Download::Active { id: current, .. }) if *current == id)
                {
                    downloads.insert(
                        progress_path.clone(),
                        Download::Active {
                            id,
                            received,
                            total,
                        },
                    );
                }
                drop(downloads);
                if !browser.gone.get() {
                    browser.update_download(&progress_path);
                }
            }
        });
        let files = self.files.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let dir = downloads_dir();
            let remote_path = path.clone();
            let result = run(async move {
                let mut last = std::time::Instant::now();
                files
                    .fetch(&remote_path, &dir, move |received, total| {
                        if last.elapsed().as_millis() >= 100 || received == total {
                            last = std::time::Instant::now();
                            let _ = tx.try_send((received, total));
                        }
                    })
                    .await
            })
            .await;
            if !matches!(this.downloads.borrow().get(&path), Some(Download::Active { id: current, .. }) if *current == id)
            {
                return;
            }
            match result {
                Ok(saved) => {
                    let saved_name = saved
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(name);
                    this.downloads
                        .borrow_mut()
                        .insert(path.clone(), Download::Complete(saved_name.clone()));
                    this.window.toast(&format!("Скачано: {saved_name}"));
                }
                Err(error) => {
                    let message = describe(&error);
                    this.downloads
                        .borrow_mut()
                        .insert(path.clone(), Download::Failed(message.clone()));
                    this.window.toast(&format!("«{name}»: {message}"));
                }
            }
            if !this.gone.get() {
                this.update_download(&path);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a/b", "c"), "a/b/c");
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
        assert_eq!(parent(""), "");
        assert_eq!(normalize_folder("~/a/./b/"), Some("a/b".into()));
        assert_eq!(normalize_folder("/"), Some(String::new()));
        assert_eq!(normalize_folder("~"), Some(String::new()));
        assert_eq!(normalize_folder("~name"), Some("~name".into()));
        for invalid in ["../a", "a/../b", "a\\b", "a\0b"] {
            assert_eq!(normalize_folder(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn folders_precede_files_and_names_sort_consistently() {
        let mut entries = vec![
            FileEntry {
                name: "я.txt".into(),
                is_dir: false,
                ..Default::default()
            },
            FileEntry {
                name: "Б".into(),
                is_dir: true,
                ..Default::default()
            },
            FileEntry {
                name: "а".into(),
                is_dir: true,
                ..Default::default()
            },
            FileEntry {
                name: "А.txt".into(),
                is_dir: false,
                ..Default::default()
            },
        ];
        sort_entries(&mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["а", "Б", "А.txt", "я.txt"]
        );
    }

    #[test]
    fn upload_queue_is_sequential_and_deduplicates_waiting_and_active_paths() {
        let (a, b, c) = (PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c"));
        let mut queue = UploadQueue::default();
        queue.append(vec![a.clone(), b.clone(), a.clone()]);
        assert_eq!(queue.start_next(), Some(a.clone()));
        assert_eq!(queue.start_next(), None);
        queue.append(vec![a, b.clone(), c.clone()]);
        assert_eq!(
            queue.pending.iter().cloned().collect::<Vec<_>>(),
            [b.clone(), c.clone()]
        );
        queue.active = None;
        assert_eq!(queue.start_next(), Some(b));
        queue.cancel();
        assert!(queue.active.is_none());
        assert!(queue.pending.is_empty());
        queue.append(vec![c.clone()]);
        assert_eq!(queue.start_next(), Some(c));
    }
}
