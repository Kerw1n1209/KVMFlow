use std::path::{Path, PathBuf};

/// Explicitly retain Electron's user-data directory. Tauri's default path
/// uses the bundle identifier and would make an upgrade look unconfigured.
pub fn data_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("KVMFLOW_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/KVMFlow"))
        .ok_or_else(|| "无法找到用户目录。".into());
    #[cfg(target_os = "windows")]
    return std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|home| home.join("KVMFlow"))
        .ok_or_else(|| "无法找到用户配置目录。".into());
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err("当前硬件后端仅支持 macOS 和 Windows。".into())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("无效的保存位置。")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temp.write_all(bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temp.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_preferences_preserves_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("startup-preference.json");
        write_atomic(&path, br#"{"openAtLogin":true}"#).unwrap();
        write_atomic(&path, br#"{"openAtLogin":false}"#).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            r#"{"openAtLogin":false}"#
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
