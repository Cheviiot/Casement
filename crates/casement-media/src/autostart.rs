//! The background host casement-host, which accepts connections while nobody has the
//! window open: on Linux the systemd user service; on Windows the system service
//! "Casement" when the installer set it up, otherwise an entry in the user's autostart
//! (HKCU Run) plus the running process. Shared by the application's "Принимать
//! подключения" and `casement-cli accept` / `setup`. These calls block briefly (they run
//! systemctl, sc, reg or taskkill).

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

/// Windows with the system service: this computer's key, password and trusted devices are
/// the machine's, in %ProgramData%\Casement (only SYSTEM and administrators may change
/// them), not the user's.
pub fn system_service() -> bool {
    imp::system_service()
}

/// Where the system service keeps the machine's settings and logs.
#[cfg(windows)]
pub fn machine_settings_dir() -> std::path::PathBuf {
    std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"))
        .join("Casement")
}

/// What anyone on this computer may know about the system service's settings: they are
/// readable by administrators only, so the service publishes this summary for the
/// application.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineStatus {
    pub fingerprint: String,
    pub password: bool,
    pub trusted: usize,
}

#[cfg(windows)]
impl MachineStatus {
    fn path() -> std::path::PathBuf {
        machine_settings_dir().join("public").join("status")
    }

    fn to_text(&self) -> String {
        format!(
            "fingerprint={}\npassword={}\ntrusted={}\n",
            self.fingerprint, self.password as u8, self.trusted
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut status = Self::default();
        for line in text.lines() {
            match line.split_once('=')? {
                ("fingerprint", v) => status.fingerprint = v.to_string(),
                ("password", v) => status.password = v == "1",
                ("trusted", v) => status.trusted = v.parse().ok()?,
                _ => {}
            }
        }
        (!status.fingerprint.is_empty()).then_some(status)
    }

    /// The summary the service last published.
    pub fn load() -> Option<Self> {
        Self::parse(&std::fs::read_to_string(Self::path()).ok()?)
    }

    /// Service side: writes the summary when it changed.
    pub fn publish(&self) -> std::io::Result<()> {
        if Self::load().as_ref() == Some(self) {
            return Ok(());
        }
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("new");
        std::fs::write(&temporary, self.to_text())?;
        std::fs::rename(temporary, path)
    }
}

/// Opens the setup wizard `casement-cli setup` as administrator (Windows asks for
/// permission) and waits until it is closed.
#[cfg(windows)]
pub fn configure_as_admin() -> Result<(), String> {
    imp::elevated(&["setup"], true).map(|_| ())
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

    pub fn system_service() -> bool {
        false
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

    /// Console tools (sc, reg, taskkill, tasklist) must not flash a window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SERVICE: &str = "Casement";
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Casement host";
    const ACCESS_DENIED: i32 = 5;

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

    /// Runs `sc ARGS`; access denied is reported in words.
    fn sc(args: &[&str]) -> Result<String, String> {
        let out = quiet("sc", args)?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        match out.status.code() {
            Some(0) => Ok(text),
            Some(ACCESS_DENIED) => Err("нужны права администратора".into()),
            _ => Err(text
                .lines()
                .last()
                .unwrap_or("sc failed")
                .trim()
                .to_string()),
        }
    }

    pub fn system_service() -> bool {
        sc(&["query", SERVICE]).is_ok()
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
        system_service() || host_exe().is_some_and(|p| p.is_file())
    }

    pub fn enabled() -> bool {
        if system_service() {
            return sc(&["qc", SERVICE]).is_ok_and(|text| text.contains("AUTO_START"));
        }
        quiet("reg", &["query", RUN_KEY, "/v", VALUE]).is_ok_and(|o| o.status.success())
    }

    /// Runs `casement-cli ARGS` as administrator and waits for it; Windows asks the user
    /// first. Returns its exit code.
    pub fn elevated(args: &[&str], show: bool) -> Result<u32, String> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, WaitForSingleObject, INFINITE,
        };
        use windows_sys::Win32::UI::Shell::{
            ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE, SHELLEXECUTEINFOW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

        let wide =
            |text: &std::ffi::OsStr| -> Vec<u16> { text.encode_wide().chain(Some(0)).collect() };
        let exe = std::env::current_exe()
            .ok()
            .and_then(|e| Some(e.parent()?.join("casement-cli.exe")))
            .filter(|p| p.is_file())
            .ok_or("casement-cli.exe not found")?;
        let file = wide(exe.as_os_str());
        let verb = wide("runas".as_ref());
        let parameters = wide(args.join(" ").as_ref());
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = parameters.as_ptr();
            info.nShow = if show { SW_SHOWNORMAL } else { SW_HIDE };
            if ShellExecuteExW(&mut info) == 0 {
                return Err(if GetLastError() == ERROR_CANCELLED {
                    "отменено".into()
                } else {
                    std::io::Error::last_os_error().to_string()
                });
            }
            if info.hProcess.is_null() {
                return Ok(0);
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 0u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            Ok(code)
        }
    }

    pub fn set_enabled(on: bool) -> Result<(), String> {
        if system_service() {
            // Changing a service needs an administrator: from the window, ask Windows.
            if unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() } == 0 {
                return match elevated(&["accept", if on { "on" } else { "off" }], false)? {
                    0 => Ok(()),
                    code => Err(format!("casement-cli accept: код {code}")),
                };
            }
            return if on {
                sc(&["config", SERVICE, "start=", "auto"])?;
                // Already running is fine.
                let _ = sc(&["start", SERVICE]);
                Ok(())
            } else {
                sc(&["config", SERVICE, "start=", "disabled"])?;
                let _ = sc(&["stop", SERVICE]);
                Ok(())
            };
        }
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
        // The system service is started by Windows; only the per-user host needs help.
        if !system_service() && enabled() && !running() {
            let _ = start();
        }
    }
}
