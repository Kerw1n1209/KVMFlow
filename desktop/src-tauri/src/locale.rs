use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

fn configuration() -> &'static Value {
    static CONFIG: OnceLock<Value> = OnceLock::new();
    CONFIG.get_or_init(|| {
        serde_json::from_str(include_str!("../../src/renderer/i18n/messages.json"))
            .expect("Invalid KVMFlow language configuration")
    })
}

pub fn resolve(preference: &str, system_locale: &str) -> &'static str {
    match preference {
        "zh-CN" => "zh-CN",
        "en" => "en",
        _ if system_locale.eq_ignore_ascii_case("zh")
            || system_locale.to_ascii_lowercase().starts_with("zh-") =>
        {
            "zh-CN"
        }
        _ => "en",
    }
}

pub fn translate(locale: &str, key: &str) -> String {
    let messages = &configuration()["messages"];
    messages[locale][key]
        .as_str()
        .or_else(|| messages["en"][key].as_str())
        .unwrap_or(key)
        .to_string()
}

pub struct Locale {
    path: PathBuf,
    saved: Mutex<Value>,
}

impl Locale {
    pub fn load(data_dir: &std::path::Path) -> Self {
        let path = data_dir.join("ui-preferences.json");
        let saved = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|value| value.is_object())
            .unwrap_or_else(|| json!({}));
        Self {
            path,
            saved: Mutex::new(saved),
        }
    }

    pub fn preference(&self) -> Option<String> {
        self.saved.lock().unwrap()["language"]
            .as_str()
            .filter(|value| matches!(*value, "auto" | "zh-CN" | "en"))
            .map(str::to_string)
    }

    pub fn text(&self, key: &str) -> String {
        let saved = self.saved.lock().unwrap();
        // The WebView supplies the OS language even when the window is hidden.
        let locale = resolve(
            saved["language"].as_str().unwrap_or("auto"),
            saved["systemLocale"].as_str().unwrap_or("en"),
        );
        translate(locale, key)
    }

    pub fn error_text(&self, message: &str) -> String {
        // Translate only known authored errors. Preserve unknown OS/backend
        // diagnostics, device identifiers and other runtime evidence verbatim.
        for catalog in configuration()["messages"].as_object().unwrap().values() {
            if let Some((key, _)) = catalog
                .as_object()
                .unwrap()
                .iter()
                .find(|(_, value)| value.as_str() == Some(message))
            {
                return self.text(key);
            }
        }
        message.to_string()
    }

    pub fn save(&self, preference: &str, system_locale: &str) -> Result<(), String> {
        if !matches!(preference, "auto" | "zh-CN" | "en") {
            return Err("Unsupported language preference".into());
        }
        let mut saved = self.saved.lock().unwrap();
        let mut next = saved.clone();
        next["language"] = json!(preference);
        // Persist the resolved system language for tray-only startup. Refresh it
        // from navigator.languages every time the WebView boots.
        next["systemLocale"] = json!(resolve("auto", system_locale));
        if next != *saved {
            crate::paths::write_atomic(&self.path, &serde_json::to_vec_pretty(&next).unwrap())?;
            *saved = next;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_and_explicit_languages_resolve_consistently() {
        assert_eq!(resolve("auto", "zh-Hant-TW"), "zh-CN");
        assert_eq!(resolve("auto", "en-US"), "en");
        assert_eq!(resolve("auto", "de-DE"), "en");
        assert_eq!(resolve("en", "zh-CN"), "en");
        assert_eq!(resolve("zh-CN", "en-US"), "zh-CN");
    }

    #[test]
    fn preferences_survive_restart_and_preserve_other_ui_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ui-preferences.json"),
            r#"{"theme":"dark"}"#,
        )
        .unwrap();
        let locale = Locale::load(dir.path());
        locale.save("zh-CN", "en-US").unwrap();
        let restored = Locale::load(dir.path());
        assert_eq!(restored.preference().as_deref(), Some("zh-CN"));
        assert_eq!(restored.text("native.quit"), "退出 KVMFlow");
        assert_eq!(restored.saved.lock().unwrap()["theme"], "dark");
        restored.save("auto", "en-US").unwrap();
        assert_eq!(restored.text("native.quit"), "Quit KVMFlow");
        assert!(restored.save("fr", "en-US").is_err());
        assert_eq!(restored.preference().as_deref(), Some("auto"));
    }

    #[test]
    fn both_catalogs_have_identical_keys_and_native_messages() {
        let config = configuration();
        let chinese = config["messages"]["zh-CN"].as_object().unwrap();
        let english = config["messages"]["en"].as_object().unwrap();
        assert_eq!(
            chinese.keys().collect::<Vec<_>>(),
            english.keys().collect::<Vec<_>>()
        );
        assert_eq!(translate("unsupported", "native.quit"), "Quit KVMFlow");
        assert_eq!(translate("en", "missing.key"), "missing.key");
    }

    #[test]
    fn known_runtime_errors_translate_without_changing_backend_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let locale = Locale::load(dir.path());
        locale.save("en", "zh-CN").unwrap();
        assert_eq!(
            locale.error_text("后台组件已停止。"),
            "The background component has stopped."
        );
        let evidence = "DDC error for 用户显示器: 0x37";
        assert_eq!(locale.error_text(evidence), evidence);
    }
}
