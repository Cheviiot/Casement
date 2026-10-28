//! The network core from the GTK side: a Tokio runtime for networking and a way to await
//! its work from the GLib main loop without blocking the window.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use casement_net::{ErrorCode, Identity, NetError};

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("casement-net")
            .enable_all()
            .build()
            .expect("Tokio runtime")
    })
}

/// Runs `future` on the network runtime and awaits its result on the caller's executor
/// (the GTK main loop).
pub async fn run<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut task = AbortOnDrop(Some(runtime().spawn(async move {
        let _ = tx.send(future.await);
    })));
    let result = rx.await.expect("network task ended without an answer");
    // The task has sent its result and is finishing. Dropping the JoinHandle detaches
    // it without cancelling successful work; cancellation while awaiting drops the guard.
    task.0.take();
    result
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// What every part of the application needs to reach the core.
pub struct Core {
    pub dir: PathBuf,
    pub name: String,
    pub identity: Arc<Identity>,
}

impl Core {
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        let identity = Identity::load_or_create(&dir).map_err(|e| e.to_string())?;
        Ok(Self {
            dir,
            name: glib::host_name().to_string(),
            identity: Arc::new(identity),
        })
    }
}

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CASEMENT_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        // %APPDATA%, as casement-cli and casement-host use (GLib's own choice is
        // %LOCALAPPDATA%, where 0.1.0 kept them: moved over once).
        let dir = PathBuf::from(appdata).join("casement");
        let old = glib::user_config_dir().join("casement");
        if !dir.join("device.key").exists() && old.join("device.key").exists() {
            let _ = std::fs::create_dir_all(&dir);
            for name in [
                "device.key",
                "connections.ini",
                "host-password",
                "trusted-devices",
                "accept-off",
            ] {
                let _ = std::fs::copy(old.join(name), dir.join(name));
            }
        }
        return dir;
    }
    glib::user_config_dir().join("casement")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct DropProbe(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[test]
    fn dropping_run_future_aborts_the_network_task() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        executor.block_on(async {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
            let waiting = tokio::spawn(async move {
                run(async move {
                    let _probe = DropProbe(Some(dropped_tx));
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                })
                .await
            });

            tokio::time::timeout(Duration::from_secs(1), started_rx)
                .await
                .expect("network task did not start")
                .expect("start signal was dropped");
            waiting.abort();
            let _ = waiting.await;
            tokio::time::timeout(Duration::from_secs(1), dropped_rx)
                .await
                .expect("dropping run did not abort its task")
                .expect("drop probe was not notified");
        });
    }
}

/// Russian text for people.
pub fn describe(error: &NetError) -> String {
    match error {
        NetError::Timeout | NetError::Connection(_) => "Компьютер не ответил".into(),
        NetError::AuthFailed => "Неверный пароль".into(),
        NetError::PasswordRequired => "Нужен пароль доступа".into(),
        NetError::Refused {
            code,
            retry_after,
            message,
        } => match code {
            ErrorCode::NoPasswordSet => "На том компьютере не задан пароль доступа".into(),
            ErrorCode::RateLimited => format!("Повторите через {retry_after} с"),
            ErrorCode::VersionMismatch => format!("Другая версия протокола Casement: {message}"),
            ErrorCode::NotAllowed => "Тот компьютер не разрешает показ экрана".into(),
            _ => format!("Компьютер отказал: {message}"),
        },
        NetError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "Компьютер с таким именем не найден".into()
        }
        NetError::Protocol(message) => match known_refusal(message) {
            Some(text) => text.into(),
            None => format!("Ошибка соединения: {message}"),
        },
        other => format!("Ошибка соединения: {other}"),
    }
}

/// What the other computer says about files, in words for people.
fn known_refusal(message: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        (
            "does not share its files",
            "Тот компьютер не открывает свои файлы",
        ),
        ("does not accept files", "Тот компьютер не принимает файлы"),
        (
            "outside the shared folder",
            "Это за пределами открытой папки",
        ),
        (
            "no such file or folder",
            "Такого файла или папки там больше нет",
        ),
        ("a folder, not a file", "Это папка, а не файл"),
        (
            "cannot open the folder",
            "Папку не удалось открыть: нет доступа",
        ),
        (
            "cannot open the file",
            "Файл не удалось открыть: нет доступа",
        ),
        (
            "checksum differs",
            "Файл пришёл повреждённым, попробуйте ещё раз",
        ),
        (
            "changed while it was being sent",
            "Файл менялся во время передачи, попробуйте ещё раз",
        ),
        (
            "the file ended early",
            "Передача оборвалась, попробуйте ещё раз",
        ),
    ];
    KNOWN
        .iter()
        .find(|(english, _)| message.contains(english))
        .map(|(_, russian)| *russian)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_in_russian() {
        let e = NetError::Protocol("this computer does not share its files".into());
        assert_eq!(describe(&e), "Тот компьютер не открывает свои файлы");
        let e = NetError::Protocol("something new".into());
        assert_eq!(describe(&e), "Ошибка соединения: something new");
    }
}
