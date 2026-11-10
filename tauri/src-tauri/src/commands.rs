//! 前端通过 invoke 调用的命令。

use crate::model::*;
use crate::state::Shared;
use crate::{autostart, netsh, ops, overlay, util};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    version: String,
    mica: bool,
    native: bool,
    last_switch: String,
}

#[tauri::command]
pub fn get_info(app: AppHandle, sh: State<'_, Arc<Shared>>) -> Info {
    Info {
        version: app.package_info().version.to_string(),
        mica: sh.mica.load(std::sync::atomic::Ordering::Relaxed),
        native: sh.native_mode.load(std::sync::atomic::Ordering::Relaxed),
        last_switch: sh.last_switch_text.lock().map(|t| t.clone()).unwrap_or_default(),
    }
}

#[tauri::command]
pub fn get_settings(sh: State<'_, Arc<Shared>>) -> Settings {
    sh.settings.lock().unwrap().clone()
}

/// 只改前端传来的那几个字段（其余保持不变），保存并应用。
#[tauri::command]
pub fn update_settings(app: AppHandle, sh: State<'_, Arc<Shared>>, patch: serde_json::Value) -> Result<Settings, String> {
    let sh = sh.inner().clone();
    let mut merged: Settings = {
        let cur = sh.settings.lock().unwrap();
        let mut v = serde_json::to_value(&*cur).map_err(|e| e.to_string())?;
        if let (Some(o), Some(p)) = (v.as_object_mut(), patch.as_object()) {
            for (k, val) in p {
                o.insert(k.clone(), val.clone());
            }
        }
        serde_json::from_value(v).map_err(|e| e.to_string())?
    };
    merged.sanitize();
    *sh.settings.lock().unwrap() = merged.clone();
    util::save_settings(&merged);
    ops::settings_changed(&app, &sh);
    Ok(merged)
}

#[tauri::command]
pub fn get_status(sh: State<'_, Arc<Shared>>) -> Option<EngineStatus> {
    sh.status.lock().unwrap().clone()
}

#[tauri::command]
pub fn get_logs(sh: State<'_, Arc<Shared>>) -> Vec<String> {
    sh.logs.lock().unwrap().clone()
}

#[tauri::command]
pub fn clear_logs(sh: State<'_, Arc<Shared>>) {
    sh.logs.lock().unwrap().clear();
}

#[tauri::command]
pub fn scan_now(sh: State<'_, Arc<Shared>>) {
    sh.wake();
}

#[tauri::command]
pub fn set_paused(sh: State<'_, Arc<Shared>>, paused: bool) {
    sh.set_manual_pause(paused);
}

#[tauri::command]
pub async fn add_profile(ssid: String, password: String) -> Result<(), String> {
    let ssid = ssid.trim().to_string();
    if ssid.is_empty() {
        return Err("请输入网络名称".into());
    }
    if password.len() < 8 || password.len() > 64 {
        return Err("WPA2 密码长度需为 8–63 个字符（或 64 位十六进制）".into());
    }
    let ok = tauri::async_runtime::spawn_blocking(move || netsh::add_profile(&ssid, &password))
        .await
        .map_err(|e| e.to_string())?;
    if ok {
        Ok(())
    } else {
        Err("保存网络配置失败，请检查名称/密码；WPA3、企业网络请先在 Windows 里手动连接一次".into())
    }
}

#[tauri::command]
pub async fn autostart_get() -> bool {
    tauri::async_runtime::spawn_blocking(autostart::is_enabled).await.unwrap_or(false)
}

#[derive(Serialize)]
pub struct AutoStartResult {
    ok: bool,
    enabled: bool,
}

#[tauri::command]
pub async fn autostart_set(enable: bool) -> Result<AutoStartResult, String> {
    let ok = tauri::async_runtime::spawn_blocking(move || if enable { autostart::enable() } else { autostart::disable() })
        .await
        .map_err(|e| e.to_string())?;
    let enabled = tauri::async_runtime::spawn_blocking(autostart::is_enabled).await.unwrap_or(false);
    Ok(AutoStartResult { ok, enabled })
}

#[tauri::command]
pub fn open_location_settings() {
    let mut c = std::process::Command::new("cmd");
    c.args(["/c", "start", "", "ms-settings:privacy-location"]);
    util::hide_window(&mut c);
    let _ = c.spawn();
}

/// 悬浮框前端量好内容尺寸（逻辑像素）后调用，窗口随之贴合。
#[tauri::command]
pub fn overlay_fit(app: AppHandle, sh: State<'_, Arc<Shared>>, w: f64, h: f64) {
    overlay::fit(&app, sh.inner(), w, h);
}

#[tauri::command]
pub fn exit_app(app: AppHandle) {
    app.exit(0);
}
