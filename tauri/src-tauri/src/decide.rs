//! 纯决策逻辑（不做任何 IO，时间由调用方以毫秒传入）。与 WPF 版的 Decider 一一对应。
//!
//! 排序规则（band_policy）：Off 仅按列表顺序；Same 先按列表顺序、同一网络内偏好频段；Global 偏好频段的网络整体优先。
//! 升级：更优候选信号 >= up_threshold，持续 confirm_ms 毫秒，且过了冷却时间。
//! 回落：当前候选信号 < down_threshold 或消失，持续 confirm_ms 毫秒后切到次优候选（此时只要求 >= down_threshold，不受冷却限制）。
//! 未连接：立刻连最优候选（不等确认）。

use crate::model::*;
use crate::parse::band_name;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const NEVER: i64 = i64::MIN / 4;
const SETTLE_MS: i64 = 4000; // 发出连接请求后，最多等这么久看结果

#[derive(Clone, Debug)]
pub struct Plan {
    pub ssid: String,
    pub band: i32,
    pub bssid: Option<String>,
    pub iface: i32,
    pub profile: String,
    pub reason: String,
    pub from_label: Option<String>,
    pub to_label: String,
}

impl Plan {
    fn key(&self) -> String {
        format!("{}|{}", self.ssid, self.band)
    }
}

#[derive(Default, Debug)]
pub struct Decision {
    pub connect: Option<Plan>,
    pub pause_minutes: i64, // > 0：检测到手动换网，需要暂停这么多分钟
    pub logs: Vec<String>,
}

pub type BadMap = Arc<Mutex<HashMap<String, i64>>>;

struct Cand {
    ssid: String,
    bssid: Option<String>,
    profile: String,
    band: i32,
    signal: i64,
    rank: usize,
    iface: i32,
}

impl Cand {
    fn key(&self) -> String {
        format!("{}|{}", self.ssid, self.band)
    }
}

pub struct Decider {
    pending_key: Option<String>,
    pending_since: i64,
    prev: Option<String>,
    expected: Option<String>,
    last_switch: i64,
    last_plan: Option<Plan>,
    last_plan_at: i64,
    bad: BadMap,
}

fn label(ssid: &str, band: i32) -> String {
    if band > 0 {
        format!("{}（{}）", ssid, band_name(band))
    } else {
        ssid.to_string()
    }
}

fn is_bad_key(bad: &BadMap, key: &str, now: i64) -> bool {
    bad.lock().unwrap().get(key).map(|&u| u > now).unwrap_or(false)
}

fn build_cands(st: &WifiState, pri: &[String], policy: &str) -> Vec<Cand> {
    let mut list = Vec::new();
    for (i, p) in pri.iter().enumerate() {
        let n = match st.nearby.iter().find(|x| &x.ssid == p) {
            Some(n) => n,
            None => continue,
        };
        let profile = n.profile.clone().unwrap_or_else(|| n.ssid.clone());

        if policy == "Off" || n.bss.is_empty() {
            let any = n.bss.iter().max_by_key(|b| b.signal);
            list.push(Cand {
                ssid: n.ssid.clone(),
                bssid: None,
                profile,
                band: 0,
                signal: if st.connected && n.ssid == st.ssid { st.signal } else { n.signal },
                rank: i,
                iface: any.map(|b| b.iface).unwrap_or(-1),
            });
            continue;
        }
        // 每个频段取信号最强的接入点
        let mut bands: Vec<i32> = n.bss.iter().map(|b| b.band).collect();
        bands.sort_unstable();
        bands.dedup();
        for bd in bands {
            let b = n.bss.iter().filter(|x| x.band == bd).max_by_key(|x| x.signal).unwrap();
            list.push(Cand {
                ssid: n.ssid.clone(),
                bssid: Some(b.bssid.clone()),
                profile: profile.clone(),
                band: bd,
                signal: b.signal,
                rank: i,
                iface: b.iface,
            });
        }
    }
    list
}

fn band_rank(b: i32, prefer24: bool) -> i32 {
    if prefer24 {
        match b {
            2 => 0,
            5 => 1,
            6 => 2,
            _ => 3,
        }
    } else {
        match b {
            6 => 0,
            5 => 1,
            2 => 2,
            _ => 3,
        }
    }
}

fn order(c: &mut Vec<Cand>, policy: &str, prefer24: bool) {
    // sort_by_key 是稳定排序
    match policy {
        "Global" => c.sort_by_key(|x| (band_rank(x.band, prefer24), x.rank)),
        "Same" => c.sort_by_key(|x| (x.rank, band_rank(x.band, prefer24))),
        _ => c.sort_by_key(|x| x.rank),
    }
}

impl Decider {
    pub fn new() -> Decider {
        Decider {
            pending_key: None,
            pending_since: 0,
            prev: None,
            expected: None,
            last_switch: NEVER,
            last_plan: None,
            last_plan_at: 0,
            bad: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 联网检测线程用它来标记“暂时不可用”的网络。
    pub fn bad_handle(&self) -> BadMap {
        self.bad.clone()
    }

    pub fn mark_bad(&self, key: &str, until_ms: i64) {
        self.bad.lock().unwrap().insert(key.to_string(), until_ms);
    }

    /// 引擎执行连接后回报结果（ok = false 表示系统没有接受连接请求）。
    pub fn connect_result(&mut self, p: &Plan, ok: bool, now: i64) {
        if ok {
            return;
        }
        self.last_plan = None;
        self.mark_bad(&p.ssid, now + 120_000);
    }

    pub fn decide(&mut self, st: &WifiState, s: &Settings, now: i64, paused_in: bool) -> Decision {
        let mut d = Decision::default();
        let mut paused = paused_in;
        let cur: Option<&str> = if st.connected { Some(st.ssid.as_str()) } else { None };

        // 1) 上一次连接请求的结果（不阻塞：每次评估时顺带检查）
        if let Some(lp) = self.last_plan.clone() {
            let on_target = st.connected && st.ssid == lp.ssid;
            let band_ok = lp.band == 0 || st.cur_band == 0 || st.cur_band == lp.band;
            let el = now - self.last_plan_at;
            if on_target && band_ok && !st.connecting {
                self.last_plan = None; // 成功
            } else if el >= SETTLE_MS && !st.connecting {
                if on_target {
                    self.mark_bad(&lp.key(), now + 600_000);
                    d.logs.push(format!(
                        "未能切换到 {}（路由器或网卡驱动没有接受），10 分钟内不再尝试该频段",
                        label(&lp.ssid, lp.band)
                    ));
                } else {
                    self.mark_bad(&lp.ssid, now + 30_000);
                    d.logs.push(format!("连接 {} 没有成功，30 秒内不再尝试", lp.ssid));
                }
                self.last_plan = None;
            }
        }

        // 2) 手动换网检测：用户在 Windows 里自己换了网络 → 暂停一段时间，避免和用户对着干
        if st.connected {
            let manual = match &self.prev {
                Some(p) => {
                    *p != st.ssid
                        && self.expected.as_deref() != Some(st.ssid.as_str())
                        && s.manual_pause_minutes > 0
                }
                None => false,
            };
            if manual {
                d.pause_minutes = s.manual_pause_minutes;
                paused = true;
                self.pending_key = None;
                d.logs.push(format!(
                    "检测到网络被手动切换到 {}，自动切换暂停 {} 分钟",
                    st.ssid, s.manual_pause_minutes
                ));
            }
            self.prev = Some(st.ssid.clone());
        } else {
            self.prev = None;
        }

        if st.location_denied || paused {
            self.pending_key = None;
            return d;
        }

        // 3) 连接请求还在进行中：不打断
        if st.connecting || self.last_plan.is_some() {
            return d;
        }

        let pri: Vec<String> = s.priorities.clone();
        let policy: &str = match s.band_policy.as_str() {
            "Off" => "Off",
            "Same" => "Same",
            _ => "Global",
        };
        let prefer24 = s.prefer_band == "2.4G";
        let mut cands = build_cands(st, &pri, policy);
        let bad = self.bad.clone();

        let is_current = |c: &Cand| -> bool {
            match cur {
                Some(cs) => c.ssid == cs && (policy == "Off" || st.cur_band == 0 || c.band == st.cur_band),
                None => false,
            }
        };
        let is_bad = |c: &Cand| -> bool { is_bad_key(&bad, &c.ssid, now) || is_bad_key(&bad, &c.key(), now) };

        let cur_signal: Option<i64> = cands.iter().filter(|c| is_current(c)).map(|c| c.signal).max();
        let cur_bad = cands.iter().filter(|c| is_current(c)).any(|c| is_bad(c));
        let cur_listed = cur.map(|c| pri.iter().any(|p| p == c)).unwrap_or(false);
        let cur_ok = cur_signal.map(|sg| !cur_bad && sg >= s.down_threshold).unwrap_or(false);

        // 未连接，或当前（列表内）网络已经不行了：只要求候选 >= 回落阈值，避免卡在烂信号上
        let relaxed = cur.is_none() || (cur_listed && !cur_ok);

        let usable = |c: &Cand| -> bool {
            if is_bad(c) {
                return false;
            }
            let cur_c = is_current(c);
            let saved = !c.profile.is_empty()
                && (st.profiles.contains(&c.profile) || st.profiles.contains(&c.ssid));
            if !cur_c && !saved {
                return false; // 没有已保存配置，无法连接
            }
            let th = if cur_c || relaxed { s.down_threshold } else { s.up_threshold };
            c.signal >= th
        };

        order(&mut cands, policy, prefer24);
        let best = match cands.iter().find(|c| usable(c)) {
            Some(b) => b,
            None => {
                self.pending_key = None;
                return d;
            }
        };
        if is_current(best) {
            self.pending_key = None;
            return d;
        }
        if cur.is_some() && !cur_listed && !s.takeover_unlisted {
            self.pending_key = None;
            return d;
        }

        let same_ssid = cur.map(|c| best.ssid == c).unwrap_or(false);
        let reason = if cur.is_none() {
            "当前未连接".to_string()
        } else if !cur_listed {
            "当前网络不在优先级列表中".to_string()
        } else if cur_ok {
            if same_ssid {
                format!("同一网络有更优频段（{}）", band_name(best.band))
            } else {
                "出现更高优先级网络".to_string()
            }
        } else if same_ssid {
            "当前频段信号丢失或过弱，回落到另一频段".to_string()
        } else {
            "当前网络信号丢失或过弱，回落到次优先级".to_string()
        };

        // 4) 防抖：未连接时立刻连；否则候选必须持续最优 confirm_ms 毫秒，升级还要过冷却时间
        let best_key = best.key();
        if cur.is_some() {
            if self.pending_key.as_deref() != Some(best_key.as_str()) {
                self.pending_key = Some(best_key.clone());
                self.pending_since = now;
            }
            let wait_confirm = now - self.pending_since < s.confirm_ms.max(0);
            let cooling = (cur_ok || !cur_listed) && now - self.last_switch < s.cooldown_sec.max(0) * 1000;
            if wait_confirm || cooling {
                return d;
            }
        }

        let plan = Plan {
            ssid: best.ssid.clone(),
            band: best.band,
            iface: best.iface,
            profile: best.profile.clone(),
            bssid: if policy != "Off" && best.band > 0 { best.bssid.clone() } else { None },
            reason,
            from_label: cur.map(|c| label(c, st.cur_band)),
            to_label: label(&best.ssid, best.band),
        };
        self.pending_key = None;
        self.last_switch = now;
        self.last_plan = Some(plan.clone());
        self.last_plan_at = now;
        self.expected = Some(plan.ssid.clone());
        d.connect = Some(plan);
        d
    }
}
