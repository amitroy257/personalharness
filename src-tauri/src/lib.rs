//! Personal Harness app (Tauri): macOS, Windows and Android. The window is the
//! UI; the router, local API and sync engine live in `harness-core`. On the
//! desktop the app keeps running in the menu bar (macOS) / system tray
//! (Windows) when the window is closed.

use std::sync::Arc;

use harness_core::{commands, Harness};
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{Manager, State};

#[tauri::command]
async fn call(state: State<'_, Arc<Harness>>, name: String, args: Value) -> Result<Value, String> {
    commands::call(state.inner(), &name, args).await
}

#[tauri::command]
async fn chat(state: State<'_, Arc<Harness>>, args: Value, on_event: Channel<Value>) -> Result<(), String> {
    let h = state.inner().clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tauri::async_runtime::spawn(async move { commands::send_message(&h, args, tx).await });
    while let Some(ev) = rx.recv().await {
        let _ = on_event.send(serde_json::to_value(&ev).unwrap_or(Value::Null));
    }
    task.await.map_err(|e| e.to_string())
}

/// Bring the main window back (tray, second launch, Dock click). Desktop only:
/// on phones the OS manages the app's single window.
#[cfg(desktop)]
fn show_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg(desktop)]
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let port = app.state::<Arc<Harness>>().settings().server.port;
    let show = MenuItem::with_id(app, "show", "Open Personal Harness", true, None::<&str>)?;
    let info = MenuItem::with_id(app, "info", format!("Local API: http://127.0.0.1:{port}/v1"), false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Personal Harness", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &info, &sep, &quit])?;
    // macOS convention: any click opens the menu. Windows convention: left
    // click opens the app, right click opens the menu.
    let mut tray = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("Personal Harness")
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if cfg!(target_os = "macos") {
                return;
            }
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // A second launch (e.g. Start menu while hidden in the tray) focuses the
    // running instance instead of fighting it for the API port.
    #[cfg(any(windows, target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)));
    // The standard app menu (Edit → Copy/Paste shortcuts etc.) belongs to macOS;
    // on Windows it would add a menu bar to the window.
    #[cfg(target_os = "macos")]
    let builder = builder.menu(tauri::menu::Menu::default);
    #[cfg(desktop)]
    let builder = builder.on_window_event(|window, event| {
        // Keep running in the menu bar / tray so the local API stays available.
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = window.hide();
        }
    });
    let app = builder
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // macOS (sandboxed): ~/Library/Containers/<id>/Data/Library/Application Support/<id>
            // Windows: %LOCALAPPDATA%\<id> (local, never roamed to other machines)
            // Android: the app's private storage (/data/user/0/<id>/...)
            let dir = app.path().app_local_data_dir()?;
            let first_run = !dir.join("settings.json").exists();
            let h = Harness::open(&dir)?;
            h.set_desktop(true);
            if cfg!(mobile) && first_run {
                // Other phone apps rarely need an OpenAI-compatible endpoint;
                // keep the port closed unless the user turns it on.
                h.update_settings(|s| s.server.enabled = false)?;
            }
            app.manage(h.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(e) = h.start_server().await {
                    eprintln!("local API failed to start: {e}");
                }
                h.spawn_background();
            });
            #[cfg(desktop)]
            if let Err(e) = build_tray(app) {
                eprintln!("tray icon unavailable: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call, chat])
        .build(tauri::generate_context!())
        .expect("error while building Personal Harness");

    app.run(|handle, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_main(handle);
        }
        let _ = (handle, event);
    });
}
