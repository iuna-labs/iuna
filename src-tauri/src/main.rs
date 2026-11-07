use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    path::Path,
    sync::{Mutex, OnceLock},
};

use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_shell::{
    ShellExt,
    process::{CommandChild, CommandEvent},
};
use tauri_plugin_updater::{Update, UpdaterExt};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

struct IunaSidecar(Mutex<Option<CommandChild>>);
struct IunaSleepInhibitor(Mutex<Option<SleepInhibitor>>);
struct PendingDesktopUpdate(Mutex<Option<Update>>);
struct DesktopCredentials {
    operator_token: String,
}
/// The launch of the current sidecar. Events of a replaced sidecar carry an older generation.
struct NodeLaunchState(Mutex<(u64, NodeLaunch)>);

const KNOWN_NODES: &str = include_str!("../../KNOWN_NODES.txt");

/// A desktop node cannot find the network on its own, so it starts with the published bootstrap
/// nodes. They do not decide which blocks are valid, and the user can remove them for good.
fn bootstrap_peer_args(known_nodes: &str) -> Vec<&str> {
    known_nodes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .flat_map(|peer| ["--bootstrap-peer", peer])
        .collect()
}

/// The node prints this on stdout only after binding the management port. Seeing it from our own
/// child proves that child owns the port, so the launcher never trusts whatever else listens there.
const NODE_READY_LINE: &str = "operator API: http://127.0.0.1:8080/api/status";

#[derive(Debug, Eq, PartialEq)]
enum NodeLaunch {
    Starting { error: Option<String> },
    Ready,
    Failed(String),
}

impl NodeLaunch {
    fn observe(&mut self, event: &CommandEvent) {
        match event {
            CommandEvent::Stdout(line) if matches!(self, Self::Starting { .. }) => {
                if String::from_utf8_lossy(line).trim_end() == NODE_READY_LINE {
                    *self = Self::Ready;
                }
            }
            CommandEvent::Stderr(line) => {
                if let Self::Starting { error } = self
                    && let Some(message) = String::from_utf8_lossy(line)
                        .trim_end()
                        .strip_prefix("Error: ")
                {
                    *error = Some(message.to_owned());
                }
            }
            CommandEvent::Error(message) => *self = Self::Failed(message.clone()),
            CommandEvent::Terminated(_) => {
                let reason = match self {
                    Self::Starting { error: Some(error) } => error.clone(),
                    Self::Failed(reason) => reason.clone(),
                    _ => "the node process stopped".to_owned(),
                };
                *self = Self::Failed(reason);
            }
            _ => {}
        }
    }
}

static SIDECAR: OnceLock<IunaSidecar> = OnceLock::new();
static SLEEP_INHIBITOR: OnceLock<IunaSleepInhibitor> = OnceLock::new();

fn main() {
    let updater = tauri_plugin_updater::Builder::new()
        .pubkey(include_str!("../../config/update-signing.key.pub").trim());
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(updater.build())
        .manage(PendingDesktopUpdate(Mutex::new(None)))
        .manage(NodeLaunchState(Mutex::new((
            0,
            NodeLaunch::Starting { error: None },
        ))))
        .invoke_handler(tauri::generate_handler![
            check_desktop_update,
            install_desktop_update,
            desktop_operator_token
        ])
        .setup(|app| {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            setup_desktop_tray(app)?;

            let app_data = app.path().app_data_dir().map_err(setup_error)?;
            fs::create_dir_all(&app_data).map_err(setup_error)?;
            let operator_token = load_or_create_secret(&app_data.join("operator.token"))?;
            app.manage(DesktopCredentials { operator_token });
            start_node(app.handle()).map_err(io::Error::other)?;

            Ok(())
        })
        .on_window_event(|_window, event| {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Err(error) = _window.hide() {
                    eprintln!("iuna desktop could not hide its window: {error}");
                }
            }

            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                stop_sidecar();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building iuna desktop")
        .run(|_app, event| {
            #[cfg(target_os = "macos")]
            if matches!(
                &event,
                tauri::RunEvent::Reopen {
                    has_visible_windows: false,
                    ..
                }
            ) {
                show_main_window(_app);
            }

            if matches!(
                event,
                tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
            ) {
                stop_sidecar();
            }
        });
}

fn start_node(app: &AppHandle) -> Result<(), String> {
    let operator_token = app.state::<DesktopCredentials>().operator_token.clone();
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let generation = {
        let launch = app.state::<NodeLaunchState>();
        let mut launch = launch.0.lock().expect("node launch mutex poisoned");
        launch.0 += 1;
        launch.1 = NodeLaunch::Starting { error: None };
        launch.0
    };
    let (mut events, child) = app
        .shell()
        .sidecar("iuna-sidecar")
        .map_err(|error| error.to_string())?
        .current_dir(&app_data)
        .env("IUNA_OPERATOR_TOKEN", operator_token)
        .args(bootstrap_peer_args(KNOWN_NODES))
        .spawn()
        .map_err(|error| error.to_string())?;

    keep_system_awake_while_node_runs();

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.recv().await {
            let launch = handle.state::<NodeLaunchState>();
            let mut launch = launch.0.lock().expect("node launch mutex poisoned");
            if launch.0 == generation {
                launch.1.observe(&event);
            }
        }
    });

    let sidecar = SIDECAR.get_or_init(|| IunaSidecar(Mutex::new(None)));
    *sidecar.0.lock().expect("sidecar mutex poisoned") = Some(child);
    Ok(())
}

/// `None` while the node starts. The token is released only once our own child owns the port.
#[tauri::command]
fn desktop_operator_token(
    credentials: State<'_, DesktopCredentials>,
    launch: State<'_, NodeLaunchState>,
) -> Result<Option<String>, String> {
    match &launch.0.lock().expect("node launch mutex poisoned").1 {
        NodeLaunch::Starting { .. } => Ok(None),
        NodeLaunch::Ready => Ok(Some(credentials.operator_token.clone())),
        NodeLaunch::Failed(reason) => Err(reason.clone()),
    }
}

fn load_or_create_secret(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        // Left by a crash in a version that wrote the file in place. It never held a secret.
        Ok(secret) if secret.is_empty() => fs::remove_file(path)?,
        Ok(secret) => return validate_secret(secret),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| io::Error::other(format!("secure randomness unavailable: {error}")))?;
    let secret = hex(&bytes);
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(secret.as_bytes())?;
    file.sync_all()?;
    drop(file);
    // Linking is atomic and refuses to overwrite, so a crash leaves no partial secret and two
    // processes agree on one.
    let installed = fs::hard_link(&temporary, path);
    let _ = fs::remove_file(&temporary);
    match installed {
        Ok(()) => Ok(secret),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            validate_secret(fs::read_to_string(path)?)
        }
        Err(error) => Err(error),
    }
}

fn validate_secret(secret: String) -> io::Result<String> {
    if secret.len() == 64 && secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(secret)
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidData,
            "desktop credential file is invalid",
        ))
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[tauri::command]
async fn check_desktop_update(
    app: AppHandle,
    pending: State<'_, PendingDesktopUpdate>,
) -> Result<Option<String>, String> {
    let update = app
        .updater()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| error.to_string())?;
    let version = update.as_ref().map(|update| update.version.clone());
    *pending.0.lock().map_err(|_| "updater mutex poisoned")? = update;
    Ok(version)
}

#[tauri::command]
async fn install_desktop_update(
    app: AppHandle,
    pending: State<'_, PendingDesktopUpdate>,
) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .map_err(|_| "updater mutex poisoned")?
        .take()
        .ok_or_else(|| "no verified desktop update is ready".to_string())?;
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|error| error.to_string())?;

    stop_sidecar();
    if let Err(error) = update.install(bytes) {
        // The app keeps running on the old version, so it needs its node back.
        *pending.0.lock().map_err(|_| "updater mutex poisoned")? = Some(update);
        start_node(&app)?;
        return Err(error.to_string());
    }

    #[cfg(not(target_os = "windows"))]
    app.restart();
    #[cfg(target_os = "windows")]
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn setup_desktop_tray(app: &tauri::App) -> tauri::Result<()> {
    const OPEN_MENU_ID: &str = "open-iuna";
    const QUIT_MENU_ID: &str = "quit-iuna";

    let open = MenuItem::with_id(app, OPEN_MENU_ID, "Open iuna", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT_MENU_ID, "Quit iuna", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let mut tray = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("iuna — node running")
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN_MENU_ID => show_main_window(app),
            QUIT_MENU_ID => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if cfg!(target_os = "windows")
                && matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                )
            {
                show_main_window(tray.app_handle());
            }
        });

    tray = tray
        .icon(tauri::include_image!("./icons/tray-icon.png"))
        .icon_as_template(cfg!(target_os = "macos"));

    tray.build(app)?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn show_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        eprintln!("iuna desktop could not find its main window");
        return;
    };

    if let Err(error) = window
        .show()
        .and_then(|_| window.unminimize())
        .and_then(|_| window.set_focus())
    {
        eprintln!("iuna desktop could not show its window: {error}");
    }
}

fn stop_sidecar() {
    if let Some(sidecar) = SIDECAR.get()
        && let Some(child) = sidecar.0.lock().expect("sidecar mutex poisoned").take()
    {
        let _ = child.kill();
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

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn stdout(line: &str) -> CommandEvent {
        CommandEvent::Stdout(line.as_bytes().to_vec())
    }

    fn terminated() -> CommandEvent {
        CommandEvent::Terminated(tauri_plugin_shell::process::TerminatedPayload {
            code: Some(1),
            signal: None,
        })
    }

    #[test]
    fn node_is_ready_only_after_our_child_reports_the_bound_port() {
        let mut launch = NodeLaunch::Starting { error: None };
        launch.observe(&stdout("IUNA mainnet-candidate node"));
        assert_eq!(launch, NodeLaunch::Starting { error: None });
        launch.observe(&stdout(&format!("{NODE_READY_LINE}\n")));
        assert_eq!(launch, NodeLaunch::Ready);
    }

    #[test]
    fn occupied_port_fails_with_the_node_error_and_never_becomes_ready() {
        let mut launch = NodeLaunch::Starting { error: None };
        launch.observe(&CommandEvent::Stderr(
            b"Error: binding operator HTTP listener on 127.0.0.1:8080".to_vec(),
        ));
        launch.observe(&CommandEvent::Stderr(b"Caused by:".to_vec()));
        launch.observe(&terminated());
        assert_eq!(
            launch,
            NodeLaunch::Failed("binding operator HTTP listener on 127.0.0.1:8080".to_owned())
        );
        launch.observe(&stdout(NODE_READY_LINE));
        assert!(matches!(launch, NodeLaunch::Failed(_)));
    }

    #[test]
    fn an_empty_secret_file_from_a_crash_is_replaced() {
        let directory = std::env::temp_dir().join(format!(
            "iuna-secret-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("operator.token");
        fs::write(&path, "").unwrap();

        let secret = load_or_create_secret(&path).unwrap();

        assert_eq!(load_or_create_secret(&path).unwrap(), secret);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn known_nodes_become_peer_arguments_without_comments() {
        assert_eq!(
            bootstrap_peer_args("# comment\n\n  one.example:1  \ntwo.example:2\n"),
            [
                "--bootstrap-peer",
                "one.example:1",
                "--bootstrap-peer",
                "two.example:2"
            ]
        );
        assert!(bootstrap_peer_args(KNOWN_NODES).contains(&"iuna.jhx.app:19733"));
    }

    #[test]
    fn a_stopped_node_is_no_longer_ready() {
        let mut launch = NodeLaunch::Ready;
        launch.observe(&terminated());
        assert!(matches!(launch, NodeLaunch::Failed(_)));
    }

    #[test]
    fn tray_icon_contains_rgba8_pixels_for_its_declared_dimensions() {
        let icon = tauri::include_image!("./icons/tray-icon.png");
        let expected_bytes = icon.width() as usize * icon.height() as usize * 4;

        assert_eq!(icon.rgba().len(), expected_bytes);
        assert!(
            icon.rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0)
        );
    }

    #[test]
    fn desktop_secret_is_randomly_created_and_stably_reloaded() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "iuna-desktop-secret-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("secret");

        let created = load_or_create_secret(&path).unwrap();
        assert_eq!(created.len(), 64);
        assert!(created.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(load_or_create_secret(&path).unwrap(), created);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }

        fs::remove_dir_all(directory).unwrap();
    }
}
