//! 救援线程（v1.2.2）。
//!
//! 背景：热点被突然关掉时，Windows 的 WLAN 服务 / 驱动会无响应 5–9 秒（日志里 `WlanQueryInterface(当前连接) 9135 ms`、
//! `WlanGetAvailableNetworkList 5288 ms`），引擎线程被卡在这些调用里，即使链路监视已经判死了当前网络，
//! 也要等这一轮结束才能回落。
//!
//! 做法：链路监视判死后启动一个救援线程，最多观察 4 秒——
//!  - 引擎自己发出了连接请求 → 什么都不做（正常路径，毫秒级）；
//!  - 引擎这一轮卡住 >= 600 ms → 用**独立的 WLAN 句柄**对预先算好的回落方案直接发起连接（见 Decider::fallback_plan）。
//! 如果卡住的是 WLAN 服务内部的锁，独立句柄的请求也会被卡，救援就帮不上——日志会写出耗时，据此判断。
//!
//! 防护：10 秒内最多救援一次；暂停状态不救援；回落方案超过 15 秒没刷新就作废；回落目标在退避期里不救援；
//! 判死之后 Windows（或用户）已经自己连上了别的网络（ACM connection_complete）也不救援；设置 `RescueConnect` 可一键关闭。

use crate::decide::UnusableMap;
use crate::native;
use crate::state::Shared;
use crate::util::now_ms;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn spawn(sh: Arc<Shared>, dead_ssid: String, verdict_at: i64, unusable: UnusableMap, log: Arc<dyn Fn(&str) + Send + Sync>) {
    if !sh.settings.lock().unwrap().rescue_connect {
        return;
    }
    let last = sh.rescued_at.load(Ordering::Relaxed);
    if last != 0 && verdict_at - last < 10_000 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("wifi-rescue".into())
        .stack_size(256 * 1024)
        .spawn(move || run(sh, dead_ssid, verdict_at, unusable, log));
}

fn run(sh: Arc<Shared>, dead_ssid: String, verdict_at: i64, unusable: UnusableMap, log: Arc<dyn Fn(&str) + Send + Sync>) {
    let deadline = verdict_at + 4000;
    let stalled_ms: i64;
    loop {
        if sh.last_connect_at.load(Ordering::Relaxed) >= verdict_at {
            return; // 引擎自己已经发起了回落连接
        }
        if native::LAST_CONN_COMPLETE.load(Ordering::Relaxed) >= verdict_at {
            return; // 判死之后已经有连接完成了（Windows 自己回落 / 用户手动换网）
        }
        let now = now_ms();
        let busy = sh.tick_busy_since.load(Ordering::Relaxed);
        if busy != 0 && now - busy >= 600 {
            stalled_ms = now - busy;
            break; // 引擎这一轮卡住了
        }
        if now >= deadline {
            return; // 引擎没卡：它自己会处理（也可能是暂停 / 没有候选，不该由救援越俎代庖）
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if sh.is_paused() {
        return;
    }
    let now = now_ms();
    let fb = match sh.fallback.lock().ok().and_then(|g| g.clone()) {
        Some(f) => f,
        None => {
            log("救援：引擎被系统调用卡住，但没有预先算好的回落方案，无法救援");
            return;
        }
    };
    if now - fb.made_at > 15_000 || fb.plan.ssid == dead_ssid {
        log("救援：预先算好的回落方案已过期，不救援");
        return;
    }
    if unusable.lock().ok().and_then(|m| m.get(&fb.plan.ssid).map(|u| u.until > now)).unwrap_or(false) {
        return; // 回落目标自己刚判死过，还在退避期
    }
    let guid = match fb.guid {
        Some(g) => g,
        None => return,
    };

    sh.rescued_at.store(now, Ordering::Relaxed);
    sh.switch_until.store(now + 3000, Ordering::Relaxed); // 和正常切换一样：3 秒内链路监视不评估
    let t = Instant::now();
    let ok = match native::Client::open_quiet() {
        Some(c) => c.connect_guid(&fb.plan.profile, None, guid),
        None => false,
    };
    log(&format!(
        "救援：引擎已被系统调用卡住 {} ms，救援线程直接发起回落连接 {} → {}（{}，WlanConnect 用时 {} ms）",
        stalled_ms,
        dead_ssid,
        fb.plan.to_label,
        if ok { "请求已被系统接受" } else { "系统没有接受请求" },
        t.elapsed().as_millis()
    ));
}
