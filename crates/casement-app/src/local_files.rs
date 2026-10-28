//! A local, multi-file picker implemented as a navigation page.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;

use crate::core::run;
use crate::pages;
use crate::window::Window;

const MAX_ENTRIES: usize = 4000;

struct Entry {
    path: PathBuf,
    name: String,
    directory: bool,
    hidden: bool,
    size: u64,
}

struct Listing {
    entries: Vec<Entry>,
    truncated: bool,
}

fn list_folder(path: &Path) -> std::io::Result<Listing> {
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_file() && !metadata.is_dir() {
            continue;
        }
        if entries.len() == MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        #[cfg(windows)]
        let hidden = {
            use std::os::windows::fs::MetadataExt;
            name.starts_with('.') || metadata.file_attributes() & 2 != 0
        };
        #[cfg(not(windows))]
        let hidden = name.starts_with('.');
        entries.push(Entry {
            path: entry.path(),
            name,
            directory: metadata.is_dir(),
            hidden,
            size: metadata.len(),
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Listing { entries, truncated })
}

fn path_from_text(text: &str, current: &Path, home: &Path) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    if text == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    let path = PathBuf::from(text);
    Some(if path.is_absolute() {
        path
    } else {
        current.join(path)
    })
}

type SelectionCallback = Box<dyn Fn(Vec<PathBuf>)>;

struct Browser {
    window: Window,
    page: adw::NavigationPage,
    path: gtk::Entry,
    up: gtk::Button,
    stack: gtk::Stack,
    list: gtk::ListBox,
    status: adw::StatusPage,
    retry: gtk::Button,
    show_hidden: gtk::ToggleButton,
    send: gtk::Button,
    count: gtk::Label,
    folder: RefCell<PathBuf>,
    listing: RefCell<Option<Listing>>,
    selected: RefCell<BTreeSet<PathBuf>>,
    on_selected: RefCell<Option<SelectionCallback>>,
    generation: Cell<u64>,
    gone: Cell<bool>,
}

/// Select files, then return once to the underlying session with their paths.
pub fn open(window: &Window, on_selected: impl Fn(Vec<PathBuf>) + 'static) {
    let header = adw::HeaderBar::new();
    let home = pages::flat_icon("user-home-symbolic", "Домашняя папка");
    let downloads = pages::flat_icon("folder-download-symbolic", "Загрузки");
    header.pack_start(&home);
    header.pack_start(&downloads);
    let show_hidden = gtk::ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text("Скрытые файлы")
        .build();
    header.pack_end(&show_hidden);
    let up = pages::flat_icon("go-up-symbolic", "На уровень выше");
    let refresh = pages::flat_icon("view-refresh-symbolic", "Обновить");
    let path = gtk::Entry::builder()
        .hexpand(true)
        .width_chars(6)
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    let location = gtk::Box::builder()
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    location.append(&up);
    location.append(&path);
    location.append(&refresh);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .valign(gtk::Align::Start)
        .build();
    let clamp = adw::Clamp::builder()
        .maximum_size(840)
        .tightening_threshold(560)
        .child(&list)
        .margin_start(18)
        .margin_end(18)
        .margin_top(12)
        .margin_bottom(24)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
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
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&location);
    content.append(&stack);
    let count = gtk::Label::builder()
        .label("Файлы не выбраны")
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label"])
        .build();
    let send = gtk::Button::builder()
        .label("Отправить")
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    let bottom = gtk::Box::builder()
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    bottom.append(&count);
    bottom.append(&send);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_bottom_bar(&bottom);
    view.set_content(Some(&content));
    let page = adw::NavigationPage::builder()
        .title("Выбор файлов")
        .tag("local-files")
        .child(&view)
        .build();
    let initial = glib::user_special_dir(glib::UserDirectory::Downloads)
        .unwrap_or_else(|| glib::home_dir().join("Downloads"));
    let browser = Rc::new(Browser {
        window: window.clone(),
        page: page.clone(),
        path: path.clone(),
        up: up.clone(),
        stack,
        list,
        status,
        retry: retry.clone(),
        show_hidden: show_hidden.clone(),
        send: send.clone(),
        count,
        folder: RefCell::new(initial.clone()),
        listing: RefCell::new(None),
        selected: RefCell::new(BTreeSet::new()),
        on_selected: RefCell::new(Some(Box::new(on_selected))),
        generation: Cell::new(0),
        gone: Cell::new(false),
    });
    let weak = Rc::downgrade(&browser);
    home.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            browser.open_folder(glib::home_dir());
        }
    });
    let weak = Rc::downgrade(&browser);
    downloads.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            let folder = glib::user_special_dir(glib::UserDirectory::Downloads)
                .unwrap_or_else(|| glib::home_dir().join("Downloads"));
            browser.open_folder(folder);
        }
    });
    let weak = Rc::downgrade(&browser);
    up.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            let folder = browser.folder.borrow().parent().map(Path::to_path_buf);
            if let Some(folder) = folder {
                browser.open_folder(folder);
            }
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
            let folder = path_from_text(&entry.text(), &browser.folder.borrow(), &glib::home_dir());
            if let Some(folder) = folder {
                browser.open_folder(folder);
            } else {
                entry.add_css_class("error");
            }
        }
    });
    path.connect_changed(|entry| entry.remove_css_class("error"));
    let weak = Rc::downgrade(&browser);
    show_hidden.connect_toggled(move |_| {
        if let Some(browser) = weak.upgrade() {
            browser.fill();
        }
    });
    let weak = Rc::downgrade(&browser);
    send.connect_clicked(move |_| {
        if let Some(browser) = weak.upgrade() {
            browser.confirm();
        }
    });
    let keep = RefCell::new(Some(browser.clone()));
    let target = page.clone();
    let handler = Rc::new(RefCell::new(None));
    let handler2 = handler.clone();
    let id = window
        .navigation()
        .connect_popped(move |navigation, popped| {
            if popped == &target {
                if let Some(browser) = keep.take() {
                    browser.gone.set(true);
                    browser.generation.set(browser.generation.get() + 1);
                    browser.on_selected.take();
                }
                if let Some(id) = handler2.take() {
                    navigation.disconnect(id);
                }
            }
        });
    handler.replace(Some(id));
    window.navigation().push(&page);
    browser.open_folder(initial);
}

impl Browser {
    fn open_folder(self: &Rc<Self>, folder: PathBuf) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.folder.replace(folder.clone());
        self.listing.replace(None);
        self.path.set_text(&folder.to_string_lossy());
        self.up.set_sensitive(folder.parent().is_some());
        self.stack.set_visible_child_name("loading");
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = run(async move {
                tokio::task::spawn_blocking(move || list_folder(&folder))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())
            })
            .await;
            if this.gone.get() || this.generation.get() != generation {
                return;
            }
            match result {
                Ok(listing) => {
                    this.listing.replace(Some(listing));
                    this.fill();
                }
                Err(error) => {
                    this.listing.replace(None);
                    this.show_status("dialog-error-symbolic", "Папка недоступна", &error, true);
                }
            }
        });
    }

    fn show_status(&self, icon: &str, title: &str, message: &str, retry: bool) {
        self.status.set_icon_name(Some(icon));
        self.status.set_title(title);
        self.status
            .set_description(Some(glib::markup_escape_text(message).as_str()));
        self.retry.set_visible(retry);
        self.stack.set_visible_child_name("status");
    }

    fn fill(self: &Rc<Self>) {
        let listing = self.listing.borrow();
        let Some(listing) = listing.as_ref() else {
            return;
        };
        self.list.remove_all();
        let mut count = 0;
        for entry in &listing.entries {
            if entry.hidden && !self.show_hidden.is_active() {
                continue;
            }
            self.list.append(&self.row(entry));
            count += 1;
        }
        if listing.truncated {
            self.list.append(
                &adw::ActionRow::builder()
                    .title("Показаны не все файлы")
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        if count == 0 && !listing.truncated {
            self.show_status("folder-symbolic", "Папка пуста", "", false);
        } else {
            self.stack.set_visible_child_name("list");
        }
    }

    fn row(self: &Rc<Self>, entry: &Entry) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&entry.name).as_str())
            .title_lines(1)
            .subtitle_lines(1)
            .activatable(true)
            .css_classes(["file-row"])
            .build();
        row.add_prefix(&pages::icon_tile(if entry.directory {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        }));
        let path = entry.path.clone();
        if entry.directory {
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.open_folder(path.clone());
                }
            });
        } else {
            row.set_subtitle(&glib::format_size(entry.size));
            let selected = gtk::CheckButton::builder()
                .active(self.selected.borrow().contains(&path))
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&selected);
            row.set_activatable_widget(Some(&selected));
            let weak = Rc::downgrade(self);
            selected.connect_toggled(move |button| {
                if let Some(browser) = weak.upgrade() {
                    if button.is_active() {
                        browser.selected.borrow_mut().insert(path.clone());
                    } else {
                        browser.selected.borrow_mut().remove(&path);
                    }
                    browser.update_selection();
                }
            });
        }
        row
    }

    fn update_selection(&self) {
        let count = self.selected.borrow().len();
        self.count.set_label(&if count == 0 {
            "Файлы не выбраны".to_string()
        } else {
            format!("Выбрано: {count}")
        });
        self.send
            .set_sensitive(count > 0 && self.on_selected.borrow().is_some());
    }

    fn confirm(&self) {
        if self.selected.borrow().is_empty()
            || self.gone.get()
            || self.window.navigation().visible_page().as_ref() != Some(&self.page)
        {
            return;
        }
        let Some(callback) = self.on_selected.take() else {
            return;
        };
        self.send.set_sensitive(false);
        let selected = self.selected.borrow().iter().cloned().collect();
        self.window.navigation().pop();
        callback(selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_support_home_absolute_and_relative_locations() {
        let root = std::env::current_dir().unwrap();
        let current = root.join("Documents");
        assert_eq!(path_from_text("~", &current, &root), Some(root.clone()));
        assert_eq!(
            path_from_text("~/Downloads", &current, &root),
            Some(root.join("Downloads"))
        );
        assert_eq!(
            path_from_text("nested", &current, &root),
            Some(current.join("nested"))
        );
        assert_eq!(
            path_from_text(&root.to_string_lossy(), &current, &root),
            Some(root.clone())
        );
        assert_eq!(path_from_text("", &current, &root), None);
        assert_eq!(path_from_text("bad\0path", &current, &root), None);
    }
}
