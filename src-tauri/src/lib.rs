//! Personal Harness desktop app (Tauri). The window is the UI; the router and
//! local API live in `harness-core` and keep running in the menu bar when the
//! window is closed.

use std::sync::Arc;

use harness_core::{commands, Harness};
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime, State, WindowEvent};

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

fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let port = app.state::<Arc<Harness>>().settings().server.port;
    let show = MenuItem::with_id(app, "show", "Open Personal Harness", true, None::<&str>)?;
    let info = MenuItem::with_id(app, "info", format!("Local API: http://127.0.0.1:{port}/v1"), false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Personal Harness", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &info, &sep, &quit])?;
    let mut tray =
        TrayIconBuilder::with_id("main").menu(&menu).tooltip("Personal Harness").show_menu_on_left_click(true).on_menu_event(
            |app, event| match event.id().as_ref() {
                "show" => show_main(app),
                "quit" => app.exit(0),
                _ => {}
            },
        );
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .menu(Menu::default)
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Inside the macOS App Sandbox this resolves to
            // ~/Library/Containers/<bundle id>/Data/Library/Application Support/<bundle id>
            let dir = app.path().app_data_dir()?;
            let h = Harness::open(&dir)?;
            app.manage(h.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(e) = h.start_server().await {
                    eprintln!("local API failed to start: {e}");
                }
                h.spawn_background();
            });
            if let Err(e) = build_tray(app) {
                eprintln!("tray icon unavailable: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Keep running in the menu bar so the local API stays available.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
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
