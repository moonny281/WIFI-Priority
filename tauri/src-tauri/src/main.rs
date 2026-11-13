// 发布版不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod commands;
mod decide;
mod engine;
mod freshness;
mod link;
mod model;
mod native;
mod netsh;
mod ops;
mod overlay;
mod parse;
mod state;
mod traffic;
mod tray;
mod util;
mod winmgr;

#[cfg(test)]
mod tests;

use engine::Host;
use model::EngineStatus;
use state::Shared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
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
    let shared_exit = shared.clone();

    let app = tauri::Builder::default()
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
            commands::get_traffic,
            commands::get_traffic_stats,
            commands::reset_traffic,
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

            tray::build(&handle, &sh)?;
            ops::settings_changed(&handle, &sh); // 应用主题、悬浮框、托盘勾选

            engine::spawn(Arc::new(TauriHost { app: handle.clone(), sh: sh.clone() }), sh.clone());
            traffic::spawn(handle.clone(), sh.clone(), sh.cur_link.clone()); // 实时流量与用量统计

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

            // 悬浮框置顶兜底：置顶模式下每 2 秒重申一次层级（别的置顶窗口出现后悬浮框不会自己回到最上面）
            {
                let (a, s) = (handle.clone(), sh.clone());
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(2));
                    let (on, top) = {
                        let g = s.settings.lock().unwrap();
                        (g.overlay_enabled, g.overlay_layer != "Desktop")
                    };
                    if on && top {
                        overlay::apply_layer(&a, &s);
                    }
                });
            }

            // 后台内存：没有窗口时每 45 秒收一次工作集；启动 6 秒后先收一次（去掉启动峰值）
            {
                let s = sh.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(6));
                    util::trim_memory();
                    loop {
                        std::thread::sleep(Duration::from_secs(45));
                        if !s.main_open.load(Ordering::Relaxed) {
                            util::trim_memory();
                        }
                    }
                });
            }

            // 主窗口按需创建（--background 启动时不创建，只留托盘 + 悬浮框）
            if !background {
                winmgr::open_main(&handle, &sh);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("启动 WiFi 优先级 失败");

    app.run(move |_handle, event| match event {
        // 所有窗口都关了也不退出：程序常驻托盘（真正退出走托盘“退出” / app.exit(0)，此时 code 不是 None）
        tauri::RunEvent::ExitRequested { api, code, .. } => {
            if code.is_none() {
                api.prevent_exit();
            }
        }
        tauri::RunEvent::Exit => {
            if let Ok(mut a) = shared_exit.acct.lock() {
                a.save(); // 流量统计落盘
            }
        }
        _ => {}
    });
}
