//! 小工具：单调时钟、配置文件读写、隐藏子进程窗口。

use crate::model::Settings;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static START: OnceLock<Instant> = OnceLock::new();

/// 程序启动以来的毫秒数（单调时钟，决策器用它计时）。
pub fn now_ms() -> i64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// "HH:MM:SS"
pub fn time_stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// "MM-dd HH:mm"
pub fn short_stamp() -> String {
    chrono::Local::now().format("%m-%d %H:%M").to_string()
}

/// 与 WPF 版相同的位置：%AppData%\WifiPriority\settings.json
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("WifiPriority").join("settings.json")
}

pub fn load_settings() -> Settings {
    let p = settings_path();
    if let Ok(text) = std::fs::read_to_string(&p) {
        let text = text.trim_start_matches('\u{feff}'); // WPF 版写的文件可能带 BOM
        if let Some(s) = Settings::from_json(text) {
            return s;
        }
    }
    Settings::fresh()
}

pub fn save_settings(s: &Settings) {
    let p = settings_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            if std::fs::rename(&tmp, &p).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

/// 不弹出控制台窗口（仅 Windows）。
pub fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn K32EmptyWorkingSet(process: isize) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn SetProcessInformation(process: isize, class: i32, info: *const std::ffi::c_void, size: u32) -> i32;
}

/// 把本进程的“内存优先级”调低（ProcessMemoryPriority = 0，MEMORY_PRIORITY_LOW = 2）：
/// 系统内存紧张时会优先回收本进程暂时不用的页。只影响内存页的回收顺序，不改 CPU 优先级，不影响响应速度。
pub fn lower_memory_priority() {
    #[cfg(windows)]
    unsafe {
        let v: u32 = 2;
        SetProcessInformation(GetCurrentProcess(), 0, &v as *const u32 as *const std::ffi::c_void, 4);
    }
}

/// 收缩本进程的工作集：把暂时用不到的页换出去，任务管理器里的“内存”会明显降下来（用到时再按需换回）。
pub fn trim_memory() {
    #[cfg(windows)]
    unsafe {
        K32EmptyWorkingSet(GetCurrentProcess());
    }
}
