//! LAN discovery is scoped to its page, never a background task on Home.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::core::runtime;
use crate::pages;
use crate::store::Computer;
use crate::window::Window;
use adw::prelude::*;
use casement_net::discovery::{Browser, DiscoveryEvent, Peer};

enum Event {
    Peer(DiscoveryEvent),
    Settled,
    Failed(String),
}

struct Scan {
    window: Window,
    list: gtk::ListBox,
    stack: gtk::Stack,
    status: adw::StatusPage,
    peers: RefCell<BTreeMap<String, Peer>>,
    stop: RefCell<Option<tokio::sync::oneshot::Sender<()>>>,
    generation: Cell<u64>,
}

pub fn open(window: &Window) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["device-list"])
        .build();
    let status = adw::StatusPage::builder()
        .icon_name("network-wireless-symbolic")
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&status, Some("status"));
    stack.add_named(&pages::scrolled(&list, 840), Some("list"));
    let refresh = pages::flat_icon("view-refresh-symbolic", "Обновить");
    let nav = pages::frame(
        "В локальной сети",
        "discovery",
        &stack,
        Some(refresh.upcast_ref()),
    );
    let scan = Rc::new(Scan {
        window: window.clone(),
        list,
        stack,
        status,
        peers: RefCell::new(BTreeMap::new()),
        stop: RefCell::new(None),
        generation: Cell::new(0),
    });
    let weak = Rc::downgrade(&scan);
    refresh.connect_clicked(move |_| {
        if let Some(scan) = weak.upgrade() {
            scan.start();
        }
    });
    let handler = Rc::new(RefCell::new(None));
    let (weak, handler2, page) = (Rc::downgrade(&scan), handler.clone(), nav.clone());
    let id = window.navigation().connect_popped(move |view, popped| {
        if popped == &page {
            if let Some(scan) = weak.upgrade() {
                scan.cancel();
            }
            if let Some(id) = handler2.take() {
                view.disconnect(id);
            }
        }
    });
    handler.replace(Some(id));
    scan.start();
    window.navigation().push(&nav);
}

impl Scan {
    fn cancel(&self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.generation.set(self.generation.get() + 1);
    }

    fn start(self: &Rc<Self>) {
        self.cancel();
        self.peers.borrow_mut().clear();
        self.status.set_title("Обнаружение устройств…");
        self.status.set_description(None);
        self.status.set_child(Some(
            &adw::Spinner::builder()
                .width_request(32)
                .height_request(32)
                .halign(gtk::Align::Center)
                .build(),
        ));
        self.stack.set_visible_child_name("status");
        let generation = self.generation.get();
        let (stop, mut cancel) = tokio::sync::oneshot::channel();
        self.stop.replace(Some(stop));
        let (tx, rx) = async_channel::bounded(32);
        runtime().spawn(async move {
            let browser = match Browser::start() {
                Ok(browser) => browser,
                Err(e) => { let _ = tx.send(Event::Failed(e.to_string())).await; return; }
            };
            let settle = tokio::time::sleep(std::time::Duration::from_secs(5));
            tokio::pin!(settle);
            let mut settled = false;
            loop {
                let event = tokio::select! {
                    _ = &mut cancel => break,
                    _ = &mut settle, if !settled => { settled = true; Event::Settled },
                    event = browser.next() => match event { Some(event) => Event::Peer(event), None => break },
                };
                if tx.send(event).await.is_err() { break; }
            }
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                if this.generation.get() != generation {
                    break;
                }
                match event {
                    Event::Peer(DiscoveryEvent::Found(peer)) => {
                        let own = this.window.core().identity.fingerprint();
                        let machine = crate::service::machine_access().map(|m| m.fingerprint);
                        if peer.fingerprint == own
                            || machine.as_deref() == Some(peer.fingerprint.display().as_str())
                        {
                            continue;
                        }
                        this.peers
                            .borrow_mut()
                            .insert(peer.fingerprint.to_hex(), peer);
                        this.fill();
                    }
                    Event::Peer(DiscoveryEvent::Lost { id }) => {
                        this.peers.borrow_mut().retain(|_, p| p.id != id);
                        this.fill();
                    }
                    Event::Settled if this.peers.borrow().is_empty() => {
                        this.status.set_title("Устройства не найдены");
                        this.status.set_child(None::<&gtk::Widget>);
                    }
                    Event::Failed(error) => {
                        this.status.set_title("Локальная сеть недоступна");
                        this.status
                            .set_description(Some(glib::markup_escape_text(&error).as_str()));
                        this.status.set_child(None::<&gtk::Widget>);
                    }
                    Event::Settled => {}
                }
            }
        });
    }

    fn fill(&self) {
        self.list.remove_all();
        let mut peers: Vec<_> = self.peers.borrow().values().cloned().collect();
        peers.sort_by_key(|p| p.name.to_lowercase());
        for peer in peers {
            let Some(address) = peer.addresses.first() else {
                continue;
            };
            let mut computer = Computer::new(&address.to_string(), peer.port);
            computer.name = peer.name;
            computer.os_display_name = peer.os_display_name;
            for address in &peer.addresses {
                computer
                    .remember_route(&std::net::SocketAddr::new(*address, peer.port).to_string());
            }
            let store = self.window.store();
            let existing = store
                .find_by_fingerprint(&peer.fingerprint.to_hex())
                // A manual entry has no key yet, so match it by address once. Entries
                // with learned, different keys may share an address in a dual-boot PC.
                .or_else(|| {
                    store
                        .find_by_address(&computer.host, computer.port)
                        .filter(|saved| saved.fingerprint.is_empty())
                });
            let address = computer.address();
            if let Some(mut saved) = existing.clone() {
                let before = saved.clone();
                saved.remember_route(&address);
                for route in &computer.routes {
                    saved.remember_route(route);
                }
                if saved != before {
                    if let Err(e) = store.put(saved) {
                        self.window.toast(&format!("Не удалось сохранить: {e}"));
                    }
                }
            }
            let (title, subtitle) =
                pages::connection_labels(&computer.name, &address, &computer.os_display_name);
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&title).as_str())
                .subtitle(glib::markup_escape_text(&subtitle).as_str())
                .title_lines(1)
                .subtitle_lines(1)
                .activatable(existing.is_none())
                .css_classes(["device-card"])
                .build();
            row.add_prefix(&pages::os_icon_tile(&computer.os_display_name));
            if existing.is_some() {
                row.add_suffix(
                    &gtk::Label::builder()
                        .label("Добавлен")
                        .css_classes(["connection-state"])
                        .build(),
                );
            } else {
                let add = gtk::Image::from_icon_name("list-add-symbolic");
                add.add_css_class("connection-add");
                row.add_suffix(&add);
                let window = self.window.clone();
                row.connect_activated(move |_| {
                    // A discovery announcement never establishes trust in a key.
                    match window.store().put(computer.clone()) {
                        Ok(()) => {
                            window.navigation().pop_to_tag("home");
                        }
                        Err(e) => window.toast(&format!("Не удалось добавить: {e}")),
                    }
                });
            }
            self.list.append(&row);
        }
        let empty = self.list.first_child().is_none();
        if empty {
            self.status.set_title("Устройства не найдены");
            self.status.set_child(None::<&gtk::Widget>);
        }
        self.stack
            .set_visible_child_name(if empty { "status" } else { "list" });
    }
}
