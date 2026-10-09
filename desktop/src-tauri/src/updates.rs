use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
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

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Value, String> {
    let state = app.state::<Updates>();
    let mut pending = state
        .pending
        .try_lock()
        .map_err(|_| "更新操作正在进行，请稍后重试。")?;
    // A periodic check must not discard an already verified download.
    if let Some(update) = pending.update.as_ref() {
        return Ok(
            json!({ "version": update.version, "notes": update.body, "downloaded": pending.bytes.is_some() }),
        );
    }
    let mut update = app
        .updater_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "无法初始化更新，请重新安装应用。")?
        .check()
        .await
        .map_err(|_| "无法检查更新。请检查网络后重试。")?;
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
        .map_err(|_| "更新操作正在进行，请稍后重试。")?;
    if pending.bytes.is_some() {
        return Ok(());
    }
    let update = pending.update.as_ref().ok_or("请先检查更新。")?;
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
        .map_err(|_| "更新包下载或签名验证失败。请检查网络后重试。")?;
    // download() verifies the signature before these bytes can be installed.
    pending.bytes = Some(bytes);
    Ok(())
}

#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let state = app.state::<Updates>();
    let pending = state
        .pending
        .try_lock()
        .map_err(|_| "更新操作正在进行，请稍后重试。")?;
    let update = pending.update.as_ref().ok_or("请先检查更新。")?.clone();
    let bytes = pending.bytes.as_ref().ok_or("请先下载更新。")?.clone();
    state.installing.store(true, Ordering::Release);
    let runtime = app.state::<crate::Host>().runtime.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        install_with_runtime(&runtime, || {
            update.install(&bytes).map_err(|error| error.to_string())
        })?;
        // Windows's installer exits and relaunches the process itself.
        // macOS returns here; stop the runtime before explicitly restarting.
        runtime.shutdown_and_join();
        Ok(())
    })
    .await;
    state.installing.store(false, Ordering::Release);
    result
        .map_err(|_| "更新操作失败，请重启应用后重试。")?
        .map_err(str::to_string)?;
    app.restart();
}

fn install_with_runtime(
    runtime: &kvmflow_sidecar::RuntimeHandle,
    install: impl FnOnce() -> Result<(), String>,
) -> Result<(), &'static str> {
    // Requests run after any in-flight DDC work on the serial worker.
    // Pausing is transient; it does not change the saved configuration.
    let before = runtime
        .request("state.get", json!({}))
        .map_err(|_| "无法读取切换状态，请稍后重试。")?;
    runtime
        .request("app.setEnabled", json!({ "enabled": false }))
        .map_err(|_| "无法暂停自动切换，请稍后重试。")?;
    if let Err(error) = install() {
        let restored = runtime.request(
            "app.setEnabled",
            json!({ "enabled": before["enabled"] == true }),
        );
        eprintln!("Update installation failed: {error}");
        return Err(if restored.is_err() {
            "更新失败，自动切换未恢复。请重启应用后重试。"
        } else {
            "无法安装更新，请重试或从官网下载最新版。"
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
