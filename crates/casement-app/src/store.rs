//! Saved computers in `connections.ini` (GLib key file, one group per computer). The file
//! format is the one the earlier Vala application wrote, so existing lists keep working.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use casement_net::DEFAULT_PORT;

#[derive(Clone, Debug, PartialEq)]
pub struct Computer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// 64 hex digits once the first connection was confirmed, else empty.
    pub fingerprint: String,
    /// Ask for a virtual monitor the size of the window instead of the host's screen.
    pub virtual_monitor: bool,
}

impl Computer {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            id: glib::uuid_string_random().to_string(),
            name: String::new(),
            host: host.to_string(),
            port,
            fingerprint: String::new(),
            virtual_monitor: false,
        }
    }

    pub fn title(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            self.address()
        } else {
            name.to_string()
        }
    }

    pub fn address(&self) -> String {
        if self.port == DEFAULT_PORT {
            self.host.clone()
        } else if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Accepts "host", "host:port", "[ipv6]" and "[ipv6]:port".
pub fn parse_address(text: &str) -> Option<(String, u16)> {
    let value = text.trim();
    if value.is_empty() || value.contains([' ', '/', '@']) {
        return None;
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':')?)),
        }
    } else if value.matches(':').count() == 1 {
        let (host, port) = value.split_once(':')?;
        (host, Some(port))
    } else {
        (value, None)
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) => p.parse::<u16>().ok().filter(|&p| p != 0)?,
        None => DEFAULT_PORT,
    };
    Some((host.to_string(), port))
}

fn is_fingerprint(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Default)]
struct Inner {
    path: PathBuf,
    items: Vec<Computer>,
    listeners: Vec<Box<dyn Fn()>>,
}

/// Shared on the main thread; clones refer to the same list.
#[derive(Clone, Default)]
pub struct Store(Rc<RefCell<Inner>>);

impl Store {
    pub fn open(dir: &Path) -> Self {
        let store = Self(Rc::new(RefCell::new(Inner {
            path: dir.join("connections.ini"),
            ..Default::default()
        })));
        store.load();
        store
    }

    pub fn path(&self) -> PathBuf {
        self.0.borrow().path.clone()
    }

    /// Unreadable groups are skipped, never fatal: one broken entry must not hide the rest.
    pub fn load(&self) {
        let file = glib::KeyFile::new();
        let path = self.path();
        let mut items = Vec::new();
        match file.load_from_file(&path, glib::KeyFileFlags::NONE) {
            Ok(()) => {
                for group in file.groups() {
                    let group = group.as_str();
                    let get = |key: &str| {
                        file.string(group, key)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default()
                    };
                    let Some((host, _)) = parse_address(&get("host")) else {
                        continue;
                    };
                    let port = file
                        .uint64(group, "port")
                        .ok()
                        .and_then(|p| u16::try_from(p).ok())
                        .filter(|&p| p != 0)
                        .unwrap_or(DEFAULT_PORT);
                    let fingerprint = get("fingerprint").to_lowercase();
                    items.push(Computer {
                        id: group.to_string(),
                        name: get("name"),
                        host,
                        port,
                        fingerprint: if is_fingerprint(&fingerprint) {
                            fingerprint
                        } else {
                            String::new()
                        },
                        virtual_monitor: file.boolean(group, "virtual-monitor").unwrap_or(false),
                    });
                }
            }
            Err(e) if e.matches(glib::FileError::Noent) => {}
            Err(e) => glib::g_warning!("Casement", "Cannot read {}: {}", path.display(), e),
        }
        items.sort_by_key(|c| c.title().to_lowercase());
        self.0.borrow_mut().items = items;
        self.notify();
    }

    fn save(&self) -> Result<(), String> {
        let file = glib::KeyFile::new();
        for item in &self.0.borrow().items {
            file.set_string(&item.id, "name", &item.name);
            file.set_string(&item.id, "host", &item.host);
            file.set_uint64(&item.id, "port", item.port as u64);
            if !item.fingerprint.is_empty() {
                file.set_string(&item.id, "fingerprint", &item.fingerprint);
            }
            if item.virtual_monitor {
                file.set_boolean(&item.id, "virtual-monitor", true);
            }
        }
        let path = self.path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        glib::file_set_contents_full(
            &path,
            file.to_data().as_bytes(),
            glib::FileSetContentsFlags::CONSISTENT,
            0o600,
        )
        .map_err(|e| e.to_string())
    }

    pub fn items(&self) -> Vec<Computer> {
        self.0.borrow().items.clone()
    }

    pub fn find(&self, id: &str) -> Option<Computer> {
        self.0.borrow().items.iter().find(|c| c.id == id).cloned()
    }

    pub fn find_by_fingerprint(&self, fingerprint: &str) -> Option<Computer> {
        if fingerprint.is_empty() {
            return None;
        }
        self.0
            .borrow()
            .items
            .iter()
            .find(|c| c.fingerprint == fingerprint)
            .cloned()
    }

    pub fn find_by_address(&self, host: &str, port: u16) -> Option<Computer> {
        self.0
            .borrow()
            .items
            .iter()
            .find(|c| c.host.eq_ignore_ascii_case(host) && c.port == port)
            .cloned()
    }

    pub fn put(&self, item: Computer) -> Result<(), String> {
        {
            let mut inner = self.0.borrow_mut();
            match inner.items.iter_mut().find(|c| c.id == item.id) {
                Some(existing) => *existing = item,
                None => inner.items.push(item),
            }
            inner.items.sort_by_key(|c| c.title().to_lowercase());
        }
        self.save()?;
        self.notify();
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.0.borrow_mut().items.retain(|c| c.id != id);
        self.save()?;
        self.notify();
        Ok(())
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.0.borrow_mut().listeners.push(Box::new(f));
    }

    fn notify(&self) {
        let listeners = std::mem::take(&mut self.0.borrow_mut().listeners);
        for listener in &listeners {
            listener();
        }
        let mut inner = self.0.borrow_mut();
        let added = std::mem::take(&mut inner.listeners);
        inner.listeners = listeners;
        inner.listeners.extend(added);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path());
        assert!(store.items().is_empty());
        let mut item = Computer::new("10.0.0.5", 47900);
        item.name = "Офис".into();
        item.fingerprint = "a".repeat(64);
        item.virtual_monitor = true;
        store.put(item.clone()).unwrap();
        store.put(Computer::new("gem12", DEFAULT_PORT)).unwrap();

        let again = Store::open(dir.path());
        assert_eq!(again.items().len(), 2);
        assert_eq!(again.find(&item.id).unwrap(), item);
        assert_eq!(
            again
                .find_by_address("GEM12", DEFAULT_PORT)
                .unwrap()
                .address(),
            "gem12"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(again.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        again.remove(&item.id).unwrap();
        assert_eq!(Store::open(dir.path()).items().len(), 1);
    }

    #[test]
    fn damaged_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.ini"),
            "[good]\nhost=pc.local\nport=99999\nfingerprint=nope\n\n[no-host]\nname=x\n\n[bad]\nhost=has space\n",
        )
        .unwrap();
        let items = Store::open(dir.path()).items();
        assert_eq!(items.len(), 1);
        assert_eq!(
            (
                items[0].id.as_str(),
                items[0].port,
                items[0].fingerprint.as_str()
            ),
            ("good", DEFAULT_PORT, "")
        );
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("pc"), Some(("pc".into(), DEFAULT_PORT)));
        assert_eq!(
            parse_address(" 10.0.0.1:3390 "),
            Some(("10.0.0.1".into(), 3390))
        );
        assert_eq!(
            parse_address("[fe80::1]:4000"),
            Some(("fe80::1".into(), 4000))
        );
        assert_eq!(
            parse_address("fe80::1"),
            Some(("fe80::1".into(), DEFAULT_PORT))
        );
        for bad in [
            "", "pc:0", "pc:70000", "pc:abc", ":3389", "[]:1", "user@pc", "rdp://pc",
        ] {
            assert_eq!(parse_address(bad), None, "{bad}");
        }
    }
}
