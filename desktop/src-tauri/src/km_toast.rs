//! The brief notice on the computer the keyboard and mouse just reached. The
//! window never takes focus or clicks, so typing carries on underneath it.
use serde_json::Value;
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

const LABEL: &str = "km-toast";
const HEIGHT: f64 = 51.0;
const PADDING: f64 = 24.0;
const BOTTOM_GAP: f64 = 96.0;
const VISIBLE: Duration = Duration::from_millis(1500);

static PREPARING: AtomicBool = AtomicBool::new(false);
static SHOWN: AtomicU64 = AtomicU64::new(0);

pub fn arrived(previous: &Value, current: &Value) -> bool {
    previous["input_here"] == false && current["input_here"] == true && current["enabled"] == true
}

/// A generous estimate: the window is opaque, so spare width only adds margin
/// and the text is never cut.
fn notice_width(text: &str) -> f64 {
    let glyphs: f64 = text
        .chars()
        .map(|c| match c {
            'a'..='l' | 'n'..='v' | 'x'..='z' | '0'..='9' | ' ' | '-' => 10.0,
            _ => 16.0,
        })
        .sum();
    glyphs + PADDING * 2.0
}

/// Loads the page ahead of the first switch. Windows must be built off the
/// main thread, or WebView2 can deadlock.
pub fn prepare(app: &AppHandle) {
    if app.get_webview_window(LABEL).is_some() || PREPARING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let built = WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::App("km-toast.html".into()))
            .title("KVMFlow")
            .decorations(false)
            .resizable(false)
            .always_on_top(true)
            .visible_on_all_workspaces(true)
            .skip_taskbar(true)
            .focusable(false)
            .focused(false)
            .shadow(false)
            .visible(false)
            .inner_size(notice_width(""), HEIGHT)
            .build();
        if let Ok(window) = built {
            let _ = window.set_ignore_cursor_events(true);
        }
        PREPARING.store(false, Ordering::Release);
    });
}

pub fn show(app: &AppHandle, text: &str) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let width = notice_width(text);
    let _ = window.set_size(LogicalSize::new(width, HEIGHT));
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let area = monitor.work_area();
        let scale = monitor.scale_factor();
        let x = f64::from(area.position.x) + (f64::from(area.size.width) - width * scale) / 2.0;
        let y = f64::from(area.position.y) + f64::from(area.size.height)
            - (HEIGHT + BOTTOM_GAP) * scale;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    let Ok(literal) = serde_json::to_string(text) else {
        return;
    };
    let _ = window.eval(format!(
        "document.getElementById('text').textContent={literal}"
    ));
    let _ = window.show();
    let shown = SHOWN.fetch_add(1, Ordering::AcqRel) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(VISIBLE);
        if SHOWN.load(Ordering::Acquire) == shown {
            let _ = window.hide();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_arriving_while_sharing_shows_a_notice() {
        let at = |here: bool, enabled: bool| json!({ "input_here": here, "enabled": enabled });
        assert!(arrived(&at(false, true), &at(true, true)));
        assert!(!arrived(&at(true, true), &at(false, true)));
        assert!(!arrived(&at(true, true), &at(true, true)));
        assert!(!arrived(&at(false, true), &at(true, false)));
        assert!(!arrived(&json!({}), &at(true, true)));
    }

    #[test]
    fn wide_glyphs_get_more_room() {
        assert!(notice_width("已切到 游戏 PC") >= 157.0);
        assert!(notice_width("Switched to Gaming PC") >= 231.0);
        assert!(notice_width("WWWWWWWWWWWWWWWWWWWW") >= 306.0 + PADDING * 2.0);
    }
}
