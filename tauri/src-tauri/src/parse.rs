//! 解析 `netsh wlan ...` 的输出（兼容模式 / 原生读取失败时的后备方案）。
//! 只依赖行结构、数字和 "GHz"，不依赖系统语言；BSSID 的标签不重要（Win11 24H2 叫 "AP BSSID"）。

use crate::model::*;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

const MAC: &str = r"([0-9A-Fa-f]{2}(?:[:\-][0-9A-Fa-f]{2}){5})";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

fn re_ssid_num() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, r"(?i)^\s*SSID\s+\d+\s*[:：]\s*(.*?)\s*$")
}
fn re_ssid_only() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, r"(?i)^\s*SSID\s*[:：]\s*(.*?)\s*$")
}
fn re_bssid_num() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, &format!(r"(?i)^\s*BSSID\s+\d+\s*[:：]\s*{}", MAC))
}
fn re_mac_line() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, &format!(r"[:：]\s*{}\s*$", MAC))
}
// 整行以 “: 85%” 结尾才算信号（排除 “Channel Utilization: 25 (9 %)”）
fn re_pct() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, r"[:：]\s*(\d{1,3})\s*%\s*$")
}
fn re_int() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, r"[:：]\s*(\d{1,3})\s*$")
}
fn re_ghz() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, r"(?i)(\d(?:\.\d)?)\s*GHz")
}

pub fn band_name(band: i32) -> &'static str {
    match band {
        6 => "6G",
        5 => "5G",
        2 => "2.4G",
        _ => "",
    }
}

fn band_from_ghz(s: &str) -> i32 {
    match s.parse::<f64>() {
        Ok(d) if d < 3.0 => 2,
        Ok(d) if d < 5.9 => 5,
        Ok(_) => 6,
        Err(_) => 0,
    }
}

fn norm_mac(m: &str) -> String {
    m.replace('-', ":").to_lowercase()
}

/// 重新计算 SSID 汇总信号与频段文字。
pub fn summarize(n: &mut NearbyNetwork) {
    n.signal = n.bss.iter().map(|b| b.signal).max().unwrap_or(0);
    let mut bands: Vec<i32> = n.bss.iter().filter(|b| b.band > 0).map(|b| b.band).collect();
    bands.sort_unstable();
    bands.dedup();
    bands.reverse(); // 6G, 5G, 2.4G
    n.bands = bands
        .iter()
        .map(|&bd| {
            let best = n.bss.iter().filter(|b| b.band == bd).map(|b| b.signal).max().unwrap_or(0);
            format!("{} {}%", band_name(bd), best)
        })
        .collect::<Vec<_>>()
        .join("  ·  ");
    n.band_tags = bands.iter().map(|&bd| band_name(bd)).collect::<Vec<_>>().join("/");
}

pub fn parse_state(if_out: &str, net_out: &str, pro_out: &str) -> WifiState {
    let mut st = WifiState::new();

    // 新版 Windows 11 需要开启“位置服务”才允许读取 WiFi 信息
    if if_out.contains("privacy-location") || net_out.contains("privacy-location") {
        st.location_denied = true;
    }

    // ---- 当前连接 ----
    let mut sig_read = false;
    let mut cur_chan: i32 = 0;
    for raw in if_out.split('\n') {
        let line = raw.trim_end_matches('\r');
        if !st.connected {
            if let Some(c) = re_ssid_only().captures(line) {
                let v = c[1].to_string();
                if !v.is_empty() {
                    st.ssid = v;
                    st.connected = true;
                }
            }
            continue;
        }
        if st.cur_bssid.is_none() {
            if let Some(c) = re_mac_line().captures(line) {
                st.cur_bssid = Some(norm_mac(&c[1]));
                continue;
            }
        }
        if !sig_read {
            if let Some(c) = re_pct().captures(line) {
                st.signal = c[1].parse().unwrap_or(0);
                sig_read = true;
                continue;
            }
        }
        if st.cur_band == 0 {
            if let Some(c) = re_ghz().captures(line) {
                st.cur_band = band_from_ghz(&c[1]);
                continue;
            }
        }
        if cur_chan == 0 {
            if let Some(c) = re_int().captures(line) {
                cur_chan = c[1].parse().unwrap_or(0);
            }
        }
    }
    if st.cur_band == 0 && cur_chan > 0 {
        st.cur_band = if cur_chan <= 14 { 2 } else { 5 };
    }

    // ---- 附近网络：SSID → BSSID ----
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, NearbyNetwork> = HashMap::new();
    let mut cur_ssid: Option<String> = None;
    let mut sig_set = false;
    for raw in net_out.split('\n') {
        let line = raw.trim_end_matches('\r');

        if let Some(c) = re_ssid_num().captures(line) {
            let name = c[1].to_string();
            if name.is_empty() {
                cur_ssid = None; // 隐藏网络
            } else {
                if !map.contains_key(&name) {
                    map.insert(name.clone(), NearbyNetwork { ssid: name.clone(), ..Default::default() });
                    order.push(name.clone());
                }
                cur_ssid = Some(name);
            }
            continue;
        }
        let ssid = match &cur_ssid {
            Some(s) => s.clone(),
            None => continue,
        };
        let net = map.get_mut(&ssid).unwrap();

        if let Some(c) = re_bssid_num().captures(line) {
            net.bss.push(BssInfo { bssid: norm_mac(&c[1]), iface: 0, ..Default::default() });
            sig_set = false;
            continue;
        }
        let bss = match net.bss.last_mut() {
            Some(b) => b,
            None => continue,
        };
        if !sig_set {
            if let Some(c) = re_pct().captures(line) {
                bss.signal = c[1].parse().unwrap_or(0);
                sig_set = true;
                continue;
            }
        }
        if let Some(c) = re_ghz().captures(line) {
            if bss.band == 0 {
                bss.band = band_from_ghz(&c[1]);
            }
            continue;
        }
        if sig_set && bss.chan == 0 {
            if let Some(c) = re_int().captures(line) {
                bss.chan = c[1].parse().unwrap_or(0);
            }
        }
    }

    for name in &order {
        let mut n = map.remove(name).unwrap();
        for b in n.bss.iter_mut() {
            if b.band == 0 && b.chan > 0 {
                b.band = if b.chan <= 14 { 2 } else { 5 }; // 老系统没有 Band 行时按信道推断
            }
        }
        summarize(&mut n);
        st.nearby.push(n);
    }

    // 用 BSSID 反查当前频段（比解析 show interfaces 更可靠）
    if st.connected {
        if let Some(cb) = &st.cur_bssid {
            'outer: for n in &st.nearby {
                for b in &n.bss {
                    if &b.bssid == cb && b.band > 0 {
                        st.cur_band = b.band;
                        break 'outer;
                    }
                }
            }
        }
    }

    // ---- 已保存的配置文件 ----
    for raw in pro_out.split('\n') {
        let t = raw.trim_end_matches('\r');
        if t.is_empty() || !t.chars().next().map(|c| c.is_whitespace()).unwrap_or(false) {
            continue; // 缩进行才是配置项
        }
        if let Some(i) = t.find(|c| c == ':' || c == '：') {
            let ch_len = t[i..].chars().next().unwrap().len_utf8();
            let v = t[i + ch_len..].trim();
            if !v.is_empty() {
                st.profiles.insert(v.to_string());
            }
        }
    }
    st
}
