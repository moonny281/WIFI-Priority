//! 识别 Windows BSS 缓存里“早已消失”的接入点。与 WPF 版的 BssFreshness 一一对应。
//!
//! 背景：WlanGetNetworkBssList 读的是缓存。热点关掉后，它的条目（连同当时的信号强度）会在缓存里留好一阵子，
//! 程序会以为它还在，于是一直尝试连接一个根本不存在的网络。
//!
//! 做法（不依赖时间戳的单位，只看“变没变”）：每个接入点条目都有 ullHostTimestamp（收到它最近一帧的时间）。
//! 接入点还在时，每次扫描完成它的时间戳都会更新；关掉后时间戳就不再变化。
//! 所以：最近两次扫描完成之后，时间戳都没变化的条目 = 已经不在了（stale）。
//!  - 启动时缓存里就有的条目先算“未确认”，等第一次扫描完成后时间戳变化了才确认在场；
//!  - 启动之后才第一次出现的条目一定是刚收到的帧，直接算在场（换了地址的热点会立刻被认出来）；
//!  - 驱动没给时间戳（0）时无法判断，按“在”处理；
//!  - 超过 15 秒收不到任何扫描完成通知，说明这个机制拿不到依据，放宽为“在”，避免永远不切换；
//!  - 当前连着的接入点一定在。

use crate::model::WifiState;
use std::collections::{HashMap, HashSet};

pub struct Freshness {
    start_at: i64,
    /// bssid(小写) -> (host timestamp, 我们观察到它变化的时间；0 = 启动时缓存里就有、还没观察到变化)
    ts: HashMap<String, (u64, i64)>,
    /// 最近 3 次扫描完成的时间（升序）
    done: Vec<i64>,
}

impl Freshness {
    pub fn new(now: i64) -> Freshness {
        Freshness { start_at: now, ts: HashMap::new(), done: Vec::new() }
    }

    pub fn scan_done(&mut self, now: i64) {
        self.done.push(now);
        if self.done.len() > 3 {
            self.done.remove(0);
        }
    }

    pub fn apply(&mut self, st: &mut WifiState, now: i64) {
        let cur = st.cur_bssid.as_ref().map(|c| c.to_ascii_lowercase());
        let mut seen: HashSet<String> = HashSet::new();
        for n in st.nearby.iter_mut() {
            for b in n.bss.iter_mut() {
                b.stale = false;
                if b.bssid.is_empty() {
                    continue;
                }
                let key = b.bssid.to_ascii_lowercase();
                seen.insert(key.clone());
                if b.host_ts == 0 {
                    continue; // 驱动没给时间戳：无法判断，按“在”处理
                }
                let startup = self.done.is_empty() && now - self.start_at < 5000;
                let e = self.ts.entry(key.clone()).or_insert((b.host_ts, if startup { 0 } else { now }));
                if e.0 != b.host_ts {
                    *e = (b.host_ts, now);
                }
                let changed = e.1;
                if cur.as_deref() == Some(key.as_str()) {
                    continue; // 当前连着的接入点一定在
                }
                b.stale = self.is_stale(changed, now);
            }
        }
        // 从列表里消失的条目：忘掉（以后再出现就是新条目，算在场）
        self.ts.retain(|k, _| seen.contains(k));
    }

    fn is_stale(&self, changed: i64, now: i64) -> bool {
        let n = self.done.len();
        if n == 0 {
            return changed == 0 && now - self.start_at <= 15_000;
        }
        // 最近两次扫描之前的那次完成时间（+ 观察延迟余量）
        let thr = if n >= 3 { self.done[0] + 300 } else { 0 };
        changed <= thr
    }
}
