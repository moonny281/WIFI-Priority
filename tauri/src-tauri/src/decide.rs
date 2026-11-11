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
const DISC_GRACE_MS: i64 = 1200; // 刚断开的短暂窗口：别抢着打断用户手动发起的连接

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
    pub retried: bool, // true = 这是“指定接入点失败后改为不指定”的重试，不再重试第二次
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
    seen_at: i64, // 最近一次被扫描到并更新的时间（-1 = 无法判断，0 = 从未观察到更新）
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
    prev_signal: i64,
    pending_last_seen: i64,
    last_diag_at: i64,
    last_order_diag_at: i64,
    disc_at: i64, // 本次“未连接”状态开始的时间（0 = 当前不是未连接）
    not_before: i64, // 系统直接拒绝连接请求后，至少隔 3 秒再试（防止每 250 ms 空转，不是网络冷却）
    block_at: HashMap<String, i64>, // 连失败 / 非程序发起断开的网络 → 发生时间。之后必须被扫描到并更新过（有新证据）才允许再连
    skip_ssid: Option<String>, // 断网状态下刚连失败的网络：15 秒内先试下一个，别在断网时只盯着它死磕
    skip_until: i64,
    no_net: Arc<Mutex<Option<String>>>, // 当前连着、但检测不到互联网的 WiFi。离开它就清除，不留冷却
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

fn build_cands(st: &WifiState, pri: &[String], policy: &str, stale_filter: bool) -> Vec<Cand> {
    let mut list = Vec::new();
    for (i, p) in pri.iter().enumerate() {
        let n = match st.nearby.iter().find(|x| &x.ssid == p) {
            Some(n) => n,
            None => continue,
        };
        let profile = n.profile.clone().unwrap_or_else(|| n.ssid.clone());

        // 已消失接入点的缓存条目不参与（Windows 的 BSS 缓存会把关掉的热点留好一阵子，信号还是旧的）
        let live: Vec<&BssInfo> = n.bss.iter().filter(|b| !(stale_filter && b.stale)).collect();

        if policy == "Off" || n.bss.is_empty() {
            let any = live.iter().copied().max_by_key(|b| b.signal);
            if any.is_none() && !n.bss.is_empty() {
                continue; // 只剩过期条目：这个网络其实不在
            }
            list.push(Cand {
                ssid: n.ssid.clone(),
                bssid: None,
                profile,
                band: 0,
                signal: if st.connected && n.ssid == st.ssid { st.signal } else { any.map(|b| b.signal).unwrap_or(n.signal) },
                rank: i,
                iface: any.map(|b| b.iface).unwrap_or(-1),
                seen_at: any.map(|b| b.changed_at).unwrap_or(-1),
            });
            continue;
        }
        // 每个频段取信号最强的接入点
        let mut bands: Vec<i32> = live.iter().map(|b| b.band).collect();
        bands.sort_unstable();
        bands.dedup();
        for bd in bands {
            let b = live.iter().copied().filter(|x| x.band == bd).max_by_key(|x| x.signal).unwrap();
            list.push(Cand {
                ssid: n.ssid.clone(),
                bssid: Some(b.bssid.clone()),
                profile: profile.clone(),
                band: bd,
                signal: b.signal,
                rank: i,
                iface: b.iface,
                seen_at: b.changed_at,
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
            prev_signal: 100,
            pending_last_seen: 0,
            last_diag_at: NEVER,
            last_order_diag_at: NEVER,
            disc_at: 0,
            not_before: 0,
            block_at: HashMap::new(),
            skip_ssid: None,
            skip_until: 0,
            no_net: Arc::new(Mutex::new(None)),
            bad: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 联网检测线程用它来标记“暂时不可用”的网络。
    pub fn bad_handle(&self) -> BadMap {
        self.bad.clone()
    }

    /// 联网检测线程用它报告/清除“当前网络无法上网”。
    pub fn no_net_handle(&self) -> Arc<Mutex<Option<String>>> {
        self.no_net.clone()
    }

    pub fn report_no_internet(&self, ssid: &str) {
        *self.no_net.lock().unwrap() = Some(ssid.to_string());
    }

    pub fn mark_bad(&self, key: &str, until_ms: i64) {
        self.bad.lock().unwrap().insert(key.to_string(), until_ms);
    }

    /// 程序正在切换到的网络（连接请求已发出、结果还没落定）。给悬浮框显示“正在切换到 …”。
    pub fn active_target(&self) -> Option<String> {
        self.last_plan.as_ref().map(|p| p.to_label.clone())
    }

    /// 引擎执行连接后回报结果（ok = false 表示系统没有接受连接请求）。
    pub fn connect_result(&mut self, _p: &Plan, ok: bool, now: i64) {
        if ok {
            return;
        }
        self.last_plan = None;
        self.last_switch = NEVER; // 请求根本没发出去，不该占用升级冷却
        self.not_before = now + 3000;
    }

    pub fn decide(&mut self, st: &WifiState, s: &Settings, now: i64, paused_in: bool) -> Decision {
        let mut d = Decision::default();
        if st.connected {
            self.block_at.remove(&st.ssid); // 连上了，说明它在
        }
        if st.connected || st.connecting {
            self.disc_at = 0;
        } else if self.disc_at == 0 {
            self.disc_at = now;
        }
        {
            // 离开了那个无网络的 WiFi：不留记忆
            let mut g = self.no_net.lock().unwrap();
            let keep = match g.as_ref() {
                Some(n) => st.connected && st.ssid == *n,
                None => true,
            };
            if !keep {
                *g = None;
            }
        }
        let no_net: Option<String> = self.no_net.lock().unwrap().clone();
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
                if !on_target && lp.bssid.is_some() && !lp.retried {
                    // 指定接入点（BSSID）没连上：BSS 列表是缓存，那个接入点可能已经不在了。
                    // 先退一步只按配置文件重试一次，再决定要不要拉黑。
                    let retry = Plan {
                        band: 0,
                        bssid: None,
                        reason: format!("{}（指定接入点失败，改为不指定重试）", lp.reason),
                        retried: true,
                        ..lp.clone()
                    };
                    d.logs.push(format!("指定接入点连接 {} 未成功，改为只按配置文件重试", lp.to_label));
                    self.last_plan = Some(retry.clone());
                    self.last_plan_at = now;
                    d.connect = Some(retry);
                    return d;
                }
                if on_target {
                    self.mark_bad(&lp.key(), now + 120_000); // 只针对“指定这个频段”，不影响连接这个网络
                    d.logs.push(format!(
                        "未能切换到 {}（路由器或网卡驱动没有接受），2 分钟内不再尝试指定该频段",
                        label(&lp.ssid, lp.band)
                    ));
                } else {
                    // 不做失败冷却：目标网络还在、还是最优，就会再次尝试。
                    // 唯一的例外：断网状态下，先让下一个候选试一次，别在断网时只盯着失败的那个死磕。
                    d.logs.push(format!("连接 {} 没有成功，等扫描确认它还在再重试", lp.ssid));
                    self.block_at.insert(lp.ssid.clone(), now);
                    if !st.connected {
                        self.skip_ssid = Some(lp.ssid.clone());
                        self.skip_until = now + 15_000;
                    }
                }
                self.last_plan = None;
            }
        }

        // 2) 手动换网检测：用户在 Windows 里自己换了网络 → 暂停一段时间，避免和用户对着干
        if st.connected {
            let changed = self.prev.as_deref().map(|p| p != st.ssid).unwrap_or(false);
            let expected = self.expected.as_deref() == Some(st.ssid.as_str());
            // 换网之前原网络信号就已低于回落阈值：这是 Windows 自己在信号变差时漫游，不是用户手动换网
            let sys_roam = self.prev_signal < s.down_threshold;
            if changed && !expected && !sys_roam && s.manual_pause_minutes > 0 {
                d.pause_minutes = s.manual_pause_minutes;
                paused = true;
                self.pending_key = None;
                d.logs.push(format!(
                    "检测到网络被手动切换到 {}，自动切换暂停 {} 分钟",
                    st.ssid, s.manual_pause_minutes
                ));
            } else if changed && !expected && sys_roam {
                d.logs.push(format!(
                    "系统自行切换到 {}（原网络信号已低于回落阈值），不视为手动换网",
                    st.ssid
                ));
            }
            self.prev = Some(st.ssid.clone());
            self.prev_signal = st.signal;
        } else {
            // 非程序发起的断开（热点被关、信号丢失）：缓存里它的条目还是旧的，必须等扫描证明它又出现了才重连
            if let Some(p) = self.prev.take() {
                if self.last_plan.is_none() {
                    self.block_at.insert(p, now);
                }
            }
        }

        if st.location_denied || paused {
            self.pending_key = None;
            return d;
        }

        // 3) 连接请求还在进行中：不打断
        if st.connecting || self.last_plan.is_some() {
            return d;
        }

        // 刚断开（<1.2 秒）：可能是用户在 Windows 里手动换网（先断开再开始连接），也可能是系统自己在重连。
        // 这时抢着发起连接会把对方的连接打断，表现为“无法连接网络”。先等一下，看对方是否开始连接。
        if !st.connected && now - self.disc_at < DISC_GRACE_MS {
            return d;
        }
        if now < self.not_before {
            return d;
        }

        let pri: Vec<String> = s.priorities.clone();
        let policy: &str = match s.band_policy.as_str() {
            "Off" => "Off",
            "Global" => "Global",
            _ => "Same",
        };
        let prefer24 = s.prefer_band == "2.4G";
        let mut cands = build_cands(st, &pri, policy, s.stale_filter);
        let bad = self.bad.clone();

        let is_current = |c: &Cand| -> bool {
            match cur {
                Some(cs) => c.ssid == cs && (policy == "Off" || st.cur_band == 0 || c.band == st.cur_band),
                None => false,
            }
        };
        let block_at = self.block_at.clone();
        let blocked = |c: &Cand| -> bool {
            match block_at.get(&c.ssid) {
                None => false,
                Some(&b) => {
                    if c.seen_at > 0 {
                        c.seen_at <= b // 必须在失败/断开之后又被扫描到并更新过，才算还在
                    } else if c.seen_at < 0 {
                        now - b < 20_000 // 驱动没给时间戳，无法求证：退化为 20 秒间隔
                    } else {
                        true // 0 = 启动时缓存里的、从未观察到更新：不算证据
                    }
                }
            }
        };
        let skip: Option<String> = if !st.connected && now < self.skip_until { self.skip_ssid.clone() } else { None };
        let is_bad = |c: &Cand| -> bool {
            is_bad_key(&bad, &c.key(), now)
                || no_net.as_deref() == Some(c.ssid.as_str())
                || skip.as_deref() == Some(c.ssid.as_str())
                || blocked(c)
        };

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
            let mut th = if cur_c || relaxed { s.down_threshold } else { s.up_threshold };
            if !cur_c {
                th = th.max(s.min_connect_signal); // 连接任何新网络都要达到“最低连接信号”
            }
            c.signal >= th
        };

        order(&mut cands, policy, prefer24);
        let best_opt = cands.iter().find(|c| usable(c));

        // 诊断：排在当前网络前面的候选被挡住时，说明原因（同一原因 30 秒最多一条），免得用户看不出“为什么没切”
        let held = match best_opt {
            None => true,
            Some(b) => is_current(b),
        };
        if held && now - self.last_diag_at > 30_000 {
            let why = |c: &Cand| -> Option<String> {
                if is_bad(c) {
                    return Some("刚才连不上、断开或无法上网，等扫描确认它还在再重试".to_string());
                }
                let saved = !c.profile.is_empty()
                    && (st.profiles.contains(&c.profile) || st.profiles.contains(&c.ssid));
                if !saved && !is_current(c) {
                    return Some("没有已保存的配置文件".to_string());
                }
                let mut th = if relaxed { s.down_threshold } else { s.up_threshold };
                let mut kind = if relaxed { "回落阈值" } else { "升级阈值" };
                if !is_current(c) && s.min_connect_signal > th {
                    th = s.min_connect_signal;
                    kind = "最低连接信号";
                }
                if c.signal < th {
                    return Some(format!("信号 {}% 低于{} {}%", c.signal, kind, th));
                }
                None
            };
            let cur_pos = cands.iter().position(|c| is_current(c)).unwrap_or(cands.len());
            let blocked = cands.iter().take(cur_pos).find_map(|c| why(c).map(|w| (label(&c.ssid, c.band), w)));
            if let Some((lb, w)) = blocked {
                self.last_diag_at = now;
                d.logs.push(format!("优先网络 {} 暂不切换：{}", lb, w));
            }
        }

        // 诊断：“全局偏好频段优先”把当前网络排在了列表里更靠前的网络之前（例如当前是 5G，更靠前的网络只有 2.4G），
        // 这会让“出现更高优先级网络”永远不触发切换。这是策略的设计，但用户通常想不到，所以 5 分钟提示一次。
        if held && policy == "Global" && now - self.last_order_diag_at > 300_000 {
            if let Some(cur_c) = cands.iter().find(|c| is_current(c)) {
                let cur_rank = cur_c.rank;
                let cur_pos = cands.iter().position(|c| is_current(c)).unwrap_or(cands.len());
                if let Some(c) = cands.iter().skip(cur_pos + 1).find(|c| c.rank < cur_rank && usable(c)) {
                    self.last_order_diag_at = now;
                    d.logs.push(format!(
                        "优先网络 {} 信号正常，但“全局偏好频段优先”把当前网络排在了它前面，所以没有切换；想让列表顺序绝对优先，请在 设置 → 频段偏好 选“仅同一网络内”",
                        label(&c.ssid, c.band)
                    ));
                }
            }
        }

        let best = match best_opt {
            Some(b) if !is_current(b) => b,
            _ => {
                // 没有可切换的候选，或最优的就是当前网络。
                // 候选只是短暂掉出阈值（信号在阈值线上下抖动，≤1.5 秒）：保留确认计时，不要清零
                if self.pending_key.is_some() && now - self.pending_last_seen < 1500 {
                    return d;
                }
                self.pending_key = None;
                return d;
            }
        };
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
            self.pending_last_seen = now;
            let wait_confirm = now - self.pending_since < s.confirm_ms.max(0);
            let cooling = (cur_ok || !cur_listed) && now - self.last_switch < s.cooldown_sec.max(0) * 1000;
            if wait_confirm || cooling {
                return d;
            }
        }

        // 只有“同一个 WiFi 有多个频段、需要在频段之间选”时才指定接入点（BSSID）。
        // 换到另一个 WiFi 时不指定：BSS 列表是缓存，手机热点的接入点地址经常变化，指定缓存里的旧地址会连不上。
        let mut bands: Vec<i32> = cands.iter().filter(|c| c.ssid == best.ssid && c.band > 0).map(|c| c.band).collect();
        bands.sort();
        bands.dedup();
        let pin = policy != "Off" && best.band > 0 && bands.len() > 1 && (cur.is_none() || cur == Some(best.ssid.as_str()));
        let plan = Plan {
            ssid: best.ssid.clone(),
            band: if pin { best.band } else { 0 },
            iface: best.iface,
            profile: best.profile.clone(),
            bssid: if pin { best.bssid.clone() } else { None },
            reason,
            from_label: cur.map(|c| label(c, st.cur_band)),
            to_label: label(&best.ssid, best.band),
            retried: false,
        };
        self.pending_key = None;
        if cur.is_some() {
            self.last_switch = now; // 只有“从一个网络换到另一个”才进入升级冷却；从未连接状态连上不算
        }
        self.last_plan = Some(plan.clone());
        self.last_plan_at = now;
        self.expected = Some(plan.ssid.clone());
        d.connect = Some(plan);
        d
    }
}
