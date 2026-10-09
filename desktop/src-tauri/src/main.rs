#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host_name;
mod locale;
mod paths;
mod updates;

use kvmflow_sidecar::{RuntimeHandle, RuntimeOptions};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;

struct Host {
    runtime: Arc<RuntimeHandle>,
    data_dir: std::path::PathBuf,
    page: Mutex<String>,
    status: MenuItem<tauri::Wry>,
    settings: MenuItem<tauri::Wry>,
    diagnostics: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    last_state: Mutex<Value>,
    pause: CheckMenuItem<tauri::Wry>,
    ready: AtomicBool,
    wants_visible: AtomicBool,
}

pub(crate) fn tr(app: &AppHandle, key: &str) -> String {
    app.try_state::<locale::Locale>()
        .map(|locale| locale.text(key))
        .unwrap_or_else(|| locale::translate("en", key))
}

#[tauri::command]
fn locale_set(app: AppHandle, preference: String, system_locale: String) -> Result<(), String> {
    app.state::<locale::Locale>()
        .save(&preference, &system_locale)?;
    let host = app.state::<Host>();
    host.settings
        .set_text(tr(&app, "settings"))
        .map_err(|error| error.to_string())?;
    host.diagnostics
        .set_text(tr(&app, "native.diagnostics"))
        .map_err(|error| error.to_string())?;
    host.pause
        .set_text(tr(&app, "native.pause"))
        .map_err(|error| error.to_string())?;
    host.quit
        .set_text(tr(&app, "native.quit"))
        .map_err(|error| error.to_string())?;
    let state = host.last_state.lock().unwrap().clone();
    update_status(&app, &state);
    Ok(())
}

fn open_client(app: &AppHandle, page: &str) {
    if let Some(host) = app.try_state::<Host>() {
        *host.page.lock().unwrap() = page.to_string();
        host.wants_visible.store(true, Ordering::Release);
        if !host.ready.load(Ordering::Acquire) {
            return;
        }
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("client-navigate", page);
    }
}

fn update_status(app: &AppHandle, data: &Value) {
    let Some(host) = app.try_state::<Host>() else {
        return;
    };
    *host.last_state.lock().unwrap() = data.clone();
    let enabled = data
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(data["state"] != "disabled");
    let text = match data["state"].as_str().unwrap_or("") {
        "error" => tr(app.app_handle(), "native.runtime.unavailable"),
        "learning" => tr(app.app_handle(), "identifying.usb.switch"),
        "pushing" => tr(app.app_handle(), "sending.switch.commands"),
        "disabled" => tr(app.app_handle(), "automatic.switching.paused"),
        _ if !enabled => tr(app.app_handle(), "automatic.switching.paused"),
        "idle" | "cooldown" | "armed" => tr(app.app_handle(), "native.connected"),
        _ if data["mode"] == "multi_device_armed" || data["configMode"] == "multi_device_armed" => {
            tr(app.app_handle(), "native.connected")
        }
        _ => tr(app.app_handle(), "native.unconfigured"),
    };
    let _ = host.status.set_text(&text);
    let _ = host.pause.set_checked(!enabled);
    if let Some(tray) = app.tray_by_id("kvmflow") {
        let _ = tray.set_tooltip(Some(format!("KVMFlow - {text}")));
    }
}

fn forward_notification(app: &AppHandle, kind: &str, data: Value) {
    if kind == "state" {
        update_status(app, &data);
    }
    if kind == "runtime.error" {
        update_status(app, &json!({ "state": "error" }));
    }
    let notice = match kind {
        "switch.report" => Some((
            tr(app.app_handle(), "native.switch.title"),
            tr(app.app_handle(), "native.switch.body"),
        )),
        "trigger" if data["action"] == "unexpected_return" => Some((
            tr(app.app_handle(), "native.return.title"),
            tr(app.app_handle(), "native.return.body"),
        )),
        "runtime.error" => Some((
            tr(app.app_handle(), "native.error.title"),
            tr(app.app_handle(), "native.error.body"),
        )),
        _ => None,
    };
    if let Some((title, body)) = notice {
        let _ = app.notification().builder().title(title).body(body).show();
    }
    let _ = app.emit(
        "runtime-notification",
        json!({ "kind": kind, "data": data }),
    );
}

#[tauri::command]
async fn runtime_request(
    app: AppHandle,
    method: String,
    params: Value,
) -> Result<Value, kvmflow_core::protocol::RpcErrorBody> {
    if app
        .state::<updates::Updates>()
        .installing
        .load(Ordering::Acquire)
    {
        return Err(kvmflow_core::protocol::RpcErrorBody {
            code: "E_BUSY".into(),
            message: tr(app.app_handle(), "native.update.busy").into(),
        });
    }
    if method == "shutdown" {
        return Err(kvmflow_core::protocol::RpcErrorBody {
            code: "E_UNKNOWN_METHOD".into(),
            message: tr(app.app_handle(), "native.exit.tray").into(),
        });
    }
    let runtime = app.state::<Host>().runtime.clone();
    let result = tauri::async_runtime::spawn_blocking(move || runtime.request(&method, params))
        .await
        .map_err(|_| kvmflow_core::protocol::RpcErrorBody {
            code: "E_BACKEND".into(),
            message: tr(app.app_handle(), "native.backend.failed").into(),
        })?;
    result.map_err(|mut error| {
        error.message = app.state::<locale::Locale>().error_text(&error.message);
        error
    })
}

#[tauri::command]
fn host_info(app: AppHandle) -> Value {
    json!({
        "version": app.package_info().version.to_string(),
        "platform": std::env::consts::OS,
        "computerName": host_name::computer_name(),
        "initialPage": *app.state::<Host>().page.lock().unwrap(),
        "languagePreference": app.state::<locale::Locale>().preference(),
    })
}

#[tauri::command]
fn client_ready(app: AppHandle) {
    let host = app.state::<Host>();
    host.ready.store(true, Ordering::Release);
    if host.wants_visible.load(Ordering::Acquire) {
        let page = host.page.lock().unwrap().clone();
        open_client(&app, &page);
    }
    eprintln!("KVMFlow client ready");
}

#[tauri::command]
fn startup_get(app: AppHandle) -> Result<bool, String> {
    app.autolaunch()
        .is_enabled()
        .map_err(|error| error.to_string())
}

fn set_startup(app: &AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let previous = manager.is_enabled().map_err(|error| error.to_string())?;
    if enabled {
        manager.enable()
    } else {
        manager.disable()
    }
    .map_err(|error| error.to_string())?;
    let path = app.state::<Host>().data_dir.join("startup-preference.json");
    if let Err(error) = paths::write_atomic(
        &path,
        &serde_json::to_vec_pretty(&json!({ "openAtLogin": enabled })).unwrap(),
    ) {
        let _ = if previous {
            manager.enable()
        } else {
            manager.disable()
        };
        return Err(error);
    }
    manager.is_enabled().map_err(|error| error.to_string())
}

#[tauri::command]
fn startup_set(app: AppHandle, enabled: bool) -> Result<bool, String> {
    set_startup(&app, enabled)
}

#[tauri::command]
async fn confirm_delete(app: AppHandle, name: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(tr(app.app_handle(), "native.delete.body").replace("{name}", &name))
            .title(tr(app.app_handle(), "native.delete.title"))
            .buttons(MessageDialogButtons::OkCancelCustom(
                tr(app.app_handle(), "delete").into(),
                tr(app.app_handle(), "cancel").into(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
async fn confirm_local_input(app: AppHandle, changes: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .message(tr(app.app_handle(), "native.local.body").replace("{changes}", &changes))
            .title(tr(
                app.app_handle(),
                "change.this.computer.s.monitor.input.values",
            ))
            .buttons(MessageDialogButtons::OkCancelCustom(
                tr(app.app_handle(), "confirm.change").into(),
                tr(app.app_handle(), "cancel").into(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
async fn export_diagnostics(app: AppHandle, payload: Value) -> Result<bool, String> {
    // A native save dialog is necessary: WebView downloads are not consistent
    // across WKWebView and WebView2. The frontend cannot choose arbitrary paths.
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .set_file_name("kvmflow-diagnostics.json")
            .add_filter("JSON", &["json"])
            .blocking_save_file()
        else {
            return Ok(false);
        };
        let path = file.into_path().map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&payload).map_err(|error| error.to_string())?;
        paths::write_atomic(&path, &bytes)?;
        Ok(true)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "--hidden") { open_client(app, "status"); }
        }))
        .plugin(tauri_plugin_autostart::Builder::new().app_name("KVMFlow").args(["--hidden"]).build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::Updates::default())
        .invoke_handler(tauri::generate_handler![runtime_request, host_info, locale_set, client_ready, startup_get, startup_set, export_diagnostics, confirm_delete, confirm_local_input, updates::update_check, updates::update_download, updates::update_install])
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = paths::data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            app.manage(locale::Locale::load(&data_dir));
            let hidden = std::env::args().any(|arg| arg == "--hidden");
            let page = if data_dir.join("config.json").is_file() { "status" } else { "wizard" };
            let status = MenuItem::with_id(app, "status", tr(app.app_handle(), "native.unconfigured"), false, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", tr(app.app_handle(), "settings"), true, None::<&str>)?;
            let diagnostics = MenuItem::with_id(app, "diagnostics", tr(app.app_handle(), "native.diagnostics"), true, None::<&str>)?;
            let pause = CheckMenuItem::with_id(app, "pause", tr(app.app_handle(), "native.pause"), true, false, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", tr(app.app_handle(), "native.quit"), true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&status, &settings, &diagnostics, &pause, &quit])?;
            let options = RuntimeOptions::new(data_dir.join("config.json"), data_dir.join("logs"));
            #[cfg(target_os = "macos")]
            let options = {
                let mut options = options;
                options.ddc_binary = std::env::var("KVMFLOW_M1DDC").ok().filter(|value| !value.is_empty());
                if options.ddc_binary.is_none() {
                    if let Ok(resources) = app.path().resource_dir() {
                        let binary = resources.join("m1ddc");
                        if binary.is_file() { options.ddc_binary = Some(binary.to_string_lossy().into_owned()); }
                    }
                }
                if options.ddc_binary.is_none() {
                    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/sidecar/m1ddc");
                    if binary.is_file() { options.ddc_binary = Some(binary.to_string_lossy().into_owned()); }
                }
                options
            };
            let notifications = handle.clone();
            let runtime = Arc::new(RuntimeHandle::start(options, move |kind, data| forward_notification(&notifications, kind, data))?);
            app.manage(Host { runtime: runtime.clone(), data_dir, page: Mutex::new(page.into()), status, settings, diagnostics, quit, pause, last_state: Mutex::new(json!({})),
                ready: AtomicBool::new(false), wants_visible: AtomicBool::new(!hidden) });
            let icon = if cfg!(target_os = "macos") {
                tauri::image::Image::from_bytes(include_bytes!("../../src/renderer/assets/trayTemplate.png"))?
            } else {
                app.default_window_icon().unwrap().clone()
            };
            TrayIconBuilder::with_id("kvmflow").icon(icon).icon_as_template(false)
                .menu(&menu).tooltip("KVMFlow")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "settings" => open_client(app, "settings"),
                    "diagnostics" => open_client(app, "diagnostics"),
                    "pause" => {
                        if app.state::<updates::Updates>().installing.load(Ordering::Acquire) {
                            let _ = app.state::<Host>().pause.set_checked(true);
                            return;
                        }
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let runtime = app.state::<Host>().runtime.clone();
                            let result = tauri::async_runtime::spawn_blocking(move || {
                                let state = runtime.request("state.get", json!({}))?;
                                runtime.request("app.setEnabled", json!({ "enabled": state["enabled"] != true }))
                            }).await;
                            match result {
                                Ok(Ok(data)) => update_status(&app, &data),
                                _ => {
                                    // Checkbox menus toggle before their callback, restore
                                    // the runtime's actual state if an operation fails.
                                    let runtime = app.state::<Host>().runtime.clone();
                                    if let Ok(Ok(data)) = tauri::async_runtime::spawn_blocking(move || runtime.request("state.get", json!({}))).await {
                                        update_status(&app, &data);
                                    }
                                    let _ = app.emit("runtime-notification", json!({ "kind": "runtime.error", "data": { "message": tr(app.app_handle(), "native.state.failed") } }));
                                }
                            }
                        });
                    }
                    "quit" => {
                        if !app.state::<updates::Updates>().installing.load(Ordering::Acquire) {
                            app.exit(0);
                        }
                    }
                    _ => {}
                }).build(app)?;
            tauri::async_runtime::spawn(async move {
                let result = tauri::async_runtime::spawn_blocking(move || runtime.request("state.get", json!({}))).await;
                match result {
                    Ok(Ok(data)) => update_status(&handle, &data),
                    _ => forward_notification(&handle, "runtime.error", json!({ "message": tr(&handle, "native.install.failed") })),
                }
            });
            // Never register the development executable at login. Release builds
            // preserve the Electron preference, including an explicit opt-out.
            if !cfg!(debug_assertions) && std::env::var_os("KVMFLOW_NO_AUTOSTART").is_none() {
                let preference = std::fs::read(app.state::<Host>().data_dir.join("startup-preference.json"))
                    .ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .and_then(|value| value["openAtLogin"].as_bool()).unwrap_or(true);
                if let Err(error) = set_startup(app.handle(), preference) {
                    eprintln!("Unable to restore startup preference: {error}");
                }
            }
            if !hidden { open_client(app.handle(), page); }
            #[cfg(target_os = "macos")]
            { app.set_activation_policy(if hidden { tauri::ActivationPolicy::Accessory } else { tauri::ActivationPolicy::Regular }); }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(host) = window.app_handle().try_state::<Host>() {
                    host.wants_visible.store(false, Ordering::Release);
                }
                let _ = window.hide();
                #[cfg(target_os = "macos")]
                let _ = window.app_handle().set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
        })
        .build(tauri::generate_context!())
        .expect("Unable to start KVMFlow");
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(host) = app.try_state::<Host>() {
                host.runtime.shutdown_and_join();
            }
        }
    });
}
