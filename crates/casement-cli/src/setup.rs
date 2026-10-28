//! Setting up this computer without the window: the `setup` wizard and the `status`,
//! `accept`, `password` and `trusted` commands. Texts are for people, so Russian.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use casement_net::auth::{StoredPassword, TrustStore};
use casement_net::{Fingerprint, Identity, DEFAULT_PORT};

/// The same rule as the application's password page.
const MIN_PASSWORD: usize = 8;

/// Read from the environment instead of the terminal, for scripts.
pub const PASSWORD_ENV: &str = "CASEMENT_CLI_PASSWORD";

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// A yes/no question; Enter takes `default`.
fn ask(question: &str, default: bool) -> Result<bool> {
    let hint = if default { "[Д/н]" } else { "[д/Н]" };
    loop {
        let answer = read_line(&format!("{question} {hint} "))?.to_lowercase();
        match answer.as_str() {
            "" => return Ok(default),
            "д" | "да" | "y" | "yes" => return Ok(true),
            "н" | "нет" | "n" | "no" => return Ok(false),
            _ => println!("Ответьте «д» или «н»."),
        }
    }
}

pub fn read_password(prompt: &str) -> Result<String> {
    if let Ok(password) = std::env::var(PASSWORD_ENV) {
        return Ok(password);
    }
    Ok(rpassword::prompt_password(prompt)?)
}

/// Asks for a new password twice (once from the environment) until it is long enough and
/// both match.
fn new_password() -> Result<String> {
    let from_env = std::env::var_os(PASSWORD_ENV).is_some();
    loop {
        let first = read_password(&format!("Пароль (не короче {MIN_PASSWORD} символов): "))?;
        if first.chars().count() < MIN_PASSWORD {
            if from_env {
                bail!("пароль короче {MIN_PASSWORD} символов");
            }
            println!("Слишком короткий.");
            continue;
        }
        if from_env {
            return Ok(first);
        }
        if read_password("Ещё раз: ")? == first {
            return Ok(first);
        }
        println!("Пароли не совпадают.");
    }
}

fn save_password(dir: &Path, password: &str) -> Result<()> {
    StoredPassword::create(password)?.save(dir)?;
    Ok(())
}

fn has_password(dir: &Path) -> bool {
    matches!(StoredPassword::load(dir), Ok(Some(_)))
}

/// Whether connections are accepted, in words; None when there is no background service.
fn accepting() -> Option<bool> {
    autostart::installed().then(autostart::enabled)
}

pub fn password(dir: &Path, action: &str) -> Result<()> {
    if action == "clear" {
        StoredPassword::remove(dir)?;
        println!("Пароль убран: подключаться могут только доверенные устройства.");
    } else {
        save_password(dir, &new_password()?)?;
        println!("Пароль сохранён.");
    }
    Ok(())
}

pub fn accept(on: bool) -> Result<()> {
    if !autostart::installed() {
        bail!(
            "служба casement-host не установлена: подключения принимаются, пока открыт \
             Casement или запущен «casement-cli listen»"
        );
    }
    autostart::set_enabled(on).map_err(anyhow::Error::msg)?;
    println!(
        "{}",
        if on {
            "Подключения принимаются."
        } else {
            "Подключения не принимаются."
        }
    );
    Ok(())
}

pub fn status(dir: &Path, identity: &Identity, name: &str) -> Result<()> {
    if autostart::installed() {
        autostart::ensure_running();
    }
    let trust = TrustStore::load(dir)?;
    let accept = match accepting() {
        Some(true) => format!("включён (служба casement-host, UDP {DEFAULT_PORT})"),
        Some(false) => "выключен".to_string(),
        None => "служба не установлена".to_string(),
    };
    let rows = [
        ("Компьютер", name.to_string()),
        ("Отпечаток", identity.fingerprint().display()),
        ("Приём", accept),
        (
            "Пароль",
            if has_password(dir) {
                "задан"
            } else {
                "не задан"
            }
            .to_string(),
        ),
        ("Доверенные", trust.devices().len().to_string()),
        ("Настройки", dir.display().to_string()),
    ];
    for (title, value) in rows {
        println!("{title:<12}{value}");
    }
    Ok(())
}

pub fn trusted_list(dir: &Path) -> Result<()> {
    let trust = TrustStore::load(dir)?;
    if trust.devices().is_empty() {
        println!("Доверенных устройств нет.");
    }
    for (n, (fingerprint, name)) in trust.devices().iter().enumerate() {
        let name = if name.is_empty() {
            "без имени"
        } else {
            name
        };
        println!("{:>2}  {name:<24}{}", n + 1, fingerprint.display());
    }
    Ok(())
}

/// The device a person means: its number in the list, its name, or the start of its
/// fingerprint (at least 4 hex digits, dashes ignored).
fn find_device(devices: &[(Fingerprint, String)], what: &str) -> Result<Fingerprint> {
    if let Ok(n) = what.parse::<usize>() {
        if (1..=devices.len()).contains(&n) {
            return Ok(devices[n - 1].0);
        }
    }
    let by_name: Vec<_> = devices
        .iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(what))
        .collect();
    let hex = what.replace('-', "").to_lowercase();
    let by_key: Vec<_> = if hex.len() >= 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        devices
            .iter()
            .filter(|(fp, _)| fp.to_hex().starts_with(&hex))
            .collect()
    } else {
        Vec::new()
    };
    match (by_name.as_slice(), by_key.as_slice()) {
        ([one], _) | ([], [one]) => Ok(one.0),
        ([], []) => bail!("нет такого устройства: «{what}» (см. casement-cli trusted list)"),
        _ => bail!("под «{what}» подходят несколько устройств, укажите номер"),
    }
}

pub fn trusted_remove(dir: &Path, what: &str) -> Result<()> {
    let mut trust = TrustStore::load(dir)?;
    let fingerprint = find_device(trust.devices(), what)?;
    trust.remove(&fingerprint)?;
    println!("Устройство больше не доверенное: при следующем подключении спросит пароль.");
    Ok(())
}

/// The wizard: password, accepting connections, trusted devices, then the summary.
pub fn wizard(dir: &Path, identity: &Identity, name: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "мастеру нужен терминал; без него — casement-cli status, accept on|off, \
             password set (пароль из {PASSWORD_ENV}), trusted list|remove"
        );
    }
    println!("Casement: настройка этого компьютера\n");
    println!("  Компьютер  {name}");
    println!("  Отпечаток  {}\n", identity.fingerprint().display());

    println!("1. Пароль доступа");
    let change = if has_password(dir) {
        ask("   Пароль задан. Сменить?", false)?
    } else {
        println!("   Не задан: войти могут только доверенные устройства, а их пока нет.");
        ask("   Задать сейчас?", true)?
    };
    if change {
        save_password(dir, &new_password()?)?;
        println!("   Сохранён.");
    }

    println!("\n2. Приём подключений");
    match accepting() {
        Some(now) => {
            let on = ask(
                &format!(
                    "   Сейчас {}. Принимать подключения?",
                    if now {
                        "включён"
                    } else {
                        "выключен"
                    }
                ),
                true,
            )?;
            if on != now {
                autostart::set_enabled(on).map_err(anyhow::Error::msg)?;
            }
            println!(
                "   {}",
                if on {
                    "Включён."
                } else {
                    "Выключен."
                }
            );
        }
        None => println!(
            "   Служба casement-host не установлена: подключения принимаются, пока открыт \
             Casement или запущен «casement-cli listen»."
        ),
    }

    let trust = TrustStore::load(dir).context("доверенные устройства")?;
    if !trust.devices().is_empty() {
        println!("\n3. Доверенные устройства (входят без пароля)");
        loop {
            let trust = TrustStore::load(dir)?;
            if trust.devices().is_empty() {
                println!("   Не осталось.");
                break;
            }
            for (n, (fingerprint, device)) in trust.devices().iter().enumerate() {
                let device = if device.is_empty() {
                    "без имени"
                } else {
                    device
                };
                println!("   {:>2}  {device:<24}{}", n + 1, fingerprint.display());
            }
            let what = read_line("   Убрать устройство (номер), Enter — оставить всех: ")?;
            if what.is_empty() {
                break;
            }
            match trusted_remove(dir, &what) {
                Ok(()) => {}
                Err(e) => println!("   {e}"),
            }
        }
    }

    println!("\nГотово.\n");
    status(dir, identity, name)
}

/// Without the background service (other systems), nothing to switch.
#[cfg(any(target_os = "linux", windows))]
use casement_media::autostart;

#[cfg(not(any(target_os = "linux", windows)))]
mod autostart {
    pub fn installed() -> bool {
        false
    }
    pub fn enabled() -> bool {
        false
    }
    pub fn set_enabled(_on: bool) -> Result<(), String> {
        Err("not on this system".into())
    }
    pub fn ensure_running() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_by_number_name_or_key() {
        let a = Fingerprint([0xab; 32]);
        let b = Fingerprint([0xac; 32]);
        let devices = vec![(a, "Ноутбук".to_string()), (b, "office".to_string())];
        assert_eq!(find_device(&devices, "1").unwrap(), a);
        assert_eq!(find_device(&devices, "OFFICE").unwrap(), b);
        assert_eq!(find_device(&devices, "ACAC-AC").unwrap(), b);
        assert!(find_device(&devices, "a").is_err(), "too short for a key");
        assert!(find_device(&devices, "3").is_err());
        assert!(find_device(&devices, "nobody").is_err());
    }
}
