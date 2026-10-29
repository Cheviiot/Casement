//! Deterministic GTK regression checks run by the headless Broadway snapshot path.
//!
//! The caller must provide a disposable `CASEMENT_CONFIG_DIR` fixture and set
//! `CASEMENT_SNAPSHOT`; this module never synthesizes operating-system input.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use adw::prelude::*;

use crate::store::Store;
use crate::window::Window;

mod latency;
mod live;
mod marker;

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const STATE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(window: &Window) -> Result<Vec<String>, String> {
    require_fixture(window)?;
    if let Some(root) = std::env::var_os("CASEMENT_UI_FIXTURE_ROOT").filter(|v| !v.is_empty()) {
        return live::run(window, &PathBuf::from(root)).await;
    }
    let mut checks = Vec::new();
    let navigation = window.navigation();
    wait_tag(&navigation, "home", STATE_TIMEOUT).await?;

    let home = visible_page(&navigation)?;
    let home_widgets = descendants(home.upcast_ref());
    let home_text = widget_text(&home_widgets);
    if home_text.iter().any(|text| {
        text.contains("Этот компьютер")
            || text == "Адрес"
            || text == "По адресу"
            || text.contains("Поиск")
            || text.to_lowercase().contains("search")
    }) || home_widgets.iter().any(|widget| {
        widget.clone().downcast::<adw::EntryRow>().is_ok()
            || widget.clone().downcast::<gtk::SearchEntry>().is_ok()
    }) {
        return Err("home: содержит адрес, поиск или повторную карточку этого компьютера".into());
    }
    checks.push("home contains only the connection list".into());

    let add = find_button(&home_widgets, "Добавить подключение")
        .or_else(|| find_button(&home_widgets, "Добавить"))
        .ok_or_else(|| "home: не найдена кнопка «Добавить»".to_string())?;
    add.emit_clicked();
    wait_tag(&navigation, "add", STATE_TIMEOUT).await?;
    checks.push("open add page".into());

    let add_page = visible_page(&navigation)?;
    let add_widgets = descendants(add_page.upcast_ref());
    activate_action_row(&add_widgets, "По адресу")?;
    wait_tag(&navigation, "address", STATE_TIMEOUT).await?;
    checks.push("open address page".into());

    let address_page = visible_page(&navigation)?;
    let address_widgets = descendants(address_page.upcast_ref());
    let address = find_entry(&address_widgets, "Адрес")?;
    let name = find_entry(&address_widgets, "Название")?;
    let add_button = find_button(&address_widgets, "Добавить")
        .ok_or_else(|| "address: не найдена кнопка «Добавить»".to_string())?;

    address.set_text("pc:70000");
    if add_button.is_sensitive() {
        return Err("address: кнопка «Добавить» включена для недопустимого порта".into());
    }
    address.set_text("127.0.0.1:47999");
    name.set_text("Проверка");
    if !add_button.is_sensitive() {
        return Err("address: кнопка «Добавить» выключена для допустимого адреса".into());
    }
    add_button.emit_clicked();
    wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
    let store = window.store();
    let mut computer = store
        .items()
        .into_iter()
        .find(|item| item.host == "127.0.0.1" && item.port == 47999)
        .ok_or_else(|| "address: подключение не появилось в Store".to_string())?;
    if computer.name != "Проверка" {
        return Err("address: сохранено неверное название".into());
    }
    checks.push("validate and add a saved connection".into());

    crate::computer::open(window, &computer.id);
    wait_tag(&navigation, "computer", STATE_TIMEOUT).await?;
    let edit_page = visible_page(&navigation)?;
    let edit_widgets = descendants(edit_page.upcast_ref());
    let address = find_entry(&edit_widgets, "Адрес")?;
    let name = find_entry(&edit_widgets, "Название")?;
    let save = find_button(&edit_widgets, "Сохранить")
        .ok_or_else(|| "computer: не найдена кнопка «Сохранить»".to_string())?;

    let original = store
        .find(&computer.id)
        .ok_or_else(|| "computer: сохранённая запись исчезла перед редактированием".to_string())?;
    address.set_text("127.0.0.1:47998");
    name.set_text("Черновик");
    if store.find(&computer.id).as_ref() != Some(&original) {
        return Err("computer: изменения применились до нажатия «Сохранить»".into());
    }
    address.set_text("127.0.0.1:70000");
    if save.is_sensitive() {
        return Err("computer: «Сохранить» включена для недопустимого адреса".into());
    }
    address.set_text("127.0.0.1:47998");
    if !save.is_sensitive() {
        return Err("computer: «Сохранить» выключена для допустимых изменений".into());
    }
    save.emit_clicked();
    wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
    computer = store
        .find(&computer.id)
        .ok_or_else(|| "computer: запись исчезла после сохранения".to_string())?;
    if computer.host != "127.0.0.1" || computer.port != 47998 || computer.name != "Черновик"
    {
        return Err("computer: сохранены неверные поля".into());
    }
    let reopened_store = Store::open(&window.core().dir);
    if reopened_store.find(&computer.id).as_ref() != Some(&computer) {
        return Err("computer: запись не сохранилась после повторного чтения Store".into());
    }
    checks.push("edit atomically and reload saved connection".into());

    let mut blocked_file = BlockSettingsFile::new(&window.core().dir, "connections.ini")?;
    crate::computer::open(window, &computer.id);
    wait_tag(&navigation, "computer", STATE_TIMEOUT).await?;
    let failed_edit = visible_page(&navigation)?;
    let failed_widgets = descendants(failed_edit.upcast_ref());
    let address = find_entry(&failed_widgets, "Адрес")?;
    let name = find_entry(&failed_widgets, "Название")?;
    let save = find_button(&failed_widgets, "Сохранить")
        .ok_or_else(|| "computer write-error: не найдена кнопка «Сохранить»".to_string())?;
    address.set_text("127.0.0.1:47997");
    name.set_text("Не сохранено");
    if !save.is_sensitive() {
        return Err("computer write-error: допустимые изменения нельзя сохранить".into());
    }
    save.emit_clicked();
    if active_tag(&navigation).as_deref() != Some("computer") {
        return Err("computer write-error: страница закрылась после ошибки записи".into());
    }
    if address.text().as_str() != "127.0.0.1:47997" || name.text().as_str() != "Не сохранено"
    {
        return Err("computer write-error: введённые данные пропали после ошибки записи".into());
    }
    if store.find(&computer.id).as_ref() != Some(&computer) {
        return Err("computer write-error: Store изменился после ошибки записи".into());
    }
    blocked_file.restore()?;
    navigation.pop_to_tag("home");
    checks.push("failed write keeps form values and restores Store".into());

    window.open_dev_page("settings");
    wait_tag(&navigation, "settings", STATE_TIMEOUT).await?;
    let settings = descendants(visible_page(&navigation)?.upcast_ref());
    if find_row(&settings, "Пароль доступа")?.subtitle().as_deref() != Some("Не задан")
        || find_row(&settings, "Доверенные устройства")?
            .subtitle()
            .as_deref()
            != Some("Нет устройств")
    {
        return Err("settings: отсутствующие файлы доступа показаны как ошибка".into());
    }
    checks.push("open settings".into());
    navigation.pop_to_tag("home");
    window.open_dev_page("set-password");
    wait_tag(&navigation, "password", STATE_TIMEOUT).await?;
    let password_page = visible_page(&navigation)?;
    let password_widgets = descendants(password_page.upcast_ref());
    let first = find_password_entry(&password_widgets, "Новый пароль")?;
    let second = find_password_entry(&password_widgets, "Повторите пароль")?;
    let save = find_button(&password_widgets, "Сохранить")
        .ok_or_else(|| "password: не найдена кнопка «Сохранить»".to_string())?;
    const TEST_PASSWORD: &str = "ui-check-password-42";
    first.set_text(TEST_PASSWORD);
    second.set_text("ui-check-password-43");
    if save.is_sensitive() {
        return Err("password: сохранение включено для несовпадающих паролей".into());
    }
    second.set_text(TEST_PASSWORD);
    if !save.is_sensitive() {
        return Err(
            "password: сохранение выключено для совпадающих паролей длиной от 8 символов".into(),
        );
    }
    save.emit_clicked();
    wait_until("password save", STATE_TIMEOUT, || {
        active_tag(&navigation).as_deref() == Some("settings")
    })
    .await?;
    let password_path = window.core().dir.join("host-password");
    let password_bytes = fs::read(&password_path)
        .map_err(|error| format!("password: не удалось прочитать host-password: {error}"))?;
    if password_bytes
        .windows(TEST_PASSWORD.len())
        .any(|window| window == TEST_PASSWORD.as_bytes())
    {
        return Err("password: тестовый пароль записан открытым текстом".into());
    }
    checks.push("validate and save password without plaintext".into());

    navigation.pop_to_tag("home");
    let mut blocked_password = BlockSettingsFile::new(&window.core().dir, "host-password")?;
    window.open_dev_page("settings");
    let settings = descendants(visible_page(&navigation)?.upcast_ref());
    let password_row = find_row(&settings, "Пароль доступа")?;
    if password_row.subtitle().as_deref() != Some("Не удалось прочитать") {
        return Err("settings: ошибка чтения пароля показана как его отсутствие".into());
    }
    password_row.emit_by_name::<()>("activated", &[]);
    if active_tag(&navigation).as_deref() != Some("settings") {
        return Err("settings: открыта форма поверх нечитаемого пароля".into());
    }
    checks.push("unreadable password is an error and cannot open an empty password form".into());
    blocked_password.restore()?;
    navigation.pop_to_tag("home");
    window.open_dev_page("settings");
    if password_row.subtitle().as_deref() != Some("Задан") {
        return Err("settings: пароль не перечитан после восстановления файла".into());
    }
    checks.push("password state recovers after the settings file becomes readable".into());

    casement_net::auth::TrustStore::load(&window.core().dir)
        .and_then(|mut trust| trust.add(window.core().identity.fingerprint(), "GTK fixture"))
        .map_err(|e| e.to_string())?;
    let mut blocked_trust = BlockSettingsFile::new(&window.core().dir, "trusted-devices")?;
    navigation.pop_to_tag("home");
    window.open_dev_page("settings");
    let settings = descendants(visible_page(&navigation)?.upcast_ref());
    if find_row(&settings, "Доверенные устройства")?
        .subtitle()
        .as_deref()
        != Some("Не удалось прочитать")
    {
        return Err("settings: ошибка чтения доверенных показана как пустой список".into());
    }
    activate_action_row(&settings, "Доверенные устройства")?;
    wait_tag(&navigation, "trusted", STATE_TIMEOUT).await?;
    let trusted = descendants(visible_page(&navigation)?.upcast_ref());
    let retry =
        find_button(&trusted, "Повторить").ok_or("trusted: нет повторного чтения после ошибки")?;
    blocked_trust.restore()?;
    retry.emit_clicked();
    find_row(
        &descendants(visible_page(&navigation)?.upcast_ref()),
        "GTK fixture",
    )?;
    checks.push("unreadable trust list shows an error and Retry restores the saved devices".into());

    computer = store
        .find(&computer.id)
        .ok_or_else(|| "session: нет сохранённого loopback-подключения".to_string())?;
    navigation.pop_to_tag("home");
    wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
    crate::session::open(window, computer);
    wait_tag(&navigation, "session", STATE_TIMEOUT).await?;
    navigation.pop();
    wait_tag(&navigation, "home", STATE_TIMEOUT).await?;
    let popped_at = Instant::now();
    while popped_at.elapsed() < Duration::from_millis(100) {
        if active_tag(&navigation).as_deref() != Some("home") {
            return Err("session: после выхода не показана главная страница".into());
        }
        glib::timeout_future(POLL_INTERVAL).await;
    }
    if active_tag(&navigation).as_deref() != Some("home") {
        return Err("session: страница вернулась после выхода".into());
    }
    checks.push("back from connecting session stays on home".into());

    checks.extend(crate::session::check_session_lifetime(window).await?);
    checks.extend(crate::session::check_cursor(window).await?);
    checks.extend(crate::session::check_decoder_recovery(window).await?);
    Ok(checks)
}

fn require_fixture(window: &Window) -> Result<(), String> {
    if std::env::var_os("CASEMENT_SNAPSHOT").is_none() {
        return Err("ui-check: требуется запуск с CASEMENT_SNAPSHOT".into());
    }
    let Some(config_dir) = std::env::var_os("CASEMENT_CONFIG_DIR") else {
        return Err("ui-check: требуется отдельный CASEMENT_CONFIG_DIR fixture".into());
    };
    let config_dir = PathBuf::from(config_dir);
    if !config_dir.join(".casement-ui-fixture").is_file() {
        return Err("ui-check: требуется маркер .casement-ui-fixture в отдельном профиле".into());
    }
    if config_dir != window.core().dir {
        return Err("ui-check: CASEMENT_CONFIG_DIR не совпадает с каталогом Core".into());
    }
    if config_dir == glib::user_config_dir().join("casement") {
        return Err("ui-check: отказ от работы с пользовательским профилем Casement".into());
    }
    Ok(())
}

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![root.clone()];
    let mut next = root.first_child();
    while let Some(child) = next {
        next = child.next_sibling();
        found.extend(descendants(&child));
    }
    found
}

fn visible_widgets(widgets: &[gtk::Widget]) -> impl Iterator<Item = &gtk::Widget> {
    widgets.iter().filter(|widget| widget.is_visible())
}

fn widget_text(widgets: &[gtk::Widget]) -> Vec<String> {
    let mut text = Vec::new();
    for widget in visible_widgets(widgets) {
        if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
            text.push(label.label().to_string());
        }
        if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
            text.extend(button.label().map(|value| value.to_string()));
            text.extend(button.tooltip_text().map(|value| value.to_string()));
        }
        if let Ok(row) = widget.clone().downcast::<adw::ActionRow>() {
            text.push(row.title().to_string());
        }
        if let Ok(row) = widget.clone().downcast::<adw::EntryRow>() {
            text.push(row.title().to_string());
        }
        if let Ok(row) = widget.clone().downcast::<adw::PasswordEntryRow>() {
            text.push(row.title().to_string());
        }
    }
    text
}

fn find_button(widgets: &[gtk::Widget], label: &str) -> Option<gtk::Button> {
    visible_widgets(widgets).find_map(|widget| {
        let button = widget.clone().downcast::<gtk::Button>().ok()?;
        (button.label().as_deref() == Some(label)
            || button.tooltip_text().as_deref() == Some(label))
        .then_some(button)
    })
}

fn find_entry(widgets: &[gtk::Widget], title: &str) -> Result<adw::EntryRow, String> {
    visible_widgets(widgets)
        .find_map(|widget| {
            let row = widget.clone().downcast::<adw::EntryRow>().ok()?;
            (row.title().as_str() == title).then_some(row)
        })
        .ok_or_else(|| format!("не найдена строка ввода «{title}»"))
}

fn find_password_entry(
    widgets: &[gtk::Widget],
    title: &str,
) -> Result<adw::PasswordEntryRow, String> {
    visible_widgets(widgets)
        .find_map(|widget| {
            let row = widget.clone().downcast::<adw::PasswordEntryRow>().ok()?;
            (row.title().as_str() == title).then_some(row)
        })
        .ok_or_else(|| format!("не найдена строка пароля «{title}»"))
}

fn find_row(widgets: &[gtk::Widget], title: &str) -> Result<adw::ActionRow, String> {
    visible_widgets(widgets)
        .find_map(|widget| {
            let row = widget.clone().downcast::<adw::ActionRow>().ok()?;
            (row.title().as_str() == title).then_some(row)
        })
        .ok_or_else(|| format!("не найдена строка действия «{title}»"))
}

fn activate_action_row(widgets: &[gtk::Widget], title: &str) -> Result<(), String> {
    find_row(widgets, title)?.emit_by_name::<()>("activated", &[]);
    Ok(())
}

fn visible_page(navigation: &adw::NavigationView) -> Result<adw::NavigationPage, String> {
    navigation
        .visible_page()
        .ok_or_else(|| "navigation: нет видимой страницы".into())
}

fn active_tag(navigation: &adw::NavigationView) -> Option<String> {
    navigation
        .visible_page()
        .and_then(|page| page.tag().map(|tag| tag.to_string()))
}

async fn wait_tag(
    navigation: &adw::NavigationView,
    expected: &str,
    timeout: Duration,
) -> Result<(), String> {
    wait_until("navigation", timeout, || {
        active_tag(navigation).as_deref() == Some(expected)
    })
    .await
    .map_err(|error| {
        format!(
            "{error}: ожидалась страница «{expected}», сейчас {:?}",
            active_tag(navigation)
        )
    })
}

async fn wait_until(
    label: &str,
    timeout: Duration,
    mut condition: impl FnMut() -> bool,
) -> Result<(), String> {
    let started = Instant::now();
    while !condition() {
        if started.elapsed() >= timeout {
            return Err(format!(
                "{label}: истекло ожидание состояния за {timeout:?}"
            ));
        }
        glib::timeout_future(POLL_INTERVAL).await;
    }
    Ok(())
}

struct BlockSettingsFile {
    path: PathBuf,
    backup: PathBuf,
    blocked: bool,
}

impl BlockSettingsFile {
    fn new(config_dir: &Path, name: &str) -> Result<Self, String> {
        let path = config_dir.join(name);
        let backup = config_dir.join(format!("{name}.ui-check-backup"));
        if !path.is_file() {
            return Err(format!(
                "ui-check fixture: {} не является файлом",
                path.display()
            ));
        }
        if backup.exists() {
            return Err(format!(
                "ui-check fixture: уже существует {}",
                backup.display()
            ));
        }
        fs::rename(&path, &backup)
            .map_err(|error| format!("ui-check: резервирование {name}: {error}"))?;
        if let Err(error) = fs::create_dir(&path) {
            let _ = fs::rename(&backup, &path);
            return Err(format!("ui-check: блокировка {name}: {error}"));
        }
        Ok(Self {
            path,
            backup,
            blocked: true,
        })
    }

    fn restore(&mut self) -> Result<(), String> {
        if self.blocked {
            fs::remove_dir(&self.path)
                .map_err(|error| format!("ui-check: удалить пустую блокирующую папку: {error}"))?;
            self.blocked = false;
        }
        fs::rename(&self.backup, &self.path)
            .map_err(|error| format!("ui-check: восстановить файл настроек: {error}"))
    }
}

impl Drop for BlockSettingsFile {
    fn drop(&mut self) {
        if self.blocked {
            let _ = fs::remove_dir(&self.path);
            self.blocked = false;
        }
        if !self.path.exists() {
            let _ = fs::rename(&self.backup, &self.path);
        }
    }
}
