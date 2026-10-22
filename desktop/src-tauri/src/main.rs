#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host_name;
mod locale;
mod mixed_menu;
mod paths;
mod recovery;
mod runtime_state;
mod updates;

use kvmflow_sidecar::{RuntimeHandle, RuntimeOptions};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;

struct Host {
    runtime: recovery::RuntimeSlot,
    pairing_gate: Arc<Mutex<()>>,
    pairing_closing: AtomicBool,
    data_dir: std::path::PathBuf,
    page: Mutex<String>,
    status: MenuItem<tauri::Wry>,
    settings: MenuItem<tauri::Wry>,
    diagnostics: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    last_state: Mutex<runtime_state::Snapshot>,
    tray_config: Mutex<Value>,
    mixed: CheckMenuItem<tauri::Wry>,
    mixed_assignment: MenuItem<tauri::Wry>,
    mixed_configure: MenuItem<tauri::Wry>,
    mixed_notice_mode: Mutex<Option<bool>>,
    mixed_busy: AtomicBool,
    pause: CheckMenuItem<tauri::Wry>,
    ready: AtomicBool,
    wants_visible: AtomicBool,
    exit_pending: AtomicBool,
    exit_ready: AtomicBool,
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
    refresh_tray(&app);
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
    {
        let mut state = host.last_state.lock().unwrap();
        if !runtime_state::merge_snapshot(&mut state, data) {
            return;
        }
        if state.value()["switch_group"].is_object() {
            host.tray_config.lock().unwrap()["switch_group"] =
                state.value()["switch_group"].clone();
        }
    }
    notify_mixed_mode(app);
    refresh_tray(app);
}

fn refresh_tray(app: &AppHandle) {
    let handle = app.clone();
    // Native APIs may synchronously wait for the main thread. Read snapshots
    // inside the queued task and release every mutex before touching any menu.
    // Older queued tasks also render the latest snapshot, avoiding stale ticks.
    let _ = app.run_on_main_thread(move || {
        let Some(host) = handle.try_state::<Host>() else {
            return;
        };
        let data = host.last_state.lock().unwrap().value().clone();
        let config = host.tray_config.lock().unwrap().clone();
        let mixed = &data["mixed"];
        let translate = |key: &str| tr(&handle, key);
        let enabled = data["enabled"]
            .as_bool()
            .unwrap_or(data["state"] != "disabled");
        let text = mixed_menu::current(&config, mixed, &translate).unwrap_or_else(|| {
            tr(
                &handle,
                match data["state"].as_str().unwrap_or("") {
                    "error" => "native.runtime.unavailable",
                    "learning" => "identifying.usb.switch",
                    "pushing" => "sending.switch.commands",
                    _ if !enabled => "automatic.switching.paused",
                    "idle" | "cooldown" | "armed" => "native.connected",
                    _ => "native.unconfigured",
                },
            )
        });
        let _ = host.status.set_text(&text);
        let _ = host.pause.set_checked(!enabled);
        let _ = host.pause.set_text(tr(&handle, "native.pause"));
        let _ = host.settings.set_text(tr(&handle, "native.open"));
        let _ = host.diagnostics.set_text(tr(&handle, "native.diagnostics"));
        let _ = host.quit.set_text(tr(&handle, "native.quit"));
        let _ = host.mixed.set_text(tr(&handle, "mixed.mode"));
        let _ = host.mixed.set_checked(mixed["enabled"] == true);
        let allowed = mixed_menu::interaction_allowed(
            mixed,
            handle
                .state::<updates::Updates>()
                .installing
                .load(Ordering::Acquire),
            host.mixed_busy.load(Ordering::Acquire),
        );
        let _ = host.mixed.set_enabled(allowed);
        let _ = host
            .mixed_configure
            .set_text(tr(&handle, "native.mixed.configure"));
        if let Some(tray) = handle.tray_by_id("kvmflow") {
            let _ = tray.set_tooltip(Some(format!("KVMFlow - {text}")));
            let result = (|| -> tauri::Result<()> {
                let menu = Menu::new(&handle)?;
                menu.append(&host.status)?;
                if let Some(text) = mixed_menu::assignment(&config, mixed, &translate) {
                    host.mixed_assignment.set_text(text)?;
                    menu.append(&host.mixed_assignment)?;
                }
                menu.append(&host.mixed)?;
                // Disabled native menu items cannot receive click events. Keep
                // a live settings entry next to the disabled checkbox.
                if !allowed {
                    menu.append(&host.mixed_configure)?;
                }
                menu.append(&PredefinedMenuItem::separator(&handle)?)?;
                menu.append(&host.settings)?;
                menu.append(&host.diagnostics)?;
                menu.append(&host.pause)?;
                menu.append(&PredefinedMenuItem::separator(&handle)?)?;
                menu.append(&host.quit)?;
                tray.set_menu(Some(menu))?;
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Unable to refresh tray: {error}");
            }
        }
    });
}

fn notify_mixed_mode(app: &AppHandle) {
    let handle = app.clone();
    // Like the tray refresh, notification tasks read the newest accepted
    // snapshot when they run. Never retain an older response across dispatch.
    let _ = app.run_on_main_thread(move || {
        let Some(host) = handle.try_state::<Host>() else {
            return;
        };
        let state = host.last_state.lock().unwrap().value().clone();
        let changed = {
            let mut baseline = host.mixed_notice_mode.lock().unwrap();
            mixed_menu::should_notify(&mut baseline, &state["mixed"])
        };
        if changed {
            let config = host.tray_config.lock().unwrap().clone();
            let _ = handle
                .notification()
                .builder()
                .title(tr(&handle, "native.mixed.title"))
                .body(mixed_menu::summary(&config, &state, &|key| {
                    tr(&handle, key)
                }))
                .show();
        }
    });
}

fn update_runtime_status(app: &AppHandle, generation: u64, data: Value) {
    if app
        .try_state::<Host>()
        .is_some_and(|host| host.runtime.is_current(generation))
    {
        update_status(app, &recovery::stamp_state(generation, data));
    }
}

fn forward_notification(app: &AppHandle, generation: u64, kind: &str, data: Value) {
    // Keep the originating worker stamp through every asynchronous handoff.
    if app
        .try_state::<Host>()
        .is_some_and(|host| !host.runtime.is_current(generation))
    {
        return;
    }
    let notification = recovery::notification(generation, kind, data);
    let data = notification["data"].clone();
    if matches!(kind, "state" | "mixed.changed") {
        update_runtime_status(app, generation, data.clone());
    }
    if kind == "runtime.error" {
        if let Some(host) = app.try_state::<Host>() {
            let accepted = host.last_state.lock().unwrap().mark_error(&data);
            if accepted {
                refresh_tray(app);
            }
        }
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
    let _ = app.emit("runtime-notification", notification);
}

fn opens_pairing_window(method: &str, params: &Value) -> bool {
    method == "lan.pair.start" || method == "lan.setup.secondary"
        || (method == "lan.pair.window" && params.get("open").and_then(Value::as_bool).unwrap_or(true))
}

fn close_pairing_screen<E>(mut request: impl FnMut(&str, Value) -> Result<Value, E>) {
    let _ = request("lan.pair.window", json!({"open": false}));
    let _ = request("lan.pair.cancel", json!({}));
    let _ = request("lan.test.stop", json!({}));
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
    let (runtime, generation) = app.state::<Host>().runtime.snapshot();
    let refresh_config = method == "config.set";
    let state_reply = matches!(
        method.as_str(),
        "state.get" | "app.setEnabled" | "mixed.set" | "mixed.assign"
    );
    let worker_runtime = runtime.clone();
    let pairing_gate = app.state::<Host>().pairing_gate.clone();
    let worker_app = app.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || {
            let pairing = method.starts_with("lan.pair.") || method == "lan.setup.secondary";
            let _gate = pairing.then(|| pairing_gate.lock().unwrap());
            let opens = opens_pairing_window(&method, &params);
            if opens && (!worker_app.state::<Host>().wants_visible.load(Ordering::Acquire)
                || worker_app.state::<Host>().pairing_closing.load(Ordering::Acquire)) {
                return Err(kvmflow_core::protocol::RpcErrorBody {
                    code: "E_LAN_PAIR_WINDOW".into(),
                    message: "The pairing screen is hidden".into(),
                });
            }
            worker_runtime.request(&method, params)
        })
            .await
            .map_err(|_| kvmflow_core::protocol::RpcErrorBody {
                code: "E_BACKEND".into(),
                message: tr(app.app_handle(), "native.backend.failed").into(),
            })?;
    if result.is_ok() && refresh_config {
        if let Ok(Ok(config)) =
            tauri::async_runtime::spawn_blocking(move || runtime.request("config.get", json!({})))
                .await
        {
            if app.state::<Host>().runtime.is_current(generation) {
                *app.state::<Host>().tray_config.lock().unwrap() = config;
                refresh_tray(&app);
            }
        }
    }
    result
        .map(|data| {
            if state_reply {
                recovery::stamp_state(generation, data)
            } else {
                data
            }
        })
        .map_err(|mut error| {
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
        "receiveBeta": app.state::<locale::Locale>().receive_beta(),
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

fn start_runtime(
    app: &AppHandle,
    data_dir: &std::path::Path,
    generation: u64,
) -> Result<Arc<RuntimeHandle>, String> {
    let options = RuntimeOptions::new(data_dir.join("config.json"), data_dir.join("logs"));
    #[cfg(target_os = "macos")]
    let options = {
        let mut options = options;
        options.ddc_binary = std::env::var("KVMFLOW_M1DDC")
            .ok()
            .filter(|value| !value.is_empty());
        if options.ddc_binary.is_none() {
            if let Ok(resources) = app.path().resource_dir() {
                let binary = resources.join("m1ddc");
                if binary.is_file() {
                    options.ddc_binary = Some(binary.to_string_lossy().into_owned());
                }
            }
        }
        if options.ddc_binary.is_none() {
            let binary =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/sidecar/m1ddc");
            if binary.is_file() {
                options.ddc_binary = Some(binary.to_string_lossy().into_owned());
            }
        }
        options
    };
    let notifications = app.clone();
    RuntimeHandle::start(options, move |kind, data| {
        forward_notification(&notifications, generation, kind, data)
    })
    .map(Arc::new)
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
        .invoke_handler(tauri::generate_handler![runtime_request, host_info, locale_set, client_ready, startup_get, startup_set, export_diagnostics, confirm_delete, confirm_local_input, updates::update_check, updates::update_set_beta, updates::update_download, updates::update_install, updates::update_open_download])
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
            let mixed = CheckMenuItem::with_id(app, "mixed", tr(app.app_handle(), "mixed.mode"), false, false, None::<&str>)?;
            let mixed_assignment = MenuItem::with_id(app, "mixed-assignment", "", false, None::<&str>)?;
            let mixed_configure = MenuItem::with_id(app, "mixed-configure", tr(app.app_handle(), "native.mixed.configure"), true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&status, &mixed, &mixed_configure, &settings, &diagnostics, &pause, &quit])?;
            let generation = recovery::INITIAL_GENERATION;
            let runtime = start_runtime(&handle, &data_dir, generation)?;
            let restart_handle = handle.clone();
            let restart_dir = data_dir.clone();
            let slot = recovery::RuntimeSlot::new(runtime.clone(), move |generation| start_runtime(&restart_handle, &restart_dir, generation));
            app.manage(Host { runtime: slot, pairing_gate: Arc::new(Mutex::new(())), pairing_closing: AtomicBool::new(false), data_dir, page: Mutex::new(page.into()), status, settings, diagnostics, quit, pause, last_state: Mutex::new(runtime_state::Snapshot::default()),
                tray_config: Mutex::new(json!({})), mixed, mixed_assignment, mixed_configure, mixed_notice_mode: Mutex::new(None), mixed_busy: AtomicBool::new(false),
                ready: AtomicBool::new(false), wants_visible: AtomicBool::new(!hidden),
                exit_pending: AtomicBool::new(false), exit_ready: AtomicBool::new(false) });
            let icon = if cfg!(target_os = "macos") {
                tauri::image::Image::from_bytes(include_bytes!("../../src/renderer/assets/trayTemplate.png"))?
            } else {
                app.default_window_icon().unwrap().clone()
            };
            TrayIconBuilder::with_id("kvmflow").icon(icon).icon_as_template(false)
                .menu(&menu).tooltip("KVMFlow")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "settings" => open_client(app, "status"),
                    "mixed-configure" => open_client(app, "settings#mixed"),
                    "mixed" => {
                        if app.state::<updates::Updates>().installing.load(Ordering::Acquire) { refresh_tray(app); return; }
                        let host = app.state::<Host>();
                        if host.mixed_busy.swap(true, Ordering::AcqRel) { refresh_tray(app); return; }
                        refresh_tray(app);
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let (runtime, generation) = app.state::<Host>().runtime.snapshot();
                            let worker_app = app.clone();
                            let result = tauri::async_runtime::spawn_blocking(move || {
                                mixed_menu::perform_toggle(&worker_app.state::<updates::Updates>().installing,
                                    |method,params| runtime.request(method,params))
                            }).await;
                            app.state::<Host>().mixed_busy.store(false, Ordering::Release);
                            match result {
                                Ok(Ok(Some(data))) => update_runtime_status(&app, generation, data),
                                Ok(Ok(None)) => refresh_tray(&app),
                                _ => {
                                    refresh_tray(&app);
                                    open_client(&app, "settings#mixed");
                                    let _ = app.notification().builder().title(tr(&app, "native.mixed.title"))
                                        .body(tr(&app, "native.mixed.failed")).show();
                                }
                            }
                        });
                    },
                    "diagnostics" => open_client(app, "diagnostics"),
                    "pause" => {
                        if app.state::<updates::Updates>().installing.load(Ordering::Acquire) {
                            let _ = app.state::<Host>().pause.set_checked(true);
                            return;
                        }
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let (runtime, generation) = app.state::<Host>().runtime.snapshot();
                            let result = tauri::async_runtime::spawn_blocking(move || {
                                let state = runtime.request("state.get", json!({}))?;
                                runtime.request("app.setEnabled", json!({ "enabled": state["enabled"] != true }))
                            }).await;
                            match result {
                                Ok(Ok(data)) => update_runtime_status(&app, generation, data),
                                _ => {
                                    // Checkbox menus toggle before their callback, restore
                                    // the runtime's actual state if an operation fails.
                                    let (runtime, fallback_generation) = app.state::<Host>().runtime.snapshot();
                                    if let Ok(Ok(data)) = tauri::async_runtime::spawn_blocking(move || runtime.request("state.get", json!({}))).await {
                                        update_runtime_status(&app, fallback_generation, data);
                                    }
                                    if app.state::<Host>().runtime.is_current(generation) {
                                        let _ = app.emit("runtime-notification", recovery::notification(generation, "runtime.error", json!({ "message": tr(app.app_handle(), "native.state.failed") })));
                                    }
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
                let config_runtime = runtime.clone();
                if let Ok(Ok(config)) = tauri::async_runtime::spawn_blocking(move || config_runtime.request("config.get", json!({}))).await {
                    *handle.state::<Host>().tray_config.lock().unwrap() = config;
                }
                let result = tauri::async_runtime::spawn_blocking(move || runtime.request("state.get", json!({}))).await;
                match result {
                    Ok(Ok(data)) => update_runtime_status(&handle, generation, data),
                    _ => forward_notification(&handle, generation, "runtime.error", json!({ "message": tr(&handle, "native.install.failed") })),
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
                    let runtime = host.runtime.current();
                    let gate = host.pairing_gate.clone();
                    host.pairing_closing.store(true, Ordering::Release);
                    let app = window.app_handle().clone();
                    // Do not rely on an unfocused/hidden WebView processing
                    // JS. Serialize against in-flight opens, so none can
                    // reopen admission after this native close.
                    tauri::async_runtime::spawn_blocking(move || {
                        let _guard = gate.lock().unwrap();
                        close_pairing_screen(|method, params| runtime.request(method, params));
                        app.state::<Host>().pairing_closing.store(false, Ordering::Release);
                    });
                }
                let _ = window.emit("client-hidden", json!({}));
                let _ = window.hide();
                #[cfg(target_os = "macos")]
                let _ = window.app_handle().set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
        })
        .build(tauri::generate_context!())
        .expect("Unable to start KVMFlow");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let Some(host) = app.try_state::<Host>() else {
                return;
            };
            if host.exit_ready.load(Ordering::Acquire) {
                return;
            }
            api.prevent_exit();
            if host.exit_pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let runtime = host.runtime.current();
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let result = tauri::async_runtime::spawn_blocking(move || {
                    recovery::attempt_exit(&runtime, std::time::Duration::from_secs(45))
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
                if let Err(error) = result {
                    // Queue resume even if release is still in flight. If the
                    // worker already stopped, restart over the same records.
                    let resume_handle = handle.clone();
                    let resumed = tauri::async_runtime::spawn_blocking(move || {
                        resume_handle
                            .state::<Host>()
                            .runtime
                            .resume_after_cancel(std::time::Duration::from_secs(5))
                    })
                    .await;
                    eprintln!("Exit recovery failed: {error}; cancellation: {resumed:?}");
                    let retry = tr(&handle, "native.recovery.retry");
                    let force = tr(&handle, "native.recovery.exitAnyway");
                    let stay = tr(&handle, "native.recovery.keepRunning");
                    let message = format!("{}\n{error}", tr(&handle, "native.recovery.exitRisk"));
                    let callback_handle = handle.clone();
                    let retry_label = retry.clone();
                    let force_label = force.clone();
                    handle
                        .dialog()
                        .message(message)
                        .title("KVMFlow")
                        .buttons(MessageDialogButtons::YesNoCancelCustom(retry, force, stay))
                        .show_with_result(move |result| {
                            let host = callback_handle.state::<Host>();
                            match recovery::exit_choice(result, &retry_label, &force_label) {
                                recovery::ExitChoice::Force => {
                                    host.runtime.current().abandon_for_exit();
                                    host.exit_ready.store(true, Ordering::Release);
                                    callback_handle.exit(0);
                                }
                                recovery::ExitChoice::Retry => {
                                    host.exit_pending.store(false, Ordering::Release);
                                    callback_handle.exit(0);
                                }
                                recovery::ExitChoice::Continue => {
                                    host.exit_pending.store(false, Ordering::Release);
                                }
                            }
                        });
                } else {
                    handle
                        .state::<Host>()
                        .exit_ready
                        .store(true, Ordering::Release);
                    handle.exit(0);
                }
            });
        }
    });
}

#[cfg(test)]
mod pairing_lifecycle_tests {
    use super::*;

    #[test]
    fn hidden_window_rejects_all_admission_open_methods() {
        for method in ["lan.pair.start", "lan.setup.secondary", "lan.pair.window"] {
            assert!(opens_pairing_window(method, &json!({})));
        }
        assert!(opens_pairing_window("lan.pair.window", &json!({"open": true})));
        assert!(!opens_pairing_window("lan.pair.window", &json!({"open": false})));
        assert!(!opens_pairing_window("lan.pair.cancel", &json!({})));
    }

    #[test]
    fn native_cleanup_continues_after_close_transport_error() {
        let mut calls = Vec::new();
        close_pairing_screen(|method, params| {
            calls.push((method.to_string(), params));
            Err::<Value, _>("closed")
        });
        assert_eq!(calls.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["lan.pair.window", "lan.pair.cancel", "lan.test.stop"]);
        assert_eq!(calls[0].1, json!({"open": false}));
    }
}
