use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Updates {
    pending: Mutex<Pending>,
    pub installing: AtomicBool,
}

#[derive(Default)]
struct Pending {
    update: Option<Update>,
    bytes: Option<Vec<u8>>,
}

const BETA_ENDPOINT: &str =
    "https://github.com/Kerw1n1209/KVMFlow/releases/download/beta-channel/latest.json";

fn beta_endpoints(config: Value) -> Result<Vec<tauri::Url>, serde_json::Error> {
    let config: tauri_plugin_updater::Config = serde_json::from_value(config)?;
    let mut endpoints = config.endpoints;
    endpoints.insert(0, BETA_ENDPOINT.parse().expect("valid beta endpoint"));
    Ok(endpoints)
}

#[tauri::command]
pub async fn update_set_beta(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let state = app.state::<Updates>();
    // Serialize with checks, downloads and installation. A failed save keeps
    // the existing preference and pending download intact.
    let mut pending = state
        .pending
        .try_lock()
        .map_err(|_| crate::tr(&app, "native.update.inProgress"))?;
    app.state::<crate::locale::Locale>()
        .save_receive_beta(enabled)?;
    *pending = Pending::default();
    Ok(enabled)
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Value, String> {
    let state = app.state::<Updates>();
    let mut pending = state
        .pending
        .try_lock()
        .map_err(|_| crate::tr(&app, "native.update.inProgress"))?;
    // A periodic check must not discard an already verified download.
    if let Some(update) = pending.update.as_ref() {
        return Ok(
            json!({ "version": update.version, "notes": update.body, "downloaded": pending.bytes.is_some() }),
        );
    }
    let mut builder = app.updater_builder();
    if app.state::<crate::locale::Locale>().receive_beta() {
        let config = app
            .config()
            .plugins
            .0
            .get("updater")
            .cloned()
            .unwrap_or_default();
        let endpoints =
            beta_endpoints(config).map_err(|_| crate::tr(&app, "native.update.initialize"))?;
        builder = builder
            .endpoints(endpoints)
            .map_err(|_| crate::tr(&app, "native.update.initialize"))?;
    }
    let mut update = builder
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| crate::tr(&app, "native.update.initialize"))?
        .check()
        .await
        .map_err(|_| {
            crate::tr(
                &app,
                "unable.to.check.for.updates.check.your.network.and.try.again",
            )
        })?;
    if let Some(update) = update.as_mut() {
        update.timeout = Some(Duration::from_secs(300));
    }
    let result = update
        .as_ref()
        .map(|update| {
            json!({
                "version": update.version, "notes": update.body, "downloaded": false
            })
        })
        .unwrap_or(Value::Null);
    pending.update = update;
    Ok(result)
}

#[tauri::command]
pub async fn update_download(app: AppHandle) -> Result<(), String> {
    let state = app.state::<Updates>();
    let mut pending = state
        .pending
        .try_lock()
        .map_err(|_| crate::tr(&app, "native.update.inProgress"))?;
    if pending.bytes.is_some() {
        return Ok(());
    }
    let update = pending
        .update
        .as_ref()
        .ok_or(crate::tr(&app, "native.update.checkFirst"))?;
    let mut downloaded = 0u64;
    let mut last_percent = None;
    let mut last_event = std::time::Instant::now();
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let percent = total
                    .filter(|size| *size > 0)
                    .map(|size| (downloaded.saturating_mul(100) / size).min(100));
                if percent != last_percent || last_event.elapsed() >= Duration::from_millis(250) {
                    let _ = app.emit(
                        "update-progress",
                        json!({ "downloaded": downloaded, "total": total, "percent": percent }),
                    );
                    last_percent = percent;
                    last_event = std::time::Instant::now();
                }
            },
            || {},
        )
        .await
        .map_err(|_| crate::tr(&app, "native.update.verify"))?;
    // download() verifies the signature before these bytes can be installed.
    pending.bytes = Some(bytes);
    Ok(())
}

#[tauri::command]
pub async fn update_install(app: AppHandle, allow_unrestored: Option<bool>) -> Result<(), String> {
    let state = app.state::<Updates>();
    let pending = state
        .pending
        .try_lock()
        .map_err(|_| crate::tr(&app, "native.update.inProgress"))?;
    let update = pending
        .update
        .as_ref()
        .ok_or(crate::tr(&app, "native.update.checkFirst"))?
        .clone();
    let bytes = pending
        .bytes
        .as_ref()
        .ok_or(crate::tr(&app, "native.update.downloadFirst"))?
        .clone();
    let allow_unrestored = allow_unrestored == Some(true);
    if allow_unrestored {
        let handle = app.clone();
        let confirmed = tauri::async_runtime::spawn_blocking(move || {
            handle
                .dialog()
                .message(crate::tr(&handle, "native.update.unrestoredRisk"))
                .title("KVMFlow")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    crate::tr(&handle, "native.update.installAnyway"),
                    crate::tr(&handle, "native.recovery.cancel"),
                ))
                .blocking_show()
        })
        .await
        .map_err(|_| crate::tr(&app, "native.update.failed"))?;
        if !confirmed {
            return Err(format!(
                "E_UPDATE_CANCELLED: {}",
                crate::tr(&app, "update.recovery.cancelled")
            ));
        }
    }
    state.installing.store(true, Ordering::Release);
    crate::refresh_tray(&app);
    let runtime = app.state::<crate::Host>().runtime.current();
    let data_dir = app.state::<crate::Host>().data_dir.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let install = || update.install(&bytes).map_err(|error| error.to_string());
        if allow_unrestored {
            install_unrestored(
                &runtime,
                true,
                || preserve_recovery_records(&data_dir),
                install,
            )?;
        } else {
            install_with_runtime(&runtime, install)?;
            runtime
                .shutdown_and_join()
                .map_err(|_| "native.update.restoreDisplays")?;
        }
        Ok(())
    })
    .await;
    state.installing.store(false, Ordering::Release);
    crate::refresh_tray(&app);
    // A failed risky installer may have stopped the old worker. Reuse the
    // same cancellation recovery path; the manual download remains available.
    if allow_unrestored && !matches!(&result, Ok(Ok(()))) {
        let handle = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            handle
                .state::<crate::Host>()
                .runtime
                .resume_after_cancel(Duration::from_secs(5))
        })
        .await;
    }
    result
        .map_err(|_| crate::tr(&app, "native.update.failed"))?
        .map_err(|key| {
            let message = if key == "native.update.restoreDisplays" {
                app.state::<crate::locale::Locale>()
                    .display_restore_error(true)
                    .into()
            } else {
                crate::tr(&app, key)
            };
            if matches!(
                key,
                "native.update.restoreDisplays"
                    | "native.update.recordBackupFailed"
                    | "update.failed.and.automatic.switching.was.not.restored.restart.the.app.a"
            ) {
                format!("E_UPDATE_RESTORE: {message}")
            } else {
                message
            }
        })?;
    app.state::<crate::Host>()
        .exit_ready
        .store(true, Ordering::Release);
    app.restart();
}

/// Existing project release page (README.md), not a new update service.
const RELEASES_URL: &str = "https://github.com/Kerw1n1209/KVMFlow/releases";
#[tauri::command]
pub fn update_open_download() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("/usr/bin/open")
        .arg(RELEASES_URL)
        .spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", RELEASES_URL])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("Open the GitHub Releases download link in your browser".into());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    result.map(|_| ()).map_err(|error| error.to_string())
}

fn preserve_recovery_records(data_dir: &std::path::Path) -> Result<(), &'static str> {
    let record = data_dir.join("display-detach.json");
    let bytes = match std::fs::read(record) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("native.update.recordBackupFailed"),
    };
    // Atomic write + sync_all; leave the canonical file in place for the new
    // version's startup recovery, and preserve a separate diagnostic copy.
    crate::paths::write_atomic(&data_dir.join("display-detach.before-update.json"), &bytes)
        .map_err(|_| "native.update.recordBackupFailed")
}

fn install_unrestored(
    runtime: &kvmflow_sidecar::RuntimeHandle,
    confirmed: bool,
    preserve: impl FnOnce() -> Result<(), &'static str>,
    install: impl FnOnce() -> Result<(), String>,
) -> Result<(), &'static str> {
    if !confirmed {
        return Err("update.recovery.cancelled");
    }
    preserve()?;
    runtime.abandon_for_exit();
    install().map_err(|error| {
        eprintln!("Explicitly confirmed update failed: {error}");
        "native.update.install"
    })
}

fn install_with_runtime(
    runtime: &kvmflow_sidecar::RuntimeHandle,
    install: impl FnOnce() -> Result<(), String>,
) -> Result<(), &'static str> {
    install_with_runtime_timeout(runtime, Duration::from_secs(45), install)
}

fn install_with_runtime_timeout(
    runtime: &kvmflow_sidecar::RuntimeHandle,
    timeout: Duration,
    install: impl FnOnce() -> Result<(), String>,
) -> Result<(), &'static str> {
    // Requests run after any in-flight DDC work on the serial worker.
    // Pausing is transient; it does not change the saved configuration.
    let before = runtime
        .request("state.get", json!({}))
        .map_err(|_| "native.update.readState")?;
    runtime
        .request("app.setEnabled", json!({ "enabled": false }))
        .map_err(|_| "native.update.pause")?;
    // Windows updater 2.13.2 uses process::exit(0), bypassing Tauri's
    // exit handler. macOS install returns, then app.restart() also exits.
    // Both paths must finish recovery before entering the installer.
    if let Err(error) = runtime.release_displays_timeout(timeout) {
        eprintln!("Update cancelled: display restoration failed: {error}");
        let _ = runtime.resume_displays_with_enabled(before["enabled"] == true);
        return Err("native.update.restoreDisplays");
    }
    if let Err(error) = install() {
        let restored = runtime.resume_displays_with_enabled(before["enabled"] == true);
        eprintln!("Update installation failed: {error}");
        return Err(if restored.is_err() {
            "update.failed.and.automatic.switching.was.not.restored.restart.the.app.a"
        } else {
            "native.update.install"
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmflow_sidecar::{RuntimeHandle, RuntimeOptions};

    #[test]
    fn installation_pauses_switching_and_failure_restores_the_previous_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut options =
            RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
        options.backend = "simulated".into();
        let runtime = RuntimeHandle::start(options, |_, _| {}).unwrap();
        for enabled in [true, false] {
            runtime
                .request("app.setEnabled", json!({ "enabled": enabled }))
                .unwrap();
            let result = install_with_runtime(&runtime, || {
                assert_eq!(
                    runtime.request("state.get", json!({})).unwrap()["enabled"],
                    false
                );
                Err("permission denied".into())
            });
            assert!(result.is_err());
            assert_eq!(
                runtime.request("state.get", json!({})).unwrap()["enabled"],
                enabled
            );
            assert!(!dir.path().join("config.json").exists());
        }
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    use kvmflow_core::{
        errors::{CoreError, E_BACKEND},
        mixed::TopologyDisplay,
    };
    use kvmflow_sidecar::topology::{DisplayTopology, FakeDisplayTopology};
    struct Failure;
    impl DisplayTopology for Failure {
        fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError> {
            Ok(vec![])
        }
        fn detach(&mut self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn attach(&mut self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn wait_enumeration(&mut self) -> Result<(), CoreError> {
            Ok(())
        }
        fn restore_all(&mut self) -> Result<(), CoreError> {
            Err(CoreError::new(E_BACKEND, "restore failed"))
        }
    }
    struct SlowRestore;
    impl DisplayTopology for SlowRestore {
        fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError> {
            Ok(vec![])
        }
        fn detach(&mut self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn attach(&mut self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn wait_enumeration(&mut self) -> Result<(), CoreError> {
            Ok(())
        }
        fn restore_all(&mut self) -> Result<(), CoreError> {
            std::thread::sleep(Duration::from_millis(80));
            Ok(())
        }
    }
    #[test]
    fn timed_out_recovery_cancels_install_and_unfreezes_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = kvmflow_sidecar::RuntimeOptions::new(
            dir.path().join("config.json"),
            dir.path().join("logs"),
        );
        options.backend = "simulated".into();
        options.topology = Some(Box::new(SlowRestore));
        let runtime = kvmflow_sidecar::RuntimeHandle::start(options, |_, _| {}).unwrap();
        let mut installed = false;
        let result = install_with_runtime_timeout(&runtime, Duration::from_millis(1), || {
            installed = true;
            Ok(())
        });
        assert_eq!(result, Err("native.update.restoreDisplays"));
        assert!(!installed);
        // Resume was queued after release, so a late success cannot leave
        // the worker frozen or allow the installer to run in the background.
        runtime
            .request("app.setEnabled", json!({"enabled":true}))
            .unwrap();
        runtime.shutdown_and_join().unwrap();
    }

    #[test]
    fn installer_is_not_entered_until_displays_are_restored() {
        for fail in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut options = kvmflow_sidecar::RuntimeOptions::new(
                dir.path().join("config.json"),
                dir.path().join("logs"),
            );
            options.backend = "simulated".into();
            let fake = FakeDisplayTopology::default();
            let operations = fake.operations.clone();
            options.topology = Some(if fail {
                Box::new(Failure)
            } else {
                Box::new(fake)
            });
            let runtime = kvmflow_sidecar::RuntimeHandle::start(options, |_, _| {}).unwrap();
            let mut entered = false;
            let result = install_with_runtime(&runtime, || {
                entered = true;
                assert_eq!(*operations.lock().unwrap(), ["restore_all"]);
                Ok(())
            });
            assert_eq!(entered, !fail);
            assert_eq!(result.is_ok(), !fail);
            if !fail {
                runtime.shutdown_and_join().unwrap();
            }
        }
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;
    use kvmflow_sidecar::{topology::FakeDisplayTopology, RuntimeHandle, RuntimeOptions};
    fn runtime(dir: &std::path::Path) -> RuntimeHandle {
        let mut options = RuntimeOptions::new(dir.join("config.json"), dir.join("logs"));
        options.backend = "simulated".into();
        options.topology = Some(Box::new(FakeDisplayTopology::default()));
        RuntimeHandle::start(options, |_, _| {}).unwrap()
    }
    #[test]
    fn risk_decline_does_not_preserve_abandon_or_install() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        let mut preserved = false;
        let mut installed = false;
        assert_eq!(
            install_unrestored(
                &runtime,
                false,
                || {
                    preserved = true;
                    Ok(())
                },
                || {
                    installed = true;
                    Ok(())
                }
            ),
            Err("update.recovery.cancelled")
        );
        assert!(!preserved && !installed);
        runtime.request("state.get", json!({})).unwrap();
        runtime.shutdown_and_join().unwrap();
    }
    #[test]
    fn confirmed_risky_install_preserves_records_even_with_a_stopped_worker() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        let record = dir.path().join("display-detach.json");
        std::fs::write(&record, "unconfirmed original record").unwrap();
        runtime.shutdown_and_join().unwrap();
        let mut installed = false;
        install_unrestored(
            &runtime,
            true,
            || preserve_recovery_records(dir.path()),
            || {
                installed = true;
                assert_eq!(
                    std::fs::read_to_string(&record).unwrap(),
                    "unconfirmed original record"
                );
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("display-detach.before-update.json"))
                        .unwrap(),
                    "unconfirmed original record"
                );
                Ok(())
            },
        )
        .unwrap();
        assert!(installed);
    }
    #[test]
    fn backup_failure_prevents_installation_and_preserves_the_running_worker() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        std::fs::create_dir(dir.path().join("display-detach.json")).unwrap();
        let mut installed = false;
        assert_eq!(
            install_unrestored(
                &runtime,
                true,
                || preserve_recovery_records(dir.path()),
                || {
                    installed = true;
                    Ok(())
                }
            ),
            Err("native.update.recordBackupFailed")
        );
        assert!(!installed);
        runtime.request("state.get", json!({})).unwrap();
        runtime.shutdown_and_join().unwrap();
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[test]
    fn beta_endpoints_prepend_the_channel_and_preserve_all_stable_fallbacks() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let updater = config["plugins"]["updater"].clone();
        let defaults = updater["endpoints"].as_array().unwrap().clone();
        let endpoints = beta_endpoints(updater).unwrap();
        assert_eq!(endpoints[0].as_str(), BETA_ENDPOINT);
        assert_eq!(endpoints.len(), defaults.len() + 1);
        for (actual, expected) in endpoints[1..].iter().zip(defaults) {
            assert_eq!(actual.as_str(), expected.as_str().unwrap());
        }
    }

    struct UpdateServer {
        address: std::net::SocketAddr,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<Vec<String>>>,
    }

    impl UpdateServer {
        fn start(remote: &str) -> Self {
            // Blocking accept/read: accepted sockets cannot inherit O_NONBLOCK.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let remote = remote.to_owned();
            let worker = std::thread::spawn(move || {
                let mut paths = Vec::new();
                loop {
                    let (mut stream, _) = listener.accept().unwrap();
                    if stopping.load(Ordering::Acquire) {
                        return paths;
                    }
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    // TCP can split the request anywhere, including its first
                    // line. Wait for the complete header before responding.
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                        assert!(request.len() < 16 * 1024, "oversized request header");
                    }
                    let request = String::from_utf8(request).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap();
                    paths.push(path.to_owned());
                    let (status, body) = if matches!(path, "/beta" | "/stable") {
                        (
                            "200 OK",
                            json!({
                                "version": remote,
                                "url": "https://example.invalid/update.tar.gz",
                                "signature": "not-downloaded"
                            })
                            .to_string(),
                        )
                    } else {
                        ("503 Service Unavailable", String::new())
                    };
                    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
            });
            Self {
                address,
                stop,
                worker: Some(worker),
            }
        }

        fn finish(&mut self) -> Vec<String> {
            self.stop.store(true, Ordering::Release);
            // Wake blocking accept explicitly, including if updater.check failed.
            let _ = TcpStream::connect(self.address);
            self.worker.take().unwrap().join().unwrap()
        }
    }

    impl Drop for UpdateServer {
        fn drop(&mut self) {
            if self.worker.is_some() {
                self.finish();
            }
        }
    }

    // Exercise the pinned updater itself, including endpoint selection and its
    // default SemVer comparator, with loopback HTTP and no hardware or installer.
    fn check_versions(
        current: &str,
        remote: &str,
        beta_first: bool,
        fallback: bool,
    ) -> Option<String> {
        let mut server = UpdateServer::start(remote);
        let address = server.address;
        let mut context = mock_context(noop_assets());
        context.package_info_mut().version = current.parse().unwrap();
        context.config_mut().plugins.0.insert(
            "updater".into(),
            json!({
                "pubkey": "test", "dangerousInsecureTransportProtocol": true,
                "endpoints": [format!("http://{address}/stable")]
            }),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        let mut builder = app
            .updater_builder()
            .no_proxy()
            .timeout(Duration::from_secs(5));
        if beta_first {
            builder = builder
                .endpoints(vec![
                    format!("http://{address}/beta").parse().unwrap(),
                    // This must never be requested after the valid beta response,
                    // even if the beta version is older than the current app.
                    format!("http://{address}/unexpected").parse().unwrap(),
                ])
                .unwrap();
        }
        if fallback {
            builder = builder
                .endpoints(vec![
                    format!("http://{address}/missing-beta").parse().unwrap(),
                    format!("http://{address}/stable").parse().unwrap(),
                ])
                .unwrap();
        }
        let result = tauri::async_runtime::block_on(builder.build().unwrap().check());
        let paths = server.finish();
        let expected = if fallback {
            vec!["/missing-beta", "/stable"]
        } else if beta_first {
            vec!["/beta"]
        } else {
            vec!["/stable"]
        };
        assert_eq!(paths, expected);
        result.unwrap().map(|update| update.version)
    }

    #[test]
    fn http_fixture_waits_for_complete_request_headers() {
        let mut server = UpdateServer::start("0.4.0");
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        stream.write_all(b"GET /sta").unwrap();
        let mut byte = [0];
        let error = stream
            .read(&mut byte)
            .expect_err("response before complete headers");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        stream
            .write_all(b"ble HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(server.finish(), ["/stable"]);
    }

    #[test]
    fn missing_beta_endpoint_falls_back_to_stable() {
        assert_eq!(
            check_versions("0.3.0", "0.4.0", false, true).as_deref(),
            Some("0.4.0")
        );
    }

    #[test]
    fn default_updater_semver_promotes_beta_to_stable_and_never_downgrades() {
        for (current, remote, beta, expected) in [
            ("0.3.0", "0.4.0-beta.1", true, Some("0.4.0-beta.1")),
            ("0.4.0-beta.1", "0.4.0", false, Some("0.4.0")),
            ("0.4.0-beta.1", "0.3.0", false, None),
            ("0.4.0", "0.4.0-beta.1", true, None),
            ("0.4.0-beta.2", "0.4.0-beta.10", true, Some("0.4.0-beta.10")),
            ("0.4.0-beta.1", "0.4.0-beta.1", true, None),
        ] {
            assert_eq!(
                check_versions(current, remote, beta, false).as_deref(),
                expected
            );
        }
    }
}
