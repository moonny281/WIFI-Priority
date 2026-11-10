//! 桌面悬浮框窗口（label = "overlay"）：显示/隐藏、鼠标穿透、吸附四角、记住拖动位置。
//! 窗口内容由前端（overlay.html）渲染，前端量好内容尺寸后调用 overlay_fit 命令来贴合窗口大小。

use crate::native;
use crate::state::Shared;
use std::sync::Arc;
use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition};

/// 按当前设置应用：显示/隐藏、锁定（鼠标穿透）、位置。
pub fn apply(app: &AppHandle, sh: &Arc<Shared>) {
    let s = sh.settings.lock().unwrap().clone();
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.set_ignore_cursor_events(s.overlay_locked);
        if s.overlay_enabled {
            let _ = w.show();
            reposition(app, sh);
        } else {
            let _ = w.hide();
        }
    }
}

/// 前端量好内容的逻辑像素尺寸后调用，窗口随之缩放并重新定位。
pub fn fit(app: &AppHandle, sh: &Arc<Shared>, w: f64, h: f64) {
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.set_size(LogicalSize::new(w.max(40.0), h.max(30.0)));
        reposition(app, sh);
    }
}

/// 根据 overlay_anchor 计算位置（物理像素）并移动窗口。
pub fn reposition(app: &AppHandle, sh: &Arc<Shared>) {
    let win = match app.get_webview_window("overlay") {
        Some(w) => w,
        None => return,
    };
    let s = sh.settings.lock().unwrap().clone();
    let size = match win.outer_size() {
        Ok(sz) => sz,
        Err(_) => return,
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let (ow, oh) = (size.width as i32, size.height as i32);
    if ow < 2 || oh < 2 {
        return;
    }
    let (wl, wt, wr, wb) = native::work_area();
    let m = (6.0 * scale) as i32;

    let (x, y) = match s.overlay_anchor.as_str() {
        "TopLeft" => (wl + m, wt + m),
        "BottomLeft" => (wl + m, wb - oh - m),
        "BottomRight" => (wr - ow - m, wb - oh - m),
        "Free" => {
            let (vx, vy, vw, vh) = native::virtual_screen();
            let fx = (s.overlay_x * scale) as i32;
            let fy = (s.overlay_y * scale) as i32;
            (fx.clamp(vx, (vx + vw - ow).max(vx)), fy.clamp(vy, (vy + vh - oh).max(vy)))
        }
        _ => (wr - ow - m, wt + m), // TopRight
    };
    if let Ok(mut t) = sh.overlay_target.lock() {
        *t = Some((x, y));
    }
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

/// 窗口被移动：如果不是程序自己设置的位置，说明用户拖动了 → 切换为“自由位置”并记住。
pub fn on_moved(app: &AppHandle, sh: &Arc<Shared>, pos: PhysicalPosition<i32>) {
    if let Ok(t) = sh.overlay_target.lock() {
        if let Some((tx, ty)) = *t {
            if (pos.x - tx).abs() <= 2 && (pos.y - ty).abs() <= 2 {
                return;
            }
        }
    }
    let scale = app
        .get_webview_window("overlay")
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0);
    if let Ok(mut s) = sh.settings.lock() {
        s.overlay_anchor = "Free".to_string();
        s.overlay_x = pos.x as f64 / scale; // 与 WPF 版一致：存逻辑像素
        s.overlay_y = pos.y as f64 / scale;
    }
    sh.settings_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
}
