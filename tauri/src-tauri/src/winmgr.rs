//! 窗口按需创建 / 销毁（v1.1.9，降低后台内存占用）。
//!
//! 每个 Tauri 窗口都带一个 WebView2 渲染进程（每个几十 MB）。旧版启动时就创建主窗口和悬浮框，
//! 关闭主窗口也只是隐藏——所以“只在托盘里待着”时仍然占着两个 WebView。现在：
//!  - 主窗口只在需要时创建，点关闭就真正销毁（WebView2 进程随之退出）；
//!  - v1.2.0：悬浮框默认改为原生（GDI）窗口（见 nativeov.rs），完全不需要 WebView2；
//!    这里的网页版悬浮框只在设置里关闭了“原生悬浮框”或原生窗口创建失败时才会创建；
//!  - 主窗口没打开、悬浮框是原生的时候，后台只剩 Rust 引擎本身（几 MB）。
//! 创建窗口一律放到单独线程里做：Windows 上在主线程（同步命令 / 事件回调里）直接 build() 会死锁。
//! 两个窗口的 WebView2 启动参数必须完全一致（同一个用户数据目录的环境选项不一致会创建失败）。

use crate::overlay;
use crate::state::Shared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::window::{Effect, EffectsBuilder};
use tauri::{AppHandle, Manager, Theme, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// 在 Tauri 默认参数基础上限制 V8 堆，并让渲染进程数量尽量少（界面很简单，用不着多进程）。
pub const WV_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
--js-flags=--max-old-space-size=48 --renderer-process-limit=1 --in-process-gpu --disable-background-networking";

fn apply_theme(app: &AppHandle, sh: &Arc<Shared>) {
    let theme = match sh.settings.lock().unwrap().theme.as_str() {
        "Dark" => Some(Theme::Dark),
        "Light" => Some(Theme::Light),
        _ => None,
    };
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_theme(theme);
    }
}

/// 打开（必要时创建）主窗口并置前。
pub fn open_main(app: &AppHandle, sh: &Arc<Shared>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let (app, sh) = (app.clone(), sh.clone());
    std::thread::spawn(move || {
        let built = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .title("WiFi 优先级")
            .inner_size(1000.0, 680.0)
            .min_inner_size(880.0, 580.0)
            .center()
            .visible(false)
            .transparent(true)
            .disable_drag_drop_handler()
            .additional_browser_args(WV_ARGS)
            .build();
        let w = match built {
            Ok(w) => w,
            Err(_) => return, // 已经有同名窗口（连点两次）等情况：忽略
        };
        let mica = w.set_effects(EffectsBuilder::new().effect(Effect::Mica).build()).is_ok();
        sh.mica.store(mica, Ordering::Relaxed);
        sh.main_open.store(true, Ordering::Relaxed);
        apply_theme(&app, &sh);
        let sh2 = sh.clone();
        w.on_window_event(move |ev| {
            if let WindowEvent::Destroyed = ev {
                sh2.main_open.store(false, Ordering::Relaxed);
                sh2.wake(); // 引擎回到省电节奏
                // 窗口销毁后把进程工作集收一收（WebView2 进程已经随窗口退出）
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    crate::util::trim_memory();
                });
            }
        });
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    });
}

/// 确保悬浮框窗口存在（不存在就创建，创建完成后应用位置/层级并显示）。
pub fn ensure_overlay(app: &AppHandle, sh: &Arc<Shared>) {
    if app.get_webview_window("overlay").is_some() {
        return;
    }
    let (app, sh) = (app.clone(), sh.clone());
    std::thread::spawn(move || {
        let built = WebviewWindowBuilder::new(&app, "overlay", WebviewUrl::App("overlay.html".into()))
            .title("WiFi 悬浮框")
            .inner_size(260.0, 80.0)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .shadow(false)
            .focused(false)
            .focusable(false)
            .visible(false)
            .disable_drag_drop_handler()
            .additional_browser_args(WV_ARGS)
            .build();
        let w = match built {
            Ok(w) => w,
            Err(_) => return,
        };
        // 悬浮框被拖动后记住位置
        let (a, s) = (app.clone(), sh.clone());
        w.on_window_event(move |ev| {
            if let WindowEvent::Moved(pos) = ev {
                overlay::on_moved(&a, &s, *pos);
            }
        });
        overlay::apply(&app, &sh);
    });
}

/// 销毁悬浮框窗口（释放它的 WebView2 进程）。
pub fn close_overlay(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.destroy();
    }
}
