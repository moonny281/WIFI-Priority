use crate::decide::*;
use crate::model::*;
use crate::parse::*;

fn st(cur: Option<(&str, i32, i64)>, bss: &[(&str, i32, i64)]) -> WifiState {
    let mut s = WifiState::new();
    s.fast = true;
    let mut k = 0;
    for (ssid, band, sig) in bss {
        let idx = match s.nearby.iter().position(|n| n.ssid == *ssid) {
            Some(i) => i,
            None => {
                s.nearby.push(NearbyNetwork { ssid: ssid.to_string(), ..Default::default() });
                s.profiles.insert(ssid.to_string());
                s.nearby.len() - 1
            }
        };
        s.nearby[idx].bss.push(BssInfo { bssid: format!("aa:bb:cc:dd:ee:{:02x}", k), band: *band, signal: *sig, ..Default::default() });
        k += 1;
    }
    for n in s.nearby.iter_mut() { summarize(n); }
    if let Some((ssid, band, sig)) = cur {
        s.connected = true;
        s.ssid = ssid.to_string();
        s.cur_band = band;
        s.signal = sig;
        let n = s.nearby.iter_mut().find(|n| n.ssid == ssid).unwrap();
        let b = n.bss.iter_mut().find(|b| b.band == band).unwrap();
        b.signal = sig;
        s.cur_bssid = Some(b.bssid.clone());
        summarize(n);
    }
    s
}

fn cfg(policy: &str, prefer: &str, confirm: i64, pri: &[&str]) -> Settings {
    let mut s = Settings::fresh();
    s.band_policy = policy.into();
    s.prefer_band = prefer.into();
    s.confirm_ms = confirm;
    s.cooldown_sec = 8;
    s.priorities = pri.iter().map(|x| x.to_string()).collect();
    s
}

#[test]
fn t1_global_5g_waits_confirm_then_switches_with_bssid() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 400, &["Home"]);
    let w = st(Some(("Home", 2, 70)), &[("Home", 2, 70), ("Home", 5, 80)]);
    assert!(d.decide(&w, &s, 1000, false).connect.is_none());
    let r = d.decide(&w, &s, 1450, false);
    let p = r.connect.expect("should switch");
    assert_eq!(p.band, 5);
    assert!(p.bssid.is_some());
    assert_eq!(p.ssid, "Home");
}

#[test]
fn t1c_confirm_zero_is_immediate() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["Home"]);
    let w = st(Some(("Home", 2, 70)), &[("Home", 2, 70), ("Home", 5, 80)]);
    assert!(d.decide(&w, &s, 1000, false).connect.is_some());
}

#[test]
fn t2_disconnected_connects_immediately_with_relaxed_threshold() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 400, &["Home"]);
    let w = st(None, &[("Home", 2, 45), ("Home", 5, 42)]);
    // v1.1.3：刚断开的 1.2 秒内先等一下（可能是用户在手动换网），之后才发起连接
    assert!(d.decide(&w, &s, 1000, false).connect.is_none());
    let p = d.decide(&w, &s, 2300, false).connect.expect("connect");
    assert_eq!(p.band, 5);
    assert!(p.from_label.is_none());
}

#[test]
fn t3_fallback_to_2g_when_5g_drops() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 400, &["Home"]);
    let w = st(Some(("Home", 5, 30)), &[("Home", 2, 55), ("Home", 5, 30)]);
    d.decide(&w, &s, 1000, false);
    let p = d.decide(&w, &s, 1500, false).connect.expect("fallback");
    assert_eq!(p.band, 2);
    assert!(p.reason.contains("回落"));
}

#[test]
fn t4_policies() {
    let w = st(Some(("Home", 5, 80)), &[("Home", 5, 80), ("Office", 2, 80)]);
    let mut d1 = Decider::new();
    assert_eq!(d1.decide(&w, &cfg("Same", "5G", 0, &["Office", "Home"]), 1000, false).connect.map(|p| p.ssid), Some("Office".to_string()));
    let mut d2 = Decider::new();
    assert!(d2.decide(&w, &cfg("Global", "5G", 0, &["Office", "Home"]), 1000, false).connect.is_none());
    let mut d3 = Decider::new();
    assert_eq!(d3.decide(&w, &cfg("Off", "5G", 0, &["Office", "Home"]), 1000, false).connect.map(|p| p.ssid), Some("Office".to_string()));
}

#[test]
fn t5_hysteresis_keeps_5g_at_50() {
    let mut d = Decider::new();
    let w = st(Some(("Home", 5, 50)), &[("Home", 2, 80), ("Home", 5, 50)]);
    assert!(d.decide(&w, &cfg("Global", "5G", 0, &["Home"]), 1000, false).connect.is_none());
}

#[test]
fn t6_up_threshold() {
    let mut d = Decider::new();
    let w = st(Some(("Home", 2, 70)), &[("Home", 2, 70), ("Home", 5, 55)]);
    assert!(d.decide(&w, &cfg("Global", "5G", 0, &["Home"]), 1000, false).connect.is_none());
}

#[test]
fn t7_no_duplicate_requests_and_failure_marks_bad() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    // v1.1.3：换到另一个 WiFi 时不指定接入点（BSSID）——缓存里的接入点地址可能已经变了（手机热点常见）
    let p = d.decide(&w, &s, 1000, false).connect.expect("switch to A");
    assert_eq!((p.ssid.as_str(), p.bssid.is_none(), p.band), ("A", true, 0));
    assert!(d.decide(&w, &s, 1300, false).connect.is_none());
    let r = d.decide(&w, &s, 5200, false);
    assert!(r.connect.is_none());
    assert!(r.logs.iter().any(|l| l.contains("没有成功")));
    assert!(d.decide(&w, &s, 6000, false).connect.is_none());
}

#[test]
fn t7b_pinned_bssid_failure_retries_without_bssid() {
    // 同一个 WiFi 在 2.4G/5G 之间选频段时才指定接入点；没连上 → 先不指定接入点重试一次
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["Home"]);
    let w = st(Some(("Home", 2, 70)), &[("Home", 2, 70), ("Home", 5, 80)]);
    let p = d.decide(&w, &s, 1000, false).connect.expect("band up");
    assert_eq!((p.band, p.bssid.is_some()), (5, true));
    let dead = st(None, &[("Home", 2, 70), ("Home", 5, 80)]); // 连接掉了
    assert!(d.decide(&dead, &s, 1300, false).connect.is_none());
    let retry = d.decide(&dead, &s, 5200, false).connect.expect("retry without bssid");
    assert_eq!((retry.ssid.as_str(), retry.bssid.is_none(), retry.retried), ("Home", true, true));
}

#[test]
fn t20_waits_after_disconnect_before_taking_over() {
    // 用户在 Windows 里手动换网：先断开（<1.2 秒内不抢着连），随后进入“正在连接”也不打断
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let off = st(None, &[("A", 2, 80), ("B", 2, 70)]);
    assert!(d.decide(&off, &s, 1000, false).connect.is_none());
    assert!(d.decide(&off, &s, 1100, false).connect.is_none());
    let mut conn = st(None, &[("A", 2, 80), ("B", 2, 70)]);
    conn.connecting = true;
    assert!(d.decide(&conn, &s, 1500, false).connect.is_none());
    assert!(d.decide(&conn, &s, 5000, false).connect.is_none());
}

#[test]
fn t8_band_not_accepted_is_blacklisted() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["Home"]);
    let w = st(Some(("Home", 2, 70)), &[("Home", 2, 70), ("Home", 5, 80)]);
    assert_eq!(d.decide(&w, &s, 1000, false).connect.map(|p| p.band), Some(5));
    assert!(d.decide(&w, &s, 2000, false).connect.is_none());
    let r = d.decide(&w, &s, 5500, false);
    assert!(r.logs.iter().any(|l| l.contains("未能切换到")));
    assert!(d.decide(&w, &s, 9000, false).connect.is_none());
}

#[test]
fn t9_manual_change_pauses() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["Home"]);
    d.decide(&st(Some(("Home", 5, 80)), &[("Home", 5, 80), ("Other", 5, 90)]), &s, 1000, false);
    let r = d.decide(&st(Some(("Other", 5, 90)), &[("Home", 5, 80), ("Other", 5, 90)]), &s, 2000, false);
    assert_eq!(r.pause_minutes, s.manual_pause_minutes);
    assert!(r.connect.is_none());
}

#[test]
fn t10_takeover_unlisted() {
    let mut d = Decider::new();
    let mut s = cfg("Global", "5G", 0, &["Home"]);
    s.takeover_unlisted = false;
    let w = st(Some(("Phone", 5, 90)), &[("Phone", 5, 90), ("Home", 5, 80)]);
    assert!(d.decide(&w, &s, 1000, false).connect.is_none());
    s.takeover_unlisted = true;
    assert_eq!(d.decide(&w, &s, 1000, false).connect.map(|p| p.ssid), Some("Home".to_string()));
}

#[test]
fn t11_prefer_24g() {
    let mut d = Decider::new();
    let w = st(Some(("Home", 5, 80)), &[("Home", 2, 80), ("Home", 5, 80)]);
    assert_eq!(d.decide(&w, &cfg("Global", "2.4G", 0, &["Home"]), 1000, false).connect.map(|p| p.band), Some(2));
}

#[test]
fn t12_unsaved_network_ignored() {
    let mut d = Decider::new();
    let mut w = st(Some(("B", 2, 70)), &[("A", 5, 90), ("B", 2, 70)]);
    w.profiles.remove("A");
    assert!(d.decide(&w, &cfg("Global", "5G", 0, &["A", "B"]), 1000, false).connect.is_none());
}

#[test]
fn t13_system_roam_is_not_manual_change() {
    // 原网络信号已低于回落阈值，Windows 自己漫游到别的网络：不应暂停自动切换
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["Home"]);
    d.decide(&st(Some(("Home", 5, 30)), &[("Home", 5, 30), ("Other", 5, 90)]), &s, 1000, false);
    let r = d.decide(&st(Some(("Other", 5, 90)), &[("Home", 5, 30), ("Other", 5, 90)]), &s, 2000, false);
    assert_eq!(r.pause_minutes, 0);
    assert!(r.logs.iter().any(|l| l.contains("系统自行切换")));
}

#[test]
fn t14_candidate_flicker_keeps_confirm_timer() {
    // 候选信号在升级阈值上下抖动：短暂掉出阈值（<1.5 秒）不应清零确认计时
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 400, &["A", "B"]);
    let up = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    let dip = st(Some(("B", 2, 70)), &[("A", 2, 55), ("B", 2, 70)]);
    assert!(d.decide(&up, &s, 1000, false).connect.is_none()); // 开始计时
    assert!(d.decide(&dip, &s, 1200, false).connect.is_none()); // 抖了一下
    let p = d.decide(&up, &s, 1500, false).connect; // 距离首次最优已 500ms
    assert_eq!(p.map(|p| p.ssid), Some("A".to_string()));
}

#[test]
fn t15_first_connect_does_not_start_upgrade_cooldown() {
    // 从未连接状态连上（不算“升级”），紧接着出现更高优先级网络，不应被 8 秒冷却拦住
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let off = st(None, &[("B", 2, 70)]);
    assert!(d.decide(&off, &s, 1000, false).connect.is_none()); // 刚断开，先等 1.2 秒
    let p = d.decide(&off, &s, 2300, false).connect.expect("connect B");
    assert_eq!(p.ssid, "B");
    d.connect_result(&p, true, 2300);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    assert_eq!(d.decide(&w, &s, 2600, false).connect.map(|p| p.ssid), Some("A".to_string()));
}

#[test]
fn t16_blocked_by_threshold_is_explained() {
    // 更高优先级网络信号不够升级阈值：不切换，但日志里要说明原因
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 50), ("B", 2, 70)]);
    let r = d.decide(&w, &s, 1000, false);
    assert!(r.connect.is_none());
    assert!(r.logs.iter().any(|l| l.contains("暂不切换") && l.contains("升级阈值")));
    // 同一原因 30 秒内不重复刷屏
    assert!(d.decide(&w, &s, 2000, false).logs.is_empty());
}

#[test]
fn t17_failed_request_does_not_consume_cooldown() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    let p = d.decide(&w, &s, 1000, false).connect.expect("switch to A");
    d.connect_result(&p, false, 1000); // 系统没接受请求
    // 清掉拉黑标记模拟“稍后重试”，冷却不应还在占用
    d.mark_bad("A", 0);
    assert!(d.decide(&w, &s, 1100, false).connect.is_some());
}

#[test]
fn t18_active_target_reports_pending_switch() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    assert!(d.active_target().is_none());
    d.decide(&w, &s, 1000, false);
    assert_eq!(d.active_target().as_deref(), Some("A（2.4G）"));
}

#[test]
fn t19_global_policy_blocks_upgrade_but_same_policy_switches() {
    // 当前连着列表靠后的 5G 网络 B，列表靠前的 A 只有 2.4G：
    // “全局偏好频段优先”下不会升级（而且会提示原因），“仅同一网络内”下立刻升级
    let w = st(Some(("B", 5, 70)), &[("A", 2, 80), ("B", 5, 70)]);
    let mut d = Decider::new();
    let r = d.decide(&w, &cfg("Global", "5G", 0, &["A", "B"]), 1000, false);
    assert!(r.connect.is_none());
    assert!(r.logs.iter().any(|l| l.contains("全局偏好频段优先")));

    let mut d = Decider::new();
    let p = d.decide(&w, &cfg("Same", "5G", 0, &["A", "B"]), 1000, false).connect.expect("upgrade to A");
    assert_eq!(p.ssid, "A");
    assert!(p.reason.contains("更高优先级"));
}

#[test]
fn s6_band_policy_migration_global_to_same() {
    // 旧配置（schema 2）里的 Global → Same，同时不重置用户自己调过的扫描/确认/冷却参数
    let s = Settings::from_json(r#"{"SchemaVersion":2,"BandPolicy":"Global","ScanIntervalSec":7,"ConfirmMs":900,"CooldownSec":20}"#).unwrap();
    assert_eq!(s.band_policy, "Same");
    assert_eq!((s.scan_interval_sec, s.confirm_ms, s.cooldown_sec), (7, 900, 20));
    assert_eq!(s.schema_version, CURRENT_SCHEMA);
    // 已是最新版本的配置：用户自己选的 Global 不会再被改
    let s = Settings::from_json(r#"{"SchemaVersion":3,"BandPolicy":"Global"}"#).unwrap();
    assert_eq!(s.band_policy, "Global");
    // 全新安装默认 Same
    assert_eq!(Settings::fresh().band_policy, "Same");
}

#[test]
fn s5_overlay_layer_default_and_sanitize() {
    let mut s = Settings::fresh();
    assert_eq!(s.overlay_layer, "Top");
    s.overlay_layer = "weird".into();
    s.sanitize();
    assert_eq!(s.overlay_layer, "Top");
    // 旧配置文件没有这个字段 → 取默认值
    let old = Settings::from_json(r#"{"SchemaVersion":2,"Priorities":["Home"]}"#).unwrap();
    assert_eq!(old.overlay_layer, "Top");
}

// ---------------- netsh 解析 ----------------
const IF_EN: &str = "There is 1 interface on the system:\r\n\r\n    Name                   : Wi-Fi\r\n    Description            : Intel(R) Wi-Fi 6 AX201 160MHz\r\n    GUID                   : 11111111-2222-3333-4444-555555555555\r\n    Physical address       : 00:11:22:33:44:55\r\n    State                  : connected\r\n    SSID                   : Home\r\n    BSSID                  : aa:bb:cc:dd:ee:12\r\n    Network type           : Infrastructure\r\n    Radio type             : 802.11ax\r\n    Authentication         : WPA2-Personal\r\n    Cipher                 : CCMP\r\n    Connection mode        : Auto Connect\r\n    Band                   : 5 GHz\r\n    Channel                : 36\r\n    Receive rate (Mbps)    : 866.7\r\n    Transmit rate (Mbps)   : 866.7\r\n    Signal                 : 91%\r\n    Profile                : Home\r\n";
const NET_EN: &str = "Interface name : Wi-Fi \r\nThere are 3 networks currently visible. \r\n\r\nSSID 1 : Home \r\n    Network type            : Infrastructure\r\n    Authentication          : WPA2-Personal\r\n    Encryption              : CCMP \r\n    BSSID 1                 : aa:bb:cc:dd:ee:12\r\n         Signal             : 91%  \r\n         Radio type         : 802.11ax\r\n         Band               : 5 GHz  \r\n         Channel            : 36 \r\n         Basic rates (Mbps) : 6 12 24\r\n         Other rates (Mbps) : 9 18 36 48 54\r\n         BSS Load:\r\n             Connected Stations:         3\r\n             Channel Utilization:        25 (9 %)\r\n    BSSID 2                 : aa:bb:cc:dd:ee:13\r\n         Signal             : 60%  \r\n         Radio type         : 802.11n\r\n         Band               : 2.4 GHz  \r\n         Channel            : 6 \r\n\r\nSSID 2 : Cafe_Guest \r\n    Network type            : Infrastructure\r\n    BSSID 1                 : 12:34:56:78:9a:bc\r\n         Signal             : 40%  \r\n         Radio type         : 802.11n\r\n         Channel            : 1 \r\n\r\nSSID 3 : \r\n    Network type            : Infrastructure\r\n    BSSID 1                 : 12:34:56:78:9a:bd\r\n         Signal             : 99%  \r\n         Channel            : 44 \r\n";
const PRO_EN: &str = "Profiles on interface Wi-Fi:\r\n\r\nGroup policy profiles (read only)\r\n---------------------------------\r\n    <None>\r\n\r\nUser profiles\r\n-------------\r\n    All User Profile     : Home\r\n    All User Profile     : Cafe_Guest\r\n";
const IF_CN: &str = "系统上有 1 个接口:\r\n\r\n    名称                   : WLAN\r\n    描述                   : Realtek 8822CE\r\n    GUID                   : 11111111-2222-3333-4444-555555555555\r\n    物理地址               : 00:11:22:33:44:55\r\n    状态                   : 已连接\r\n    SSID                   : 家里WiFi\r\n    AP BSSID               : aa:bb:cc:dd:ee:34\r\n    网络类型               : 结构\r\n    无线电类型             : 802.11ac\r\n    身份验证               : WPA2-个人\r\n    密码                   : CCMP\r\n    连接模式               : 自动连接\r\n    频带                   : 2.4 GHz\r\n    信道                   : 11\r\n    接收速率(Mbps)         : 144.4\r\n    传输速率(Mbps)         : 144.4\r\n    信号                   : 77%\r\n    配置文件               : 家里WiFi\r\n";

#[test]
fn p1_english_interface() {
    let p = parse_state(IF_EN, NET_EN, PRO_EN);
    assert!(p.connected && p.ssid == "Home" && p.signal == 91 && p.cur_band == 5);
    assert_eq!(p.cur_bssid.as_deref(), Some("aa:bb:cc:dd:ee:12"));
}

#[test]
fn p2_ap_bssid_label_win11_24h2() {
    let w = IF_EN.replace("BSSID                  :", "AP BSSID               :");
    let p = parse_state(&w, NET_EN, PRO_EN);
    assert!(p.connected && p.signal == 91 && p.cur_band == 5);
    assert_eq!(p.cur_bssid.as_deref(), Some("aa:bb:cc:dd:ee:12"));
}

#[test]
fn p3_band_inferred_from_channel() {
    let w = IF_EN.replace("    Band                   : 5 GHz\r\n", "").replace("Channel                : 36", "Channel                : 6");
    let p = parse_state(&w, "", PRO_EN);
    assert!(p.signal == 91 && p.cur_band == 2);
}

#[test]
fn p4_chinese_interface() {
    let p = parse_state(IF_CN, NET_EN, PRO_EN);
    assert!(p.ssid == "家里WiFi" && p.signal == 77 && p.cur_band == 2);
    assert_eq!(p.cur_bssid.as_deref(), Some("aa:bb:cc:dd:ee:34"));
}

#[test]
fn p5_per_band_signals_ignore_utilization() {
    let p = parse_state(IF_EN, NET_EN, PRO_EN);
    let home = p.nearby.iter().find(|n| n.ssid == "Home").unwrap();
    assert_eq!(home.bss.len(), 2);
    assert_eq!(home.signal, 91);
    assert_eq!(home.bands, "5G 91%  ·  2.4G 60%");
    assert_eq!(home.band_tags, "5G/2.4G");
}

#[test]
fn p6_hidden_ssid_skipped_band_from_channel() {
    let p = parse_state(IF_EN, NET_EN, PRO_EN);
    assert_eq!(p.nearby.len(), 2);
    assert_eq!(p.nearby.iter().find(|n| n.ssid == "Cafe_Guest").unwrap().bss[0].band, 2);
}

#[test]
fn p7_profiles() {
    let p = parse_state(IF_EN, NET_EN, PRO_EN);
    assert!(p.profiles.contains("Home") && p.profiles.contains("Cafe_Guest") && p.profiles.len() == 2);
}

#[test]
fn p8_location_denied() {
    let p = parse_state("Network shell commands need location permission to access WLAN information. ms-settings:privacy-location", "", "");
    assert!(p.location_denied && !p.connected);
}

// ---------------- 设置 ----------------
#[test]
fn s1_old_settings_migrate_and_keep_user_choices() {
    let json = r#"{"ScanIntervalSec":10,"ConfirmCount":2,"CooldownSec":60,"Priorities":["Home"],"Theme":"Dark","UpThreshold":65}"#;
    let s = Settings::from_json(json).unwrap();
    assert_eq!((s.scan_interval_sec, s.confirm_ms, s.cooldown_sec), (3, 400, 8));
    assert_eq!(s.theme, "Dark");
    assert_eq!(s.up_threshold, 65);
    assert_eq!(s.priorities, vec!["Home".to_string()]);
    assert_eq!(s.schema_version, CURRENT_SCHEMA);
}

#[test]
fn s2_current_schema_is_not_overwritten() {
    let json = r#"{"SchemaVersion":2,"ScanIntervalSec":7,"ConfirmMs":900,"CooldownSec":20}"#;
    let s = Settings::from_json(json).unwrap();
    assert_eq!((s.scan_interval_sec, s.confirm_ms, s.cooldown_sec), (7, 900, 20));
}

#[test]
fn s3_roundtrip_uses_pascal_case_like_wpf() {
    let s = Settings::fresh();
    let j = serde_json::to_string(&s).unwrap();
    for k in ["\"ScanIntervalSec\"", "\"ConfirmMs\"", "\"BandPolicy\"", "\"OverlayX\"", "\"SchemaVersion\"", "\"TakeoverUnlisted\""] {
        assert!(j.contains(k), "missing {}", k);
    }
}

#[test]
fn s4_sanitize() {
    let mut s = Settings::fresh();
    s.up_threshold = 50; s.down_threshold = 80; s.band_policy = "x".into(); s.overlay_opacity = 5.0; s.scan_interval_sec = -3;
    s.sanitize();
    assert_eq!((s.up_threshold, s.down_threshold, s.scan_interval_sec), (50, 50, 0));
    assert_eq!(s.band_policy, "Same"); // 非法值回落到“仅同一网络内”（列表顺序绝对优先）
    assert_eq!(s.overlay_opacity, 1.0);
}
