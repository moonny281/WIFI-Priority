// 发布版不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod commands;
mod decide;
mod engine;
mod model;
mod native;
mod netsh;
mod ops;
mod overlay;
mod parse;
mod state;
mod tray;
mod util;

#[cfg(test)]
mod tests;

use engine::Host;
use model::EngineStatus;
use state::Shared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::window::{Effect, EffectsBuilder};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_notification::NotificationExt;

/// 引擎 → 界面 / 托盘 / 系统通知。
struct TauriHost {
    app: AppHandle,
    sh: Arc<Shared>,
}

impl Host for TauriHost {
    fn on_status(&self, s: &EngineStatus) {
        let _ = self.app.emit("status", s);
        tray::update(&self.app, s);
    }

    fn on_log(&self, line: &str) {
        let _ = self.app.emit("log", line);
    }

    fn on_switched(&self, from: Option<&str>, to: &str, reason: &str) {
        let text = self.sh.last_switch_text.lock().map(|t| t.clone()).unwrap_or_default();
        let _ = self.app.emit("switched", text);
        let notify = self.sh.settings.lock().map(|s| s.notify).unwrap_or(false);
        if notify {
            let _ = self
                .app
                .notification()
                .builder()
                .title("已切换 WiFi")
                .body(format!("{} → {}\n{}", from.unwrap_or("未连接"), to, reason))
                .show();
        }
    }
}

fn main() {
    util::now_ms(); // 启动单调时钟
    let background = std::env::args().any(|a| a == "--background");
    let shared = Arc::new(Shared::new(util::load_settings()));

    tauri::Builder::default()
        // 单实例：第二次启动只是把已有窗口唤出来（必须最先注册）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            ops::show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            commands::get_info,
            commands::get_settings,
            commands::update_settings,
            commands::get_status,
            commands::get_logs,
            commands::clear_logs,
            commands::scan_now,
            commands::set_paused,
            commands::add_profile,
            commands::autostart_get,
            commands::autostart_set,
            commands::open_location_settings,
            commands::overlay_fit,
            commands::exit_app,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let sh = shared.clone();

            // ---- 主窗口：云母 + 关闭按钮缩到托盘 ----
            let main = app.get_webview_window("main").ok_or("找不到主窗口")?;
            let mica = main.set_effects(EffectsBuilder::new().effect(Effect::Mica).build()).is_ok();
            sh.mica.store(mica, Ordering::Relaxed);
            let main2 = main.clone();
            main.on_window_event(move |ev| {
                if let WindowEvent::CloseRequested { api, .. } = ev {
                    api.prevent_close();
                    let _ = main2.hide();
                }
            });

            // ---- 悬浮框：用户拖动后记住位置 ----
            if let Some(ov) = app.get_webview_window("overlay") {
                let (a, s) = (handle.clone(), sh.clone());
                ov.on_window_event(move |ev| {
                    if let WindowEvent::Moved(pos) = ev {
                        overlay::on_moved(&a, &s, *pos);
                    }
                });
            }

            tray::build(&handle, &sh)?;
            ops::settings_changed(&handle, &sh); // 应用主题、悬浮框、托盘勾选

            engine::spawn(Arc::new(TauriHost { app: handle.clone(), sh: sh.clone() }), sh.clone());

            // 悬浮框被拖动时只改了内存里的设置，这里 1 秒内落盘并通知前端
            {
                let (a, s) = (handle.clone(), sh.clone());
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(1));
                    if s.settings_dirty.swap(false, Ordering::Relaxed) {
                        let cur = s.settings.lock().unwrap().clone();
                        util::save_settings(&cur);
                        let _ = a.emit("settings", &cur);
                    }
                });
            }

            if !background {
                let _ = main.show();
                let _ = main.set_focus();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 WiFi 优先级 失败");
}
