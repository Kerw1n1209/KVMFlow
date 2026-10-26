// A purpose-specific external link command. No general shell/opener permission
// is exposed to the renderer, matching the native update download action.
const ISSUE_URL: &str = "https://github.com/Kerw1n1209/KVMFlow/issues/new";
const MAX_URL_LENGTH: usize = 2000;

fn allowed_url(value: &str) -> bool {
    if value.len() > MAX_URL_LENGTH || !value.is_ascii() || value.chars().any(char::is_control) {
        return false;
    }
    let Ok(url) = tauri::Url::parse(value) else {
        return false;
    };
    value.starts_with(&format!("{ISSUE_URL}?"))
        && url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.path() == "/Kerw1n1209/KVMFlow/issues/new"
        && url.fragment().is_none()
        && url
            .query_pairs()
            .all(|(key, _)| matches!(key.as_ref(), "template" | "title" | "report"))
        && url
            .query_pairs()
            .any(|(key, value)| key == "template" && value == "compatibility.yml")
}

#[tauri::command]
pub fn compatibility_open_issue(url: String) -> Result<(), String> {
    if !allowed_url(&url) {
        return Err("Invalid compatibility issue URL".into());
    }
    open_browser(&url)
}

#[tauri::command]
pub fn feedback_open_privacy() -> Result<(), String> {
    open_browser("https://kvmflow.net/privacy")
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("/usr/bin/open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("Unsupported browser platform".into());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    result.map(|_| ()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allows_only_bounded_compatibility_issue_links() {
        let valid =
            format!("{ISSUE_URL}?template=compatibility.yml&report=Monitor%201%0AVID%3APID");
        assert!(allowed_url(&valid));
        assert!(allowed_url(&format!(
            "{ISSUE_URL}?template=compatibility.yml"
        )));
        for invalid in [
            valid.replace("https:", "http:"),
            valid.replace("github.com", "github.com.evil.test"),
            valid.replace("github.com", "user@github.com"),
            valid.replace("issues/new?", "issues/new/extra?"),
            valid.replace("issues/new?", "issues/new.evil?"),
            valid.replace("compatibility.yml", "other.yml"),
            format!("{valid}#fragment"),
            format!("{valid}&body=extra"),
            format!("{valid}{}", "x".repeat(2000)),
            "file:///Users/private".into(),
            format!("{valid}\n"),
        ] {
            assert!(!allowed_url(&invalid), "accepted {invalid}");
        }
    }
}
