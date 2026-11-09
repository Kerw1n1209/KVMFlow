//! The macOS privacy grants keyboard and mouse sharing needs. Other platforms
//! need none, so every check passes there.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Permissions {
    /// Lets this computer receive input from the other one.
    pub accessibility: bool,
    /// Lets this computer send its input to the other one.
    pub input_monitoring: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Accessibility,
    InputMonitoring,
}
impl Permission {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "accessibility" => Some(Self::Accessibility),
            "input_monitoring" => Some(Self::InputMonitoring),
            _ => None,
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Permission, Permissions};
    use core_foundation::{
        base::TCFType,
        boolean::CFBoolean,
        dictionary::{CFDictionary, CFDictionaryRef},
        string::{CFString, CFStringRef},
    };

    const LISTEN_EVENT: u32 = 1;
    const GRANTED: u32 = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: CFStringRef;
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDCheckAccess(request: u32) -> u32;
        fn IOHIDRequestAccess(request: u32) -> bool;
    }

    pub fn current() -> Permissions {
        // SAFETY: both take no pointers and only read this process's TCC state.
        unsafe {
            Permissions {
                accessibility: AXIsProcessTrusted(),
                input_monitoring: IOHIDCheckAccess(LISTEN_EVENT) == GRANTED,
            }
        }
    }

    /// Adds the app to the System Settings list, then opens that list.
    pub fn request(permission: Permission) {
        let pane = match permission {
            Permission::Accessibility => {
                // SAFETY: the key is a constant from the framework, and the
                // dictionary outlives the call.
                unsafe {
                    let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
                    let options =
                        CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
                    AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
                }
                "Privacy_Accessibility"
            }
            Permission::InputMonitoring => {
                // SAFETY: takes no pointers.
                unsafe { IOHIDRequestAccess(LISTEN_EVENT) };
                "Privacy_ListenEvent"
            }
        };
        let _ = std::process::Command::new("/usr/bin/open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{pane}"
            ))
            .status();
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{Permission, Permissions};

    pub fn current() -> Permissions {
        Permissions {
            accessibility: true,
            input_monitoring: true,
        }
    }

    pub fn request(_: Permission) {}
}

pub use platform::{current, request};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_names_parse() {
        assert_eq!(
            Permission::parse("accessibility"),
            Some(Permission::Accessibility)
        );
        assert_eq!(
            Permission::parse("input_monitoring"),
            Some(Permission::InputMonitoring)
        );
        assert_eq!(Permission::parse("camera"), None);
    }
}
