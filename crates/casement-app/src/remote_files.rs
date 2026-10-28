//! "Файлы того компьютера": browse the host's shared folder and download into Downloads.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use casement_net::client::FileSender;
use casement_proto::{FileEntry, FileListing};

use crate::core::{describe, run};
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
    let time = glib::DateTime::from_unix_local(seconds as i64).ok()?;
    time.format("%d.%m.%Y %H:%M").ok().map(|s| s.to_string())
}

struct Browser {
    files: FileSender,
    window: Window,
    title: adw::WindowTitle,
    up: gtk::Button,
    stack: gtk::Stack,
    list: gtk::ListBox,
    status: adw::StatusPage,
    show_hidden: gtk::ToggleButton,
    folder: RefCell<String>,
    listing: RefCell<Option<FileListing>>,
    /// Bumped on every navigation so a slow answer for an old folder is dropped.
    generation: std::cell::Cell<u64>,
}

/// Opens the browser as a page over the session; `host` names the other computer.
pub fn open(window: &Window, files: FileSender, host: &str) {
    let title = adw::WindowTitle::new(host, "Домашняя папка");
    let up = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("На уровень выше")
        .sensitive(false)
        .build();
    let show_hidden = gtk::ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text("Показывать скрытые файлы")
        .build();
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    header.pack_start(&up);
    header.pack_end(&show_hidden);

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
        .child(&list)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build();
    let status = adw::StatusPage::new();
    let stack = gtk::Stack::new();
    stack.add_named(&spinner, Some("loading"));
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&status, Some("status"));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    let nav = adw::NavigationPage::builder()
        .title("Файлы того компьютера")
        .tag("remote-files")
        .child(&view)
        .build();

    let browser = Rc::new(Browser {
        files,
        window: window.clone(),
        title,
        up: up.clone(),
        stack,
        list,
        status,
        show_hidden: show_hidden.clone(),
        folder: RefCell::new(String::new()),
        listing: RefCell::new(None),
        generation: std::cell::Cell::new(0),
    });
    let weak = Rc::downgrade(&browser);
    up.connect_clicked(move |_| {
        if let Some(b) = weak.upgrade() {
            let up = parent(&b.folder.borrow());
            b.open_folder(up);
        }
    });
    let weak = Rc::downgrade(&browser);
    show_hidden.connect_toggled(move |_| {
        if let Some(b) = weak.upgrade() {
            b.fill();
        }
    });
    // The page keeps the browser alive while it is in the stack.
    let keep = RefCell::new(Some(browser.clone()));
    nav.connect_hidden(move |_| {
        keep.take();
    });
    browser.open_folder(String::new());
    window.navigation().push(&nav);
}

impl Browser {
    fn open_folder(self: &Rc<Self>, folder: String) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.folder.replace(folder.clone());
        self.up.set_sensitive(!folder.is_empty());
        self.title.set_subtitle(&if folder.is_empty() {
            "Домашняя папка".to_string()
        } else {
            format!("~/{folder}")
        });
        self.stack.set_visible_child_name("loading");
        let files = self.files.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let path = folder.clone();
            let result = run(async move { files.list(&path).await }).await;
            if this.generation.get() != generation {
                return;
            }
            match result {
                Ok(listing) => {
                    this.listing.replace(Some(listing));
                    this.fill();
                }
                Err(e) => {
                    this.listing.replace(None);
                    this.show_status(
                        "dialog-error-symbolic",
                        "Не удалось открыть папку",
                        &describe(&e),
                    );
                }
            }
        });
    }

    fn show_status(&self, icon: &str, title: &str, description: &str) {
        self.status.set_icon_name(Some(icon));
        self.status.set_title(title);
        self.status
            .set_description(Some(glib::markup_escape_text(description).as_str()));
        self.stack.set_visible_child_name("status");
    }

    fn fill(self: &Rc<Self>) {
        let listing = self.listing.borrow();
        let Some(listing) = listing.as_ref() else {
            return;
        };
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
                .title("Показаны не все файлы: в папке их слишком много")
                .css_classes(["dim-label"])
                .build();
            self.list.append(&more);
        }
        if shown == 0 && !listing.truncated {
            self.show_status("folder-symbolic", "Папка пуста", "");
        } else {
            self.stack.set_visible_child_name("list");
        }
    }

    fn row(self: &Rc<Self>, entry: &FileEntry) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&entry.name).as_str())
            .activatable(true)
            .build();
        let path = join(&self.folder.borrow(), &entry.name);
        if entry.is_dir {
            row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let this = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(this) = this.upgrade() {
                    this.open_folder(path.clone());
                }
            });
        } else {
            row.add_prefix(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            let mut subtitle = size_text(entry.size);
            if let Some(when) = modified_text(entry.modified) {
                subtitle = format!("{subtitle} · {when}");
            }
            row.set_subtitle(&subtitle);
            let download = gtk::Button::builder()
                .icon_name("folder-download-symbolic")
                .tooltip_text("Скачать в «Загрузки»")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&download);
            row.set_activatable_widget(Some(&download));
            let this = Rc::downgrade(self);
            let name = entry.name.clone();
            let size = entry.size;
            download.connect_clicked(move |button| {
                if let Some(this) = this.upgrade() {
                    this.download(button, path.clone(), name.clone(), size);
                }
            });
        }
        row
    }

    fn download(&self, button: &gtk::Button, path: String, name: String, size: u64) {
        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .width_request(80)
            .build();
        button.set_child(Some(&progress));
        button.set_sensitive(false);
        // Progress crosses from the network thread through a channel, a few times a second.
        let (tx, rx) = async_channel::bounded::<f64>(4);
        glib::spawn_future_local(glib::clone!(
            #[weak]
            progress,
            async move {
                while let Ok(fraction) = rx.recv().await {
                    progress.set_fraction(fraction);
                }
            }
        ));
        let files = self.files.clone();
        let window = self.window.clone();
        let button = button.clone();
        glib::spawn_future_local(async move {
            let dir = downloads_dir();
            let result = run(async move {
                let mut last = std::time::Instant::now();
                files
                    .fetch(&path, &dir, move |got, total| {
                        if last.elapsed().as_millis() >= 100 && total > 0 {
                            last = std::time::Instant::now();
                            let _ = tx.try_send(got as f64 / total as f64);
                        }
                    })
                    .await
            })
            .await;
            button.set_child(None::<&gtk::Widget>);
            button.set_icon_name("folder-download-symbolic");
            button.set_sensitive(true);
            match result {
                Ok(saved) => {
                    let saved = saved
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(name);
                    window.toast(&format!(
                        "Скачано «{saved}» ({}) в «Загрузки»",
                        size_text(size)
                    ));
                }
                Err(e) => window.toast(&format!("Не удалось скачать «{name}»: {}", describe(&e))),
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
    }
}
