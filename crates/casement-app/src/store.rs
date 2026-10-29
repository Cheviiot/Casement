//! Saved computers in `connections.ini` (GLib key file, one group per computer). The file
//! format is the one the earlier Vala application wrote, so existing lists keep working.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use casement_net::DEFAULT_PORT;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoQuality {
    Economy,
    #[default]
    Balanced,
    High,
    Ultra,
}

impl VideoQuality {
    pub const LABELS: [&'static str; 4] =
        ["720p · Авто", "1080p · Авто", "1440p · Авто", "4K · Авто"];

    pub fn from_index(index: u32) -> Self {
        match index {
            0 => Self::Economy,
            2 => Self::High,
            3 => Self::Ultra,
            _ => Self::Balanced,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            Self::Economy => 0,
            Self::Balanced => 1,
            Self::High => 2,
            Self::Ultra => 3,
        }
    }

    pub fn from_key(value: &str) -> Self {
        match value {
            "economy" => Self::Economy,
            "high" => Self::High,
            "ultra" => Self::Ultra,
            _ => Self::Balanced,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::High => "high",
            Self::Ultra => "ultra",
        }
    }

    pub fn bitrate_kbps(self) -> u32 {
        match self {
            Self::Economy => 8_000,
            Self::Balanced => 20_000,
            Self::High => 40_000,
            Self::Ultra => 80_000,
        }
    }

    /// New peers grow from the profile's safe starting point to what the path can sustain.
    pub fn max_bitrate_kbps(self) -> u32 {
        casement_net::screen::MAX_VIDEO_BITRATE_KBPS
    }

    pub fn virtual_size(self) -> (u32, u32) {
        match self {
            Self::Economy => (1280, 720),
            Self::Balanced => (1920, 1080),
            Self::High => (2560, 1440),
            Self::Ultra => (3840, 2160),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Computer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// 64 hex digits once the first connection was confirmed, else empty.
    pub fingerprint: String,
    /// Human-readable OS reported by the remote computer; absent in old records.
    pub os_display_name: String,
    /// Ask for a stable virtual desktop instead of mirroring the host's screen.
    pub virtual_monitor: bool,
    /// Resolution and bitrate selected for this connection.
    pub video_quality: VideoQuality,
    /// Most recent successful connection, as Unix seconds; zero means never.
    pub last_connected: i64,
}

impl Computer {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            id: glib::uuid_string_random().to_string(),
            name: String::new(),
            host: host.to_string(),
            port,
            fingerprint: String::new(),
            os_display_name: String::new(),
            virtual_monitor: false,
            video_quality: VideoQuality::default(),
            last_connected: 0,
        }
    }

    pub fn title(&self) -> String {
        let name = self.name.trim();
        let name = if name.is_empty() {
            self.address()
        } else {
            name.to_string()
        };
        if self.os_display_name.trim().is_empty() {
            name
        } else {
            format!("{name} · {}", self.os_display_name.trim())
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

    /// Record a successful connection using whole Unix seconds.
    pub fn mark_connected(&mut self) {
        self.last_connected = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64;
    }
}

/// Accepts "host", "host:port", "[ipv6]" and "[ipv6]:port".
pub fn parse_address(text: &str) -> Option<(String, u16)> {
    if text.chars().any(char::is_control) {
        return None;
    }
    let value = text.trim();
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.contains(['/', '@', '?', '#'])
        || value.contains("://")
    {
        return None;
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        if !valid_ipv6(host) {
            return None;
        }
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':')?)),
        }
    } else if value.matches(':').count() == 1 {
        let (host, port) = value.split_once(':')?;
        (host, Some(port))
    } else {
        if value.contains(':') && !valid_ipv6(value) {
            return None;
        }
        (value, None)
    };
    if host.is_empty() || (!host.contains(':') && !valid_hostname(host)) {
        return None;
    }
    let port = match port {
        Some(p) => p.parse::<u16>().ok().filter(|&p| p != 0)?,
        None => DEFAULT_PORT,
    };
    Some((host.to_string(), port))
}

fn valid_hostname(host: &str) -> bool {
    host.strip_suffix('.')
        .unwrap_or(host)
        .split('.')
        .all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        })
}

fn valid_ipv6(host: &str) -> bool {
    let address = if let Some((address, zone)) = host.split_once('%') {
        if zone.is_empty()
            || !zone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return false;
        }
        address
    } else {
        host
    };
    address.parse::<std::net::Ipv6Addr>().is_ok()
}

fn sort_computers(items: &mut [Computer]) {
    items.sort_by(|a, b| {
        a.title()
            .to_lowercase()
            .cmp(&b.title().to_lowercase())
            .then_with(|| a.address().to_lowercase().cmp(&b.address().to_lowercase()))
    });
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
                        os_display_name: get("os-name"),
                        virtual_monitor: file.boolean(group, "virtual-monitor").unwrap_or(false),
                        video_quality: VideoQuality::from_key(&get("video-quality")),
                        last_connected: file
                            .int64(group, "last-connected")
                            .ok()
                            .filter(|&seconds| seconds > 0)
                            .unwrap_or(0),
                    });
                }
            }
            Err(e) if e.matches(glib::FileError::Noent) => {}
            Err(e) => glib::g_warning!("Casement", "Cannot read {}: {}", path.display(), e),
        }
        sort_computers(&mut items);
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
            if !item.os_display_name.is_empty() {
                file.set_string(&item.id, "os-name", &item.os_display_name);
            }
            if item.virtual_monitor {
                file.set_boolean(&item.id, "virtual-monitor", true);
            }
            if item.video_quality != VideoQuality::default() {
                file.set_string(&item.id, "video-quality", item.video_quality.key());
            }
            file.set_int64(&item.id, "last-connected", item.last_connected.max(0));
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
        let previous = self.0.borrow().items.clone();
        {
            let mut inner = self.0.borrow_mut();
            match inner.items.iter_mut().find(|c| c.id == item.id) {
                Some(existing) => *existing = item,
                None => inner.items.push(item),
            }
            sort_computers(&mut inner.items);
        }
        if let Err(error) = self.save() {
            self.0.borrow_mut().items = previous;
            return Err(error);
        }
        self.notify();
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let previous = self.0.borrow().items.clone();
        self.0.borrow_mut().items.retain(|c| c.id != id);
        if let Err(error) = self.save() {
            self.0.borrow_mut().items = previous;
            return Err(error);
        }
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
        item.os_display_name = "ALT Linux".into();
        item.fingerprint = "a".repeat(64);
        item.virtual_monitor = true;
        item.video_quality = VideoQuality::Ultra;
        item.last_connected = 1_800_000_000;
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
        assert_eq!(items[0].last_connected, 0);
    }

    #[test]
    fn old_entries_default_last_connected_and_new_value_persists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.ini"),
            "[legacy]\nhost=legacy.local\nport=47820\n\n[modern]\nhost=modern.local\nport=47820\nlast-connected=1234\n",
        )
        .unwrap();

        let store = Store::open(dir.path());
        let legacy = store.find("legacy").unwrap();
        assert_eq!(legacy.last_connected, 0);
        let modern = store.find("modern").unwrap();
        assert_eq!(modern.last_connected, 1234);

        store.put(legacy).unwrap();
        let reloaded = Store::open(dir.path());
        assert_eq!(reloaded.find("legacy").unwrap().last_connected, 0);
        assert_eq!(reloaded.find("modern").unwrap().last_connected, 1234);
        assert_eq!(reloaded.find("legacy").unwrap().os_display_name, "");
    }

    #[test]
    fn same_address_can_hold_two_distinct_os_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path());
        let mut linux = Computer::new("gem12", DEFAULT_PORT);
        linux.name = "GEM12".into();
        linux.os_display_name = "ALT Linux".into();
        linux.fingerprint = "a".repeat(64);
        let mut windows = linux.clone();
        windows.id = "windows".into();
        windows.os_display_name = "Windows".into();
        windows.fingerprint = "b".repeat(64);
        store.put(linux.clone()).unwrap();
        store.put(windows.clone()).unwrap();

        let reloaded = Store::open(dir.path());
        assert_eq!(reloaded.items().len(), 2);
        assert_eq!(
            reloaded.find(&linux.id).unwrap().title(),
            "GEM12 · ALT Linux"
        );
        assert_eq!(
            reloaded.find(&windows.id).unwrap().title(),
            "GEM12 · Windows"
        );
    }

    #[test]
    fn sort_is_stable_by_unicode_title_and_address_not_connection_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path());
        let mut alpha = Computer::new("z.example", DEFAULT_PORT);
        alpha.id = "alpha".into();
        alpha.name = "Альфа".into();
        let mut title_tie_b = Computer::new("b.example", DEFAULT_PORT);
        title_tie_b.id = "title-tie-b".into();
        title_tie_b.name = "Яблоко".into();
        title_tie_b.last_connected = 100;
        let mut title_tie_a = title_tie_b.clone();
        title_tie_a.id = "title-tie-a".into();
        title_tie_a.host = "a.example".into();
        title_tie_a.last_connected = 0;
        let mut title_tie_same_address = title_tie_b.clone();
        title_tie_same_address.id = "same-address".into();
        title_tie_same_address.name = "яблоко".into();
        title_tie_same_address.last_connected = 1;
        for item in [title_tie_b, alpha, title_tie_same_address, title_tie_a] {
            store.put(item).unwrap();
        }

        let before = store.items();
        assert_eq!(
            before
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "title-tie-a", "title-tie-b", "same-address"]
        );
        let mut changed_recency = store.find("title-tie-b").unwrap();
        changed_recency.last_connected = 1_800_000_000;
        store.put(changed_recency).unwrap();
        assert_eq!(
            store
                .items()
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            before
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn failed_put_and_remove_restore_items_without_notifying() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path());
        let existing = Computer::new("existing", DEFAULT_PORT);
        store.put(existing.clone()).unwrap();
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = notifications.clone();
        store.connect_changed(move || observed.set(observed.get() + 1));

        std::fs::remove_file(store.path()).unwrap();
        std::fs::create_dir(store.path()).unwrap();

        let mut changed = existing.clone();
        changed.name = "changed".into();
        assert!(store.put(changed).is_err());
        assert_eq!(store.items().as_slice(), std::slice::from_ref(&existing));
        assert_eq!(notifications.get(), 0);

        assert!(store.remove(&existing.id).is_err());
        assert_eq!(store.items(), [existing]);
        assert_eq!(notifications.get(), 0);
    }

    #[test]
    fn mark_connected_sets_positive_unix_seconds() {
        let mut computer = Computer::new("pc", DEFAULT_PORT);
        computer.mark_connected();
        assert!(computer.last_connected > 0);
        assert!(
            computer.last_connected
                <= std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64
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
        assert_eq!(parse_address("[::1]"), Some(("::1".into(), DEFAULT_PORT)));
        assert_eq!(
            parse_address("server.local."),
            Some(("server.local.".into(), DEFAULT_PORT))
        );
        assert_eq!(
            parse_address("legacy_pc"),
            Some(("legacy_pc".into(), DEFAULT_PORT))
        );
        assert_eq!(
            parse_address("[fe80::1%enp1s0]:4000"),
            Some(("fe80::1%enp1s0".into(), 4000))
        );
        assert_eq!(
            parse_address("casement-device.tailnet-abc.ts.net"),
            Some(("casement-device.tailnet-abc.ts.net".into(), DEFAULT_PORT))
        );
        for bad in [
            "",
            "pc:0",
            "pc:70000",
            "pc:abc",
            ":3389",
            "[]:1",
            "user@pc",
            "rdp://pc",
            "pc\n",
            "pc\u{0000}",
            "pc/path",
            "pc?x",
            "pc#x",
            "pc..local",
            "-pc",
            "pc-",
            "[fe80::1%]:4000",
            "pc:80:90",
            "[not-ipv6]:80",
            "[::1]junk",
            "2001:db8::zz",
        ] {
            assert_eq!(parse_address(bad), None, "{bad}");
        }
    }
}
