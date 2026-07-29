use std::{
    error::Error,
    io,
    sync::{Mutex, OnceLock},
};

use tauri::WindowEvent;
use tauri_plugin_shell::{ShellExt, process::CommandChild};

struct IunaSidecar(Mutex<Option<CommandChild>>);

static SIDECAR: OnceLock<IunaSidecar> = OnceLock::new();

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
    let Some(sidecar) = SIDECAR.get() else {
        return;
    };
    if let Some(child) = sidecar.0.lock().expect("sidecar mutex poisoned").take() {
        let _ = child.kill();
    }
}

fn setup_error(error: impl Error + Send + Sync + 'static) -> Box<dyn Error> {
    Box::new(io::Error::other(error))
}
