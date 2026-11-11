//! 切换引擎（事件驱动，独立线程）：
//!  - 原生 WLAN 接口：每 250 ms 用进程内调用读一次缓存数据（< 5 ms），WLAN 通知（断线/连上/扫描完成）立刻唤醒；
//!  - 后台按 scan_interval_sec 主动请求扫描（0 = 连续扫描），扫描结果一出来就评估；
//!  - 决策逻辑在 decide.rs（纯函数），这里只负责 IO；
//!  - 原生读取失败时自动回退到 netsh 模式（慢，但兼容）。

use crate::decide::{Decider, Plan};
use crate::freshness::Freshness;
use crate::model::*;
use crate::native;
use crate::netsh;
use crate::parse;
use crate::state::Shared;
use crate::util::{now_ms, short_stamp, time_stamp, unix_now};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 引擎向外界（界面、托盘、通知）汇报的接口，由 main.rs 用 Tauri 实现。
pub trait Host: Send + Sync + 'static {
    fn on_status(&self, status: &EngineStatus);
    fn on_log(&self, line: &str);
    fn on_switched(&self, from: Option<&str>, to: &str, reason: &str);
}

pub fn spawn<H: Host>(host: Arc<H>, shared: Arc<Shared>) {
    let (tx, rx) = mpsc::channel::<u32>();
    if let Ok(mut g) = shared.wake.lock() {
        *g = Some(tx.clone());
    }
    let _ = std::thread::Builder::new().name("wifi-engine".into()).spawn(move || {
        let mut e = Engine::new(host, shared, tx);
        e.run(rx);
    });
}

fn log_line<H: Host>(host: &Arc<H>, sh: &Arc<Shared>, msg: &str) {
    let line = format!("{}  {}", time_stamp(), msg);
    if let Ok(mut l) = sh.logs.lock() {
        l.push(line.clone());
        if l.len() > 500 {
            l.remove(0);
        }
    }
    host.on_log(&line);
}

/// 确保“当前连接”出现在附近网络里，并以网卡上报的信号为准。
fn ensure_current(st: &mut WifiState) {
    if !st.connected {
        return;
    }
    let idx = match st.nearby.iter().position(|n| n.ssid == st.ssid) {
        Some(i) => i,
        None => {
            st.nearby.push(NearbyNetwork { ssid: st.ssid.clone(), ..Default::default() });
            st.nearby.len() - 1
        }
    };
    let (cur_bssid, cur_band, cur_iface, signal) = (st.cur_bssid.clone(), st.cur_band, st.cur_iface, st.signal);
    let cur_profile = st.cur_profile.clone();
    let n = &mut st.nearby[idx];
    if n.profile.is_none() {
        n.profile = cur_profile;
    }
    let pos = cur_bssid
        .as_ref()
        .and_then(|cb| n.bss.iter().position(|b| b.bssid.eq_ignore_ascii_case(cb)));
    let i = match pos {
        Some(i) => i,
        None => {
            n.bss.push(BssInfo {
                bssid: cur_bssid.clone().unwrap_or_default(),
                band: cur_band,
                iface: cur_iface,
                ..Default::default()
            });
            n.bss.len() - 1
        }
    };
    n.bss[i].signal = signal;
    if n.bss[i].band == 0 {
        n.bss[i].band = cur_band;
    }
    parse::summarize(n);
}

struct Engine<H: Host> {
    host: Arc<H>,
    sh: Arc<Shared>,
    client: Option<native::Client>,
    native_ok: bool,
    dec: Decider,
    fresh: Freshness, // 识别缓存里已经消失的接入点
    last_scan_at: i64,
    connecting_seen_at: i64, // 最近一次看到“正在连接”的时间
    scan_busy: bool,
    last_sig: String,
    last_pub_at: i64,
    cur_ssid: Arc<Mutex<Option<String>>>,
}

impl<H: Host> Engine<H> {
    fn new(host: Arc<H>, sh: Arc<Shared>, tx: mpsc::Sender<u32>) -> Engine<H> {
        let client = native::Client::open(tx);
        let native_ok = client.is_some();
        sh.native_mode.store(native_ok, std::sync::atomic::Ordering::Relaxed);
        Engine {
            host,
            sh,
            client,
            native_ok,
            dec: Decider::new(),
            fresh: Freshness::new(now_ms()),
            last_scan_at: -100_000,
            connecting_seen_at: 0,
            scan_busy: false,
            last_sig: String::new(),
            last_pub_at: 0,
            cur_ssid: Arc::new(Mutex::new(None)),
        }
    }

    fn log(&self, msg: &str) {
        log_line(&self.host, &self.sh, msg);
    }

    fn run(&mut self, rx: mpsc::Receiver<u32>) {
        if self.native_ok {
            self.log("已启用原生 WLAN 接口：事件驱动，毫秒级响应");
        } else {
            self.log("无法打开原生 WLAN 接口，使用 netsh 兼容模式（切换较慢）");
        }
        if BAND_POLICY_MIGRATED.load(std::sync::atomic::Ordering::SeqCst) {
            self.log("v1.1.3：频段策略已由“全局偏好频段优先”改为“仅同一网络内”（列表顺序绝对优先）——旧策略会让当前的 5G 网络排在列表更靠前的 2.4G 网络之前，导致更高优先级的网络出现时不切换。想恢复可在 设置 → 频段偏好 里改回");
        }
        self.spawn_internet_monitor();

        loop {
            let wait = self.tick();
            match rx.recv_timeout(Duration::from_millis(wait)) {
                Ok(code) => {
                    self.note(code);
                    while let Ok(c) = rx.try_recv() {
                        self.note(c);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn note(&mut self, code: u32) {
        if code == 7 || code == 8 {
            self.scan_busy = false; // 扫描完成 / 失败
        }
        if code == 7 {
            self.fresh.scan_done(now_ms());
        }
    }

    fn start_scan_if_due(&mut self, s: &Settings) {
        let now = now_ms();
        let gap = if s.scan_interval_sec <= 0 { 400 } else { s.scan_interval_sec * 1000 };
        if now - self.last_scan_at < gap {
            return;
        }
        // 正在连接时不扫描：用户在 Windows 里手动连、或我们自己刚发出的连接请求。
        // 有些网卡驱动在关联过程中被扫描打断会直接报“无法连接网络”（手机热点响应慢，更容易中招）。
        if self.connecting_seen_at != 0 && now - self.connecting_seen_at < 2000 {
            return;
        }
        if self.dec.active_target().is_some() {
            return;
        }
        if self.scan_busy && now - self.last_scan_at < 6000 {
            return; // 上一次扫描还没完成
        }
        self.last_scan_at = now;
        let mut ok = false;
        if let Some(c) = &self.client {
            ok = c.scan();
        }
        // 请求没被接受就不会有“扫描完成”通知，别把 scan_busy 挂着空等
        self.scan_busy = ok;
    }

    /// 一轮：读状态 → 决策 → 执行。返回下一轮前的等待毫秒数。
    fn tick(&mut self) -> u64 {
        let t0 = Instant::now();
        let s = self.sh.settings.lock().unwrap().clone();

        let mut st_opt: Option<WifiState> = None;
        if self.native_ok {
            self.start_scan_if_due(&s);
            if let Some(c) = self.client.as_mut() {
                st_opt = c.read_state();
            }
            if st_opt.is_none() {
                self.native_ok = false;
                self.sh.native_mode.store(false, std::sync::atomic::Ordering::Relaxed);
                self.log("原生 WLAN 读取校验失败，已回退到 netsh 兼容模式（切换较慢）");
            }
        }
        let mut st = match st_opt {
            Some(x) => x,
            None => {
                if let Some(c) = &self.client {
                    c.scan();
                    std::thread::sleep(Duration::from_millis(2500));
                }
                netsh::read_state()
            }
        };

        self.fresh.apply(&mut st, now_ms());
        ensure_current(&mut st);
        if st.connecting {
            self.connecting_seen_at = now_ms();
        }
        if let Ok(mut g) = self.cur_ssid.lock() {
            *g = if st.connected { Some(st.ssid.clone()) } else { None };
        }

        let dec = self.dec.decide(&st, &s, now_ms(), self.sh.is_paused());
        for l in &dec.logs {
            self.log(l);
        }
        if dec.pause_minutes > 0 {
            self.sh.paused_until.store(unix_now() + dec.pause_minutes * 60, std::sync::atomic::Ordering::Relaxed);
        }
        let switched = dec.connect.is_some();
        if let Some(plan) = dec.connect {
            self.execute(&plan, t0);
        }

        self.publish(&st, switched);
        if self.native_ok {
            250
        } else {
            (s.scan_interval_sec.max(3) * 1000) as u64
        }
    }

    fn execute(&mut self, plan: &Plan, t0: Instant) {
        self.log(&format!(
            "切换：{} → {}（{}）",
            plan.from_label.as_deref().unwrap_or("(未连接)"),
            plan.to_label,
            plan.reason
        ));

        let mut ok = false;
        if self.native_ok {
            if let Some(c) = &self.client {
                ok = c.connect(&plan.profile, plan.bssid.as_deref(), plan.iface);
            }
        }
        if !ok {
            ok = netsh::connect(&plan.profile, &plan.ssid); // 兜底（netsh 不能指定频段）
        }

        if ok {
            self.log(&format!(
                "连接请求已发出，从读取状态到发出请求用时 {} ms（之后是路由器握手时间）",
                t0.elapsed().as_millis()
            ));
        } else {
            self.log(&format!("系统没有接受连接 {} 的请求，2 分钟后重试", plan.to_label));
        }

        self.dec.connect_result(plan, ok, now_ms());
        if ok {
            if let Ok(mut t) = self.sh.last_switch_text.lock() {
                *t = format!(
                    "{}   {} → {}   （{}）",
                    short_stamp(),
                    plan.from_label.as_deref().unwrap_or("(未连接)"),
                    plan.to_label,
                    plan.reason
                );
            }
            self.host.on_switched(plan.from_label.as_deref(), &plan.to_label, &plan.reason);
        }
    }

    // ---------- 联网检测（独立后台线程，不阻塞切换） ----------
    // 只用来决定“刚连上的网络能不能上网，不能就回退”。不做任何冷却：离开那个网络后不留记忆，
    // 它还在、还是最优，就会再次连接。
    fn spawn_internet_monitor(&self) {
        let no_net = self.dec.no_net_handle();
        let cur = self.cur_ssid.clone();
        let sh = self.sh.clone();
        let host = self.host.clone();
        let _ = std::thread::Builder::new().name("wifi-ping".into()).spawn(move || {
            let mut fail = 0;
            let mut last: Option<String> = None;
            let mut since = now_ms();
            loop {
                std::thread::sleep(Duration::from_secs(3));
                let enabled = sh.settings.lock().map(|s| s.internet_check).unwrap_or(false);
                let ssid = cur.lock().ok().and_then(|g| g.clone());
                let ssid = match (enabled, ssid) {
                    (true, Some(s)) => s,
                    _ => {
                        fail = 0;
                        last = None;
                        continue;
                    }
                };
                // 刚连上（DHCP 还没完成、路由还没通）的 12 秒内不检测，避免把正在握手的热点误判成“无法上网”
                if last.as_deref() != Some(ssid.as_str()) {
                    last = Some(ssid.clone());
                    since = now_ms();
                    fail = 0;
                    continue;
                }
                if now_ms() - since < 12_000 {
                    continue;
                }
                if netsh::has_internet() {
                    fail = 0;
                    if let Ok(mut g) = no_net.lock() {
                        if g.as_deref() == Some(ssid.as_str()) {
                            *g = None;
                        }
                    }
                } else {
                    fail += 1;
                    if fail >= 3 {
                        // 连续 3 次（约 9 秒）都不通才回退
                        fail = 0;
                        let mut newly = false;
                        if let Ok(mut g) = no_net.lock() {
                            if g.as_deref() != Some(ssid.as_str()) {
                                *g = Some(ssid.clone());
                                newly = true;
                            }
                        }
                        if newly {
                            log_line(&host, &sh, &format!("{} 已连接但无法上网，回退到下一个可用网络", ssid));
                            sh.wake();
                        }
                    }
                }
            }
        });
    }

    // ---------- 发布给界面（限频：状态没变化时每 2 秒一次） ----------
    fn publish(&mut self, st: &WifiState, force: bool) {
        let paused = self.sh.is_paused();
        let target = self.dec.active_target();
        let connecting = st.connecting || target.is_some();
        let sig = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            st.connected,
            st.ssid,
            st.cur_band,
            st.signal,
            paused,
            st.location_denied,
            connecting,
            target.as_deref().unwrap_or(""),
            st.nearby.iter().map(|n| format!("{}:{}", n.ssid, n.signal)).collect::<Vec<_>>().join(",")
        );
        let now = now_ms();
        if !force && sig == self.last_sig && now - self.last_pub_at < 2000 {
            return;
        }
        self.last_sig = sig;
        self.last_pub_at = now;

        let mut nearby = st.nearby.clone();
        nearby.sort_by(|a, b| b.signal.cmp(&a.signal));
        let mut profiles: Vec<String> = st.profiles.iter().cloned().collect();
        profiles.sort();

        let status = EngineStatus {
            connected: st.connected,
            ssid: if st.connected { Some(st.ssid.clone()) } else { None },
            signal: st.signal,
            cur_band: st.cur_band,
            cur_bssid: st.cur_bssid.clone(),
            location_denied: st.location_denied,
            paused,
            paused_until: self.sh.paused_until_opt(),
            connecting,
            target,
            message: if st.location_denied {
                Some("位置服务未开启".to_string())
            } else if st.nearby.is_empty() && !st.connected {
                Some("未扫描到任何网络".to_string())
            } else {
                None
            },
            native: self.native_ok,
            nearby,
            profiles,
        };
        if let Ok(mut g) = self.sh.status.lock() {
            *g = Some(status.clone());
        }
        self.host.on_status(&status);
    }
}
