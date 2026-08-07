use std::{
    error::Error,
    io,
    sync::{Mutex, OnceLock},
};

use tauri::WindowEvent;
use tauri_plugin_shell::{ShellExt, process::CommandChild};

struct IunaSidecar(Mutex<Option<CommandChild>>);
struct IunaSleepInhibitor(Mutex<Option<SleepInhibitor>>);

static SIDECAR: OnceLock<IunaSidecar> = OnceLock::new();
static SLEEP_INHIBITOR: OnceLock<IunaSleepInhibitor> = OnceLock::new();

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let (mut events, child) = app
                .shell()
                .sidecar("iuna-sidecar")
                .map_err(setup_error)?
                .spawn()
                .map_err(setup_error)?;

            keep_system_awake_while_node_runs();

            tauri::async_runtime::spawn(async move { while events.recv().await.is_some() {} });

            let sidecar = SIDECAR.get_or_init(|| IunaSidecar(Mutex::new(None)));
            *sidecar.0.lock().expect("sidecar mutex poisoned") = Some(child);
            Ok(())
        })
        .on_window_event(|_window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                stop_sidecar();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building iuna desktop")
        .run(|_app, event| {
            if matches!(
                event,
                tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
            ) {
                stop_sidecar();
            }
        });
}

fn stop_sidecar() {
    if let Some(sidecar) = SIDECAR.get() {
        if let Some(child) = sidecar.0.lock().expect("sidecar mutex poisoned").take() {
            let _ = child.kill();
        }
    }
    release_system_awake_inhibitor();
}

fn setup_error(error: impl Error + Send + Sync + 'static) -> Box<dyn Error> {
    Box::new(io::Error::other(error))
}

fn keep_system_awake_while_node_runs() {
    match SleepInhibitor::acquire() {
        Ok(inhibitor) => {
            let store = SLEEP_INHIBITOR.get_or_init(|| IunaSleepInhibitor(Mutex::new(None)));
            *store.0.lock().expect("sleep inhibitor mutex poisoned") = Some(inhibitor);
        }
        Err(error) => {
            eprintln!("iuna desktop could not prevent system sleep: {error}");
        }
    }
}

fn release_system_awake_inhibitor() {
    let Some(store) = SLEEP_INHIBITOR.get() else {
        return;
    };
    let _ = store
        .0
        .lock()
        .expect("sleep inhibitor mutex poisoned")
        .take();
}

#[cfg(target_os = "macos")]
struct SleepInhibitor {
    assertion_id: u32,
}

#[cfg(target_os = "macos")]
impl SleepInhibitor {
    fn acquire() -> io::Result<Self> {
        macos_sleep::prevent_idle_system_sleep("iuna node is running")
            .map(|assertion_id| Self { assertion_id })
    }
}

#[cfg(target_os = "macos")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        let _ = macos_sleep::release_assertion(self.assertion_id);
    }
}

#[cfg(target_os = "macos")]
mod macos_sleep {
    use std::{
        ffi::{CString, c_char, c_void},
        io, ptr,
    };

    type CFStringRef = *const c_void;
    type IOReturn = i32;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    const K_IO_RETURN_SUCCESS: IOReturn = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut u32,
        ) -> IOReturn;
        fn IOPMAssertionRelease(assertion_id: u32) -> IOReturn;
    }

    pub fn prevent_idle_system_sleep(reason: &str) -> io::Result<u32> {
        let assertion_type = cf_string("PreventUserIdleSystemSleep")?;
        let assertion_name = cf_string(reason)?;
        let mut assertion_id = 0_u32;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type,
                K_IOPM_ASSERTION_LEVEL_ON,
                assertion_name,
                &mut assertion_id,
            )
        };
        unsafe {
            CFRelease(assertion_type);
            CFRelease(assertion_name);
        }
        if result == K_IO_RETURN_SUCCESS {
            Ok(assertion_id)
        } else {
            Err(io::Error::other(format!(
                "IOPMAssertionCreateWithName failed with status {result}"
            )))
        }
    }

    pub fn release_assertion(assertion_id: u32) -> io::Result<()> {
        let result = unsafe { IOPMAssertionRelease(assertion_id) };
        if result == K_IO_RETURN_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "IOPMAssertionRelease failed with status {result}"
            )))
        }
    }

    fn cf_string(value: &str) -> io::Result<CFStringRef> {
        let value = CString::new(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let cf_string = unsafe {
            CFStringCreateWithCString(ptr::null(), value.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        if cf_string.is_null() {
            Err(io::Error::other("CFStringCreateWithCString returned null"))
        } else {
            Ok(cf_string)
        }
    }
}

#[cfg(target_os = "windows")]
struct SleepInhibitor;

#[cfg(target_os = "windows")]
impl SleepInhibitor {
    fn acquire() -> io::Result<Self> {
        windows_sleep::prevent_idle_system_sleep()?;
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        let _ = windows_sleep::clear_sleep_requirement();
    }
}

#[cfg(target_os = "windows")]
mod windows_sleep {
    use std::io;

    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadExecutionState(es_flags: u32) -> u32;
    }

    pub fn prevent_idle_system_sleep() -> io::Result<()> {
        set_execution_state(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
    }

    pub fn clear_sleep_requirement() -> io::Result<()> {
        set_execution_state(ES_CONTINUOUS)
    }

    fn set_execution_state(flags: u32) -> io::Result<()> {
        let previous = unsafe { SetThreadExecutionState(flags) };
        if previous == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
struct SleepInhibitor {
    child: Option<std::process::Child>,
}

#[cfg(target_os = "linux")]
impl SleepInhibitor {
    fn acquire() -> io::Result<Self> {
        let parent_pid = std::process::id().to_string();
        let child = std::process::Command::new("systemd-inhibit")
            .args([
                "--what=sleep",
                "--mode=block",
                "--why=iuna node is running",
                "sh",
                "-c",
                "trap 'exit 0' TERM; while kill -0 \"$1\" 2>/dev/null; do sleep 30 & wait $!; done",
                "iuna-sleep-inhibitor",
                &parent_pid,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(Self { child: Some(child) })
    }
}

#[cfg(target_os = "linux")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
struct SleepInhibitor;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl SleepInhibitor {
    fn acquire() -> io::Result<Self> {
        Ok(Self)
    }
}
