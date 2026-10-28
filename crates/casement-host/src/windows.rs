//! casement-host as a Windows service. Services live in session 0, which has no desktop, so
//! the service (SYSTEM, started with Windows) runs a worker — this program with `--worker`
//! — in the session on the console, under the service's own token moved to that session.
//! Running as SYSTEM there, the worker can capture and drive the login screen, the lock
//! screen and UAC prompts, which live on the separate "Winlogon" desktop.
//!
//! A process captures and sends input only on the desktop it was started on. The worker
//! watches the input desktop and, when it changes, exits with a code naming the new one;
//! the service starts it again there. Clients see a short break and reconnect.

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId,
    TOKEN_ALL_ACCESS,
};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, STARTUPINFOW,
};

pub const SERVICE_NAME: &str = "Casement";

/// Desktops of the console's window station, by the code a worker exits with to ask for
/// one of them. Anything else is treated as the user's desktop.
const DESKTOPS: [&str; 3] = ["Default", "Winlogon", "Screen-saver"];
const SWITCH_CODE: u32 = 0x40;
const NO_SESSION: u32 = u32::MAX;
/// Desktop access needed to read the input desktop's name.
const DESKTOP_READOBJECTS: u32 = 0x0001;

/// The folder the service and its workers keep settings and logs in.
pub fn machine_dir() -> PathBuf {
    casement_media::autostart::machine_settings_dir()
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// The name of the desktop that now receives input, if it can be read.
pub fn input_desktop() -> Option<String> {
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return None;
        }
        let mut name = [0u16; 128];
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            desktop as HANDLE,
            UOI_NAME,
            name.as_mut_ptr() as *mut c_void,
            (name.len() * 2) as u32,
            &mut needed,
        );
        CloseDesktop(desktop);
        if ok == 0 {
            return None;
        }
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        Some(String::from_utf16_lossy(&name[..len]))
    }
}

fn desktop_code(name: &str) -> u32 {
    let index = DESKTOPS
        .iter()
        .position(|d| d.eq_ignore_ascii_case(name))
        .unwrap_or(0);
    SWITCH_CODE + index as u32
}

/// Worker side: leaves the process, asking for the right desktop, as soon as the input
/// desktop is not `own` (checked at once, then four times a second).
pub fn follow_input_desktop(own: String) {
    std::thread::spawn(move || loop {
        if let Some(input) = input_desktop() {
            if !input.eq_ignore_ascii_case(&own) {
                tracing::info!("input moved to the {input} desktop; restarting there");
                std::process::exit(desktop_code(&input) as i32);
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    });
}

/// Worker side: keeps the summary of the machine's settings that the application may read
/// up to date (they change from `casement-cli` and when a device becomes trusted).
pub async fn publish_status(dir: PathBuf, fingerprint: String) {
    use casement_media::autostart::MachineStatus;
    use casement_net::auth::{StoredPassword, TrustStore};
    loop {
        let status = MachineStatus {
            fingerprint: fingerprint.clone(),
            password: matches!(StoredPassword::load(&dir), Ok(Some(_))),
            trusted: TrustStore::load(&dir).map_or(0, |t| t.devices().len()),
        };
        if let Err(e) = status.publish() {
            tracing::warn!("cannot publish the status: {e}");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// A worker started in a session.
struct Worker {
    process: HANDLE,
    session: u32,
}

impl Worker {
    /// Starts `exe --worker --desktop DESKTOP` in `session` with this process's token.
    fn start(exe: &Path, session: u32, desktop: &str) -> Result<Self, String> {
        unsafe {
            let mut own: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own) == 0 {
                return Err(format!(
                    "OpenProcessToken: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut token: HANDLE = std::ptr::null_mut();
            let duplicated = DuplicateTokenEx(
                own,
                TOKEN_ALL_ACCESS,
                std::ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut token,
            );
            CloseHandle(own);
            if duplicated == 0 {
                return Err(format!(
                    "DuplicateTokenEx: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let set = SetTokenInformation(
                token,
                TokenSessionId,
                &session as *const u32 as *const c_void,
                std::mem::size_of::<u32>() as u32,
            );
            if set == 0 {
                CloseHandle(token);
                return Err(format!(
                    "SetTokenInformation: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut command: Vec<u16> = wide(&OsString::from(format!(
                "\"{}\" --worker --desktop {desktop}",
                exe.display()
            )));
            let mut station: Vec<u16> = wide(OsStr::new(&format!("winsta0\\{desktop}")));
            let directory = exe.parent().map(|d| wide(d.as_os_str()));
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            startup.lpDesktop = station.as_mut_ptr();
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessAsUserW(
                token,
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                std::ptr::null(),
                directory.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                &startup,
                &mut info,
            );
            CloseHandle(token);
            if created == 0 {
                return Err(format!(
                    "CreateProcessAsUserW: {}",
                    std::io::Error::last_os_error()
                ));
            }
            CloseHandle(info.hThread);
            Ok(Self {
                process: info.hProcess,
                session,
            })
        }
    }

    /// The exit code once the worker has ended.
    fn exited(&self) -> Option<u32> {
        unsafe {
            if WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.process, &mut code);
            Some(code)
        }
    }

    fn stop(self) {
        unsafe {
            if self.exited().is_none() {
                TerminateProcess(self.process, 0);
                WaitForSingleObject(self.process, 5000);
            }
            CloseHandle(self.process);
        }
    }
}

enum Event {
    Stop,
    SessionChanged,
}

/// Keeps one worker on the console session's input desktop until told to stop.
fn supervise(events: mpsc::Receiver<Event>) {
    let Ok(exe) = std::env::current_exe() else {
        tracing::error!("cannot find casement-host.exe");
        return;
    };
    let mut worker: Option<Worker> = None;
    let mut desktop = DESKTOPS[0];
    let mut failures = 0u32;
    loop {
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if let Some(current) = worker.take() {
            match current.exited() {
                Some(code)
                    if (SWITCH_CODE..SWITCH_CODE + DESKTOPS.len() as u32).contains(&code) =>
                {
                    desktop = DESKTOPS[(code - SWITCH_CODE) as usize];
                    current.stop();
                    failures = 0;
                }
                Some(code) => {
                    tracing::warn!("worker ended with code {code:#x}");
                    current.stop();
                    failures += 1;
                }
                None if current.session != session => {
                    tracing::info!("console moved to session {session}");
                    current.stop();
                    desktop = DESKTOPS[0];
                }
                None => worker = Some(current),
            }
        }
        if worker.is_none() && session != NO_SESSION {
            // A worker that keeps failing is retried more slowly, up to every 30 s.
            let pause = Duration::from_secs((1u64 << failures.min(5)).min(30));
            if failures > 0 && wait(&events, pause) {
                break;
            }
            match Worker::start(&exe, session, desktop) {
                Ok(started) => {
                    tracing::info!("worker in session {session} on the {desktop} desktop");
                    worker = Some(started);
                }
                Err(e) => {
                    tracing::error!("cannot start the worker: {e}");
                    failures += 1;
                }
            }
        }
        if wait(&events, Duration::from_millis(300)) {
            break;
        }
    }
    if let Some(current) = worker {
        current.stop();
    }
}

/// Waits up to `pause` for an event; true when the service must stop.
fn wait(events: &mpsc::Receiver<Event>, pause: Duration) -> bool {
    match events.recv_timeout(pause) {
        Ok(Event::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
        Ok(Event::SessionChanged) | Err(mpsc::RecvTimeoutError::Timeout) => false,
    }
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service_body() {
        tracing::error!("service: {e}");
    }
}

fn run_service_body() -> windows_service::Result<()> {
    let (sender, events) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = sender.send(Event::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            let _ = sender.send(Event::SessionChanged);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state, accepted| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepted,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
    };
    report(
        ServiceState::Running,
        ServiceControlAccept::STOP
            | ServiceControlAccept::SHUTDOWN
            | ServiceControlAccept::SESSION_CHANGE,
    )?;
    tracing::info!("service started");
    supervise(events);
    tracing::info!("service stopped");
    report(ServiceState::Stopped, ServiceControlAccept::empty())
}

/// Hands this process to the service control manager; returns when the service stops.
pub fn run_service() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_codes() {
        assert_eq!(desktop_code("Winlogon"), SWITCH_CODE + 1);
        assert_eq!(desktop_code("default"), SWITCH_CODE);
        assert_eq!(desktop_code("Something"), SWITCH_CODE);
    }
}
