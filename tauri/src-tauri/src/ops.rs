//! 托盘、命令、悬浮框都会用到的几个操作。

use crate::overlay;
use crate::state::Shared;
use crate::tray;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Theme};

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 设置被修改后统一做的事：主窗口主题、悬浮框、托盘勾选、通知前端、唤醒引擎。
pub fn settings_changed(app: &AppHandle, sh: &Arc<Shared>) {
    let s = sh.settings.lock().unwrap().clone();
    if let Some(w) = app.get_webview_window("main") {
        let theme = match s.theme.as_str() {
            "Dark" => Some(Theme::Dark),
            "Light" => Some(Theme::Light),
            _ => None, // 跟随系统
        };
        let _ = w.set_theme(theme);
    }
    overlay::apply(app, sh);
    tray::refresh_checks(app, sh);
    let _ = app.emit("settings", &s);
    sh.wake();
}

/// 修改设置（闭包里改），保存并应用。
pub fn mutate_settings<F: FnOnce(&mut crate::model::Settings)>(app: &AppHandle, sh: &Arc<Shared>, f: F) {
    {
        let mut s = sh.settings.lock().unwrap();
        f(&mut s);
        s.sanitize();
        crate::util::save_settings(&s);
    }
    settings_changed(app, sh);
}

pub fn toggle_pause(sh: &Arc<Shared>) {
    let paused = sh.is_paused();
    sh.set_manual_pause(!paused);
}
