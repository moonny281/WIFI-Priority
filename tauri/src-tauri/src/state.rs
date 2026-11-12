//! 引擎线程与界面线程共享的状态。

use crate::model::*;
use crate::util::unix_now;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub status: Mutex<Option<EngineStatus>>,
    pub logs: Mutex<Vec<String>>,
    pub last_switch_text: Mutex<String>,
    pub manual_pause: AtomicBool,
    pub paused_until: AtomicI64, // unix 秒，0 = 无
    pub wake: Mutex<Option<Sender<u32>>>,
    pub native_mode: AtomicBool,
    pub urgent_scan: AtomicBool, // 链路监视线程判死当前网络后置位：引擎下一轮无视扫描间隔立即扫描
    pub mica: AtomicBool,
    pub settings_dirty: AtomicBool, // 悬浮框被拖动等“后台改了设置”的情况，由保存线程落盘并通知前端
    pub overlay_target: Mutex<Option<(i32, i32)>>, // 程序最近一次设置的悬浮框位置（物理像素），用来区分“用户拖动”
}

impl Shared {
    pub fn new(settings: Settings) -> Shared {
        Shared {
            settings: Mutex::new(settings),
            status: Mutex::new(None),
            logs: Mutex::new(Vec::new()),
            last_switch_text: Mutex::new("尚无切换记录".to_string()),
            manual_pause: AtomicBool::new(false),
            paused_until: AtomicI64::new(0),
            wake: Mutex::new(None),
            native_mode: AtomicBool::new(false),
            urgent_scan: AtomicBool::new(false),
            mica: AtomicBool::new(false),
            settings_dirty: AtomicBool::new(false),
            overlay_target: Mutex::new(None),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.manual_pause.load(Ordering::Relaxed) || unix_now() < self.paused_until.load(Ordering::Relaxed)
    }

    /// 自动恢复时间（unix 秒）；手动无限期暂停或未暂停时为 None。
    pub fn paused_until_opt(&self) -> Option<i64> {
        let u = self.paused_until.load(Ordering::Relaxed);
        if !self.manual_pause.load(Ordering::Relaxed) && unix_now() < u {
            Some(u)
        } else {
            None
        }
    }

    pub fn set_manual_pause(&self, paused: bool) {
        self.manual_pause.store(paused, Ordering::Relaxed);
        if !paused {
            self.paused_until.store(0, Ordering::Relaxed);
        }
        self.wake();
    }

    /// 立即唤醒引擎做一轮评估。
    pub fn wake(&self) {
        if let Ok(g) = self.wake.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(0);
            }
        }
    }
}
