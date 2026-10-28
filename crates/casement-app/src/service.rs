//! The background host `casement-host`, which accepts connections while nobody has the
//! window open. On Linux it is the systemd user service; on Windows an entry in the user's
//! autostart (HKCU Run) plus the running process. "Принимать подключения" drives it.

/// The service is there to be driven (installed next to the application).
pub fn installed() -> bool {
    imp::installed()
}

/// It is switched on.
pub async fn is_active() -> bool {
    imp::is_active().await
}

/// Switches it on (and starts it) or off (and stops it).
pub async fn set_enabled(on: bool) -> Result<(), String> {
    imp::set_enabled(on).await
}

/// Restarts it if it is on: the host reads the password and trusted devices at start.
pub async fn restart() {
    imp::restart().await
}

#[cfg(not(windows))]
mod imp {
    const UNIT: &str = "casement-host.service";

    pub fn installed() -> bool {
        let user_unit = glib::user_config_dir().join("systemd/user").join(UNIT);
        user_unit.exists()
            || ["/usr/lib/systemd/user", "/etc/systemd/user"]
                .iter()
                .any(|dir| std::path::Path::new(dir).join(UNIT).exists())
    }

    /// Runs `systemctl --user ARGS` through GLib (no Rust process spawning, see
    /// glibc_compat) and returns stdout.
    async fn systemctl(args: &[&str]) -> Result<String, String> {
        let mut argv: Vec<&std::ffi::OsStr> = vec!["systemctl".as_ref(), "--user".as_ref()];
        argv.extend(args.iter().map(|a| std::ffi::OsStr::new(*a)));
        let process = gtk::gio::Subprocess::newv(
            &argv,
            gtk::gio::SubprocessFlags::STDOUT_PIPE | gtk::gio::SubprocessFlags::STDERR_PIPE,
        )
        .map_err(|e| e.to_string())?;
        let (out, err) = process
            .communicate_utf8_future(None)
            .await
            .map_err(|e| e.to_string())?;
        if process.is_successful() {
            Ok(out.map(|o| o.to_string()).unwrap_or_default())
        } else {
            Err(err
                .map(|e| e.trim().to_string())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "systemctl failed".into()))
        }
    }

    pub async fn is_active() -> bool {
        systemctl(&["is-active", UNIT])
            .await
            .map(|s| s.trim() == "active")
            .unwrap_or(false)
    }

    pub async fn set_enabled(on: bool) -> Result<(), String> {
        let verb = if on { "enable" } else { "disable" };
        systemctl(&[verb, "--now", UNIT]).await.map(|_| ())
    }

    pub async fn restart() {
        let _ = systemctl(&["try-restart", UNIT]).await;
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    /// Console tools (reg, taskkill) must not flash a window over the application.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Casement host";

    fn host_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("casement-host.exe"))
    }

    fn quiet(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
        Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())
    }

    /// Runs blocking work off the main loop.
    async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        crate::core::run(async move { tokio::task::spawn_blocking(work).await.expect("task") })
            .await
    }

    pub fn installed() -> bool {
        host_exe().is_some_and(|p| p.is_file())
    }

    pub async fn is_active() -> bool {
        blocking(|| {
            quiet("reg", &["query", RUN_KEY, "/v", VALUE]).is_ok_and(|o| o.status.success())
        })
        .await
    }

    fn stop() {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let filter = format!("USERNAME eq {user}");
        let _ = quiet(
            "taskkill",
            &["/F", "/IM", "casement-host.exe", "/FI", &filter],
        );
    }

    fn start() -> Result<(), String> {
        let exe = host_exe().ok_or("casement-host.exe not found")?;
        Command::new(&exe)
            .current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub async fn set_enabled(on: bool) -> Result<(), String> {
        blocking(move || {
            if on {
                let exe = host_exe().ok_or("casement-host.exe not found")?;
                let command = format!("\"{}\"", exe.display());
                let out = quiet(
                    "reg",
                    &[
                        "add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &command, "/f",
                    ],
                )?;
                if !out.status.success() {
                    return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
                }
                stop();
                start()
            } else {
                let _ = quiet("reg", &["delete", RUN_KEY, "/v", VALUE, "/f"]);
                stop();
                Ok(())
            }
        })
        .await
    }

    pub async fn restart() {
        if is_active().await {
            blocking(|| {
                stop();
                let _ = start();
            })
            .await;
        }
    }
}
