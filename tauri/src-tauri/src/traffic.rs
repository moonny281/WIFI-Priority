//! 实时流量与用量统计（v1.1.9）。
//!
//! 数据来自 WiFi 网卡驱动（NDIS）的 64 位收发字节计数器（GetIfEntry2，见 link.rs）——是硬件累计值，
//! 采样间隔拉长不会丢数据，只是速度的粒度变粗，所以没人看的时候可以降到很低的频率。
//!
//! 按“网络”和“连接”区分：
//!  - 网络（SSID）：今日 / 累计用量，持久化到 %AppData%\WifiPriority\traffic.json；
//!  - 连接（一次连接 = SSID 或 BSSID 变化后重新计）：本次连接用量，最近 30 次连接的记录也会保存。
//! 悬浮框显示：实时 ↓↑ 速度，以及 本次 / 今日 / 累计。

use crate::link::{CurLink, IfSnapshot};
use crate::state::Shared;
use crate::util::{now_ms, unix_now};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const MAX_NETS: usize = 200;
const MAX_RECENT: usize = 30;

/// 发给界面的实时快照。
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSnap {
    pub connected: bool,
    pub ssid: String,
    pub rx_bps: u64, // 实时下行（字节/秒）
    pub tx_bps: u64, // 实时上行
    pub sess_rx: u64, // 本次连接
    pub sess_tx: u64,
    pub sess_secs: i64,
    pub today_rx: u64, // 该网络今日
    pub today_tx: u64,
    pub total_rx: u64, // 该网络累计
    pub total_tx: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct NetStat {
    pub total_rx: u64,
    pub total_tx: u64,
    pub today_rx: u64,
    pub today_tx: u64,
    pub day: String, // yyyy-MM-dd：today_* 对应的日期
    pub last_seen: i64, // unix 秒
    pub sessions: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SessionRec {
    pub ssid: String,
    pub start: i64, // unix 秒
    pub secs: i64,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Store {
    pub nets: HashMap<String, NetStat>,
    pub recent: Vec<SessionRec>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NetRow {
    pub ssid: String,
    pub total_rx: u64,
    pub total_tx: u64,
    pub today_rx: u64,
    pub today_tx: u64,
    pub sessions: u32,
    pub last_seen: i64,
    pub current: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub ssid: String,
    pub start: i64,
    pub secs: i64,
    pub rx: u64,
    pub tx: u64,
    pub active: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TrafficStats {
    pub nets: Vec<NetRow>,
    pub sessions: Vec<SessionRow>,
}

struct Cur {
    ssid: String,
    conn_at: i64,
    guid: [u8; 16],
    last_in: u64,
    last_out: u64,
    last_at: i64, // 单调毫秒
    sess_rx: u64,
    sess_tx: u64,
    start_unix: i64,
    start_ms: i64,
    rx_bps: f64,
    tx_bps: f64,
}

/// 计费（纯逻辑，便于测试）：读数函数由调用方给，时间由调用方传入。
pub struct Acct {
    pub store: Store,
    cur: Option<Cur>,
    pub dirty: bool,
    pub save_soon: bool,
}

fn delta(prev: u64, now: u64) -> u64 {
    if now >= prev {
        now - prev
    } else {
        now // 计数器回零（网卡被重置 / 禁用再启用）：从 0 重新计
    }
}

impl Acct {
    pub fn new(store: Store) -> Acct {
        Acct { store, cur: None, dirty: false, save_soon: false }
    }

    fn add(&mut self, ssid: &str, rx: u64, tx: u64, today: &str, unix: i64) {
        if rx == 0 && tx == 0 {
            return;
        }
        if !self.store.nets.contains_key(ssid) && self.store.nets.len() >= MAX_NETS {
            // 记录太多：丢掉最久没见过的
            if let Some(old) = self.store.nets.iter().min_by_key(|(_, v)| v.last_seen).map(|(k, _)| k.clone()) {
                self.store.nets.remove(&old);
            }
        }
        let n = self.store.nets.entry(ssid.to_string()).or_default();
        if n.day != today {
            n.day = today.to_string();
            n.today_rx = 0;
            n.today_tx = 0;
        }
        n.total_rx = n.total_rx.saturating_add(rx);
        n.total_tx = n.total_tx.saturating_add(tx);
        n.today_rx = n.today_rx.saturating_add(rx);
        n.today_tx = n.today_tx.saturating_add(tx);
        n.last_seen = unix;
        self.dirty = true;
    }

    /// 读一次当前连接的计数器，把增量记到“网络”和“本次连接”上，并更新实时速度。
    fn sample(&mut self, read: &dyn Fn(&[u8; 16]) -> Option<IfSnapshot>, now: i64, today: &str, unix: i64) {
        let (ssid, guid) = match &self.cur {
            Some(c) => (c.ssid.clone(), c.guid),
            None => return,
        };
        let snap = match read(&guid) {
            Some(s) => s,
            None => return,
        };
        let (d_in, d_out, dt) = {
            let c = self.cur.as_mut().unwrap();
            let d_in = delta(c.last_in, snap.in_octets);
            let d_out = delta(c.last_out, snap.out_octets);
            let dt = (now - c.last_at).max(1);
            c.last_in = snap.in_octets;
            c.last_out = snap.out_octets;
            c.last_at = now;
            c.sess_rx = c.sess_rx.saturating_add(d_in);
            c.sess_tx = c.sess_tx.saturating_add(d_out);
            let (ri, ti) = (d_in as f64 * 1000.0 / dt as f64, d_out as f64 * 1000.0 / dt as f64);
            c.rx_bps = 0.6 * ri + 0.4 * c.rx_bps; // 轻度平滑，免得数字乱跳
            c.tx_bps = 0.6 * ti + 0.4 * c.tx_bps;
            (d_in, d_out, dt)
        };
        let _ = dt;
        self.add(&ssid, d_in, d_out, today, unix);
    }

    fn close_session(&mut self, read: &dyn Fn(&[u8; 16]) -> Option<IfSnapshot>, now: i64, today: &str, unix: i64) {
        self.sample(read, now, today, unix); // 最后读一次，把断开前的增量记到它头上
        if let Some(c) = self.cur.take() {
            let secs = (now - c.start_ms) / 1000;
            if c.sess_rx + c.sess_tx > 0 || secs >= 5 {
                self.store.recent.push(SessionRec { ssid: c.ssid, start: c.start_unix, secs, rx: c.sess_rx, tx: c.sess_tx });
                if self.store.recent.len() > MAX_RECENT {
                    let extra = self.store.recent.len() - MAX_RECENT;
                    self.store.recent.drain(0..extra);
                }
            }
            self.dirty = true;
            self.save_soon = true;
        }
    }

    /// 喂入当前连接（None = 没连上）。返回给界面的快照。
    pub fn feed(
        &mut self,
        cl: Option<&CurLink>,
        read: &dyn Fn(&[u8; 16]) -> Option<IfSnapshot>,
        now: i64,
        today: &str,
        unix: i64,
    ) -> TrafficSnap {
        let changed = match (&self.cur, cl) {
            (Some(c), Some(l)) => c.ssid != l.ssid || c.conn_at != l.connected_at,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        if changed && self.cur.is_some() {
            self.close_session(read, now, today, unix);
        }
        if self.cur.is_none() {
            if let Some(l) = cl {
                if let Some(s) = read(&l.guid) {
                    self.cur = Some(Cur {
                        ssid: l.ssid.clone(),
                        conn_at: l.connected_at,
                        guid: l.guid,
                        last_in: s.in_octets,
                        last_out: s.out_octets,
                        last_at: now,
                        sess_rx: 0,
                        sess_tx: 0,
                        start_unix: unix,
                        start_ms: now,
                        rx_bps: 0.0,
                        tx_bps: 0.0,
                    });
                    let n = self.store.nets.entry(l.ssid.clone()).or_default();
                    n.sessions = n.sessions.saturating_add(1);
                    n.last_seen = unix;
                    if n.day != today {
                        n.day = today.to_string();
                        n.today_rx = 0;
                        n.today_tx = 0;
                    }
                    self.dirty = true;
                }
            }
        } else {
            self.sample(read, now, today, unix);
        }
        self.snapshot(now, today)
    }

    pub fn snapshot(&self, now: i64, today: &str) -> TrafficSnap {
        match &self.cur {
            None => TrafficSnap::default(),
            Some(c) => {
                let n = self.store.nets.get(&c.ssid).cloned().unwrap_or_default();
                let same_day = n.day == today;
                TrafficSnap {
                    connected: true,
                    ssid: c.ssid.clone(),
                    rx_bps: c.rx_bps.round() as u64,
                    tx_bps: c.tx_bps.round() as u64,
                    sess_rx: c.sess_rx,
                    sess_tx: c.sess_tx,
                    sess_secs: (now - c.start_ms) / 1000,
                    today_rx: if same_day { n.today_rx } else { 0 },
                    today_tx: if same_day { n.today_tx } else { 0 },
                    total_rx: n.total_rx,
                    total_tx: n.total_tx,
                }
            }
        }
    }

    pub fn stats(&self, now: i64) -> TrafficStats {
        let cur_ssid = self.cur.as_ref().map(|c| c.ssid.clone());
        let mut nets: Vec<NetRow> = self
            .store
            .nets
            .iter()
            .map(|(k, v)| NetRow {
                ssid: k.clone(),
                total_rx: v.total_rx,
                total_tx: v.total_tx,
                today_rx: v.today_rx,
                today_tx: v.today_tx,
                sessions: v.sessions,
                last_seen: v.last_seen,
                current: cur_ssid.as_deref() == Some(k.as_str()),
            })
            .collect();
        nets.sort_by(|a, b| (b.total_rx + b.total_tx).cmp(&(a.total_rx + a.total_tx)));
        let mut sessions: Vec<SessionRow> = Vec::new();
        if let Some(c) = &self.cur {
            sessions.push(SessionRow {
                ssid: c.ssid.clone(),
                start: c.start_unix,
                secs: (now - c.start_ms) / 1000,
                rx: c.sess_rx,
                tx: c.sess_tx,
                active: true,
            });
        }
        for r in self.store.recent.iter().rev() {
            sessions.push(SessionRow { ssid: r.ssid.clone(), start: r.start, secs: r.secs, rx: r.rx, tx: r.tx, active: false });
        }
        TrafficStats { nets, sessions }
    }

    /// 清零某个网络（None = 全部）的用量统计。当前连接的“本次用量”不动。
    pub fn reset(&mut self, ssid: Option<&str>) {
        match ssid {
            Some(s) => {
                self.store.nets.remove(s);
                self.store.recent.retain(|r| r.ssid != s);
            }
            None => {
                self.store.nets.clear();
                self.store.recent.clear();
            }
        }
        self.dirty = true;
        self.save_soon = true;
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let p = traffic_path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&self.store) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
                self.dirty = false;
            }
        }
        self.save_soon = false;
    }
}

pub fn traffic_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir());
    base.join("WifiPriority").join("traffic.json")
}

pub fn load_store() -> Store {
    std::fs::read_to_string(traffic_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Store>(&t).ok())
        .unwrap_or_default()
}

fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 有人在看（悬浮框开着并且显示流量，或主窗口开着）才用 1 秒的间隔并推送给界面；否则 3 秒采一次（只为把用量记对）。
fn watching(sh: &Shared) -> bool {
    if sh.main_open.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    let g = sh.settings.lock().unwrap();
    g.overlay_enabled && g.overlay_show_traffic
}

/// 采样线程。cur 是引擎发布的“当前连接”（SSID / 网卡 GUID / 连接开始时间）。
pub fn spawn(app: AppHandle, sh: Arc<Shared>, cur: Arc<Mutex<Option<CurLink>>>) {
    let _ = std::thread::Builder::new().name("wifi-traffic".into()).spawn(move || {
        let mut last_emit = TrafficSnap::default();
        let mut last_save = now_ms();
        loop {
            let fast = watching(&sh);
            std::thread::sleep(Duration::from_millis(if fast { 1000 } else { 3000 }));
            let cl = cur.lock().ok().and_then(|g| g.clone());
            let (now, unix, today) = (now_ms(), unix_now(), today_str());
            let snap = {
                let mut a = sh.acct.lock().unwrap();
                let snap = a.feed(cl.as_ref(), &|g| crate::link::read_if(g), now, &today, unix);
                if a.save_soon || now - last_save > 60_000 {
                    a.save();
                    last_save = now;
                }
                snap
            };
            sh.busy_bps.store(snap.rx_bps + snap.tx_bps, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut g) = sh.traffic.lock() {
                *g = snap.clone();
            }
            // 速度/用量有变化才推送（sess_secs 每秒都变，不算）
            let mut cmp = snap.clone();
            cmp.sess_secs = last_emit.sess_secs;
            if fast && cmp != last_emit {
                let _ = app.emit("traffic", &snap);
            }
            last_emit = snap;
        }
    });
}
