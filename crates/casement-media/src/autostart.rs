//! The background host casement-host, which accepts connections while nobody has the
//! window open: on Linux the systemd user service, on Windows an entry in the user's
//! autostart (HKCU Run) plus the running process. Shared by the application's "Принимать
//! подключения" and `casement-cli accept` / `setup`. These calls block briefly (they run
//! systemctl, reg or taskkill).

/// The service can be driven here (installed next to the programs or as a unit).
pub fn installed() -> bool {
    imp::installed()
}

/// It is switched on: started at login.
pub fn enabled() -> bool {
    imp::enabled()
}

/// Switches it on and starts it, or off and stops it.
pub fn set_enabled(on: bool) -> Result<(), String> {
    imp::set_enabled(on)
}

/// Starts it if it is switched on but not running: enabled by a package while this session
/// was already open, or (Windows) after an update or a crash.
pub fn ensure_running() {
    imp::ensure_running()
}

#[cfg(not(windows))]
mod imp {
    use std::process::Command;

    const UNIT: &str = "casement-host.service";

    fn systemctl(args: &[&str]) -> Result<String, String> {
        let out = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .map_err(|e| format!("systemctl: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if stderr.is_empty() { stdout } else { stderr })
        }
    }

    pub fn installed() -> bool {
        let user_dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
            .map(|config| config.join("systemd/user"));
        user_dir
            .into_iter()
            .chain(["/usr/lib/systemd/user".into(), "/etc/systemd/user".into()])
            .any(|dir| dir.join(UNIT).exists())
    }

    pub fn enabled() -> bool {
        // "enabled" also when the package enabled it for everyone (--global).
        systemctl(&["is-enabled", UNIT]).is_ok_and(|state| state == "enabled")
    }

    pub fn set_enabled(on: bool) -> Result<(), String> {
        if on {
            // A user who switched it off earlier may have masked it (see below).
            let _ = systemctl(&["unmask", UNIT]);
            systemctl(&["enable", "--now", UNIT]).map(|_| ())
        } else {
            systemctl(&["disable", "--now", UNIT])?;
            // Enabled for everyone by the package: only a mask turns it off for this user.
            if enabled() {
                systemctl(&["mask", "--now", UNIT])?;
            }
            Ok(())
        }
    }

    pub fn ensure_running() {
        // Enabled by the package for everyone, but this session started before that.
        if enabled() && systemctl(&["is-active", UNIT]).is_err() {
            let _ = systemctl(&["start", UNIT]);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    /// Console tools (reg, taskkill, tasklist) must not flash a window.
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
            .map_err(|e| format!("{program}: {e}"))
    }

    fn user_filter() -> String {
        format!(
            "USERNAME eq {}",
            std::env::var("USERNAME").unwrap_or_default()
        )
    }

    fn running() -> bool {
        quiet(
            "tasklist",
            &[
                "/FI",
                "IMAGENAME eq casement-host.exe",
                "/FI",
                &user_filter(),
                "/NH",
            ],
        )
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("casement-host.exe"))
    }

    fn stop() {
        let _ = quiet(
            "taskkill",
            &["/F", "/IM", "casement-host.exe", "/FI", &user_filter()],
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

    pub fn installed() -> bool {
        host_exe().is_some_and(|p| p.is_file())
    }

    pub fn enabled() -> bool {
        quiet("reg", &["query", RUN_KEY, "/v", VALUE]).is_ok_and(|o| o.status.success())
    }

    pub fn set_enabled(on: bool) -> Result<(), String> {
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
    }

    pub fn ensure_running() {
        if enabled() && !running() {
            let _ = start();
        }
    }
}
