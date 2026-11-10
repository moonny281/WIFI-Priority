//! 系统托盘：图标（已连接 / 未连接 / 已暂停）与右键菜单。

use crate::model::EngineStatus;
use crate::ops;
use crate::state::Shared;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const ICON_CONNECTED: &[u8] = include_bytes!("../icons/tray-connected.png");
const ICON_IDLE: &[u8] = include_bytes!("../icons/tray-idle.png");
const ICON_PAUSED: &[u8] = include_bytes!("../icons/tray-paused.png");

pub struct TrayItems {
    pause: MenuItem<Wry>,
    overlay: CheckMenuItem<Wry>,
    lock: CheckMenuItem<Wry>,
    state: AtomicI32,
}

fn icon_for(state: i32) -> Option<Image<'static>> {
    let bytes = match state {
        2 => ICON_PAUSED,
        1 => ICON_CONNECTED,
        _ => ICON_IDLE,
    };
    Image::from_bytes(bytes).ok()
}

pub fn build(app: &AppHandle, sh: &Arc<Shared>) -> tauri::Result<()> {
    let s = sh.settings.lock().unwrap().clone();

    let open = MenuItem::with_id(app, "open", "打开主窗口", true, None::<&str>)?;
    let scan = MenuItem::with_id(app, "scan", "立即扫描", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "暂停自动切换", true, None::<&str>)?;
    let overlay = CheckMenuItem::with_id(app, "overlay", "显示桌面悬浮框", true, s.overlay_enabled, None::<&str>)?;
    let lock = CheckMenuItem::with_id(app, "lock", "锁定悬浮框（鼠标穿透）", true, s.overlay_locked, None::<&str>)?;
    let a_tl = MenuItem::with_id(app, "anchor_TopLeft", "左上角", true, None::<&str>)?;
    let a_tr = MenuItem::with_id(app, "anchor_TopRight", "右上角", true, None::<&str>)?;
    let a_bl = MenuItem::with_id(app, "anchor_BottomLeft", "左下角", true, None::<&str>)?;
    let a_br = MenuItem::with_id(app, "anchor_BottomRight", "右下角", true, None::<&str>)?;
    let pos = Submenu::with_items(app, "悬浮框位置", true, &[&a_tl, &a_tr, &a_bl, &a_br])?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let exit = MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &scan, &sep1, &pause, &overlay, &lock, &pos, &sep2, &exit])?;

    app.manage(TrayItems { pause, overlay, lock, state: AtomicI32::new(-1) });

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("WiFi 优先级")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id().0.clone();
            handle_menu(app, &id);
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                ops::show_main(tray.app_handle());
            }
        });
    if let Some(img) = icon_for(0) {
        builder = builder.icon(img);
    }
    builder.build(app)?;
    Ok(())
}

fn handle_menu(app: &AppHandle, id: &str) {
    let sh = app.state::<Arc<Shared>>().inner().clone();
    match id {
        "open" => ops::show_main(app),
        "scan" => sh.wake(),
        "pause" => {
            ops::toggle_pause(&sh);
            refresh_pause_text(app, &sh);
        }
        "overlay" => ops::mutate_settings(app, &sh, |s| s.overlay_enabled = !s.overlay_enabled),
        "lock" => ops::mutate_settings(app, &sh, |s| s.overlay_locked = !s.overlay_locked),
        "exit" => app.exit(0),
        other => {
            if let Some(a) = other.strip_prefix("anchor_") {
                let a = a.to_string();
                ops::mutate_settings(app, &sh, move |s| {
                    s.overlay_enabled = true;
                    s.overlay_anchor = a;
                });
            }
        }
    }
}

fn refresh_pause_text(app: &AppHandle, sh: &Arc<Shared>) {
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.pause.set_text(if sh.is_paused() { "恢复自动切换" } else { "暂停自动切换" });
    }
}

/// 设置变化后同步菜单上的勾选状态。
pub fn refresh_checks(app: &AppHandle, sh: &Arc<Shared>) {
    let s = sh.settings.lock().unwrap().clone();
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.overlay.set_checked(s.overlay_enabled);
        let _ = items.lock.set_checked(s.overlay_locked);
    }
    refresh_pause_text(app, sh);
}

/// 引擎状态变化：换图标、改提示文字、同步“暂停/恢复”文字。
pub fn update(app: &AppHandle, st: &EngineStatus) {
    let state = if st.paused { 2 } else if st.connected { 1 } else { 0 };
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.pause.set_text(if st.paused { "恢复自动切换" } else { "暂停自动切换" });
        if items.state.swap(state, Ordering::Relaxed) != state {
            if let Some(tray) = app.tray_by_id("main") {
                if let Some(img) = icon_for(state) {
                    let _ = tray.set_icon(Some(img));
                }
            }
        }
    }
    if let Some(tray) = app.tray_by_id("main") {
        let mut tip = match &st.ssid {
            Some(s) if st.connected => format!("WiFi：{}", s),
            _ => "WiFi：未连接".to_string(),
        };
        if st.paused {
            tip.push_str("（已暂停）");
        }
        let _ = tray.set_tooltip(Some(tip));
    }
}
