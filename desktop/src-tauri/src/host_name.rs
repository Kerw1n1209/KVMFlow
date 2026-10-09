fn normalize(value: Option<String>) -> Option<String> {
    value
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

pub fn computer_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());

    #[cfg(target_os = "windows")]
    let name = std::env::var("COMPUTERNAME").ok();

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let name = None;

    normalize(name)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn missing_and_blank_names_are_unavailable() {
        assert_eq!(normalize(None), None);
        assert_eq!(normalize(Some(" \n".into())), None);
    }

    #[test]
    fn computer_names_keep_unicode_and_remove_command_newlines() {
        assert_eq!(
            normalize(Some(" 工作电脑 \n".into())),
            Some("工作电脑".into())
        );
    }
}
