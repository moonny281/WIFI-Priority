use crate::decide::*;
use crate::freshness::Freshness;
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
        s.nearby[idx].bss.push(BssInfo { bssid: format!("aa:bb:cc:dd:ee:{:02x}", k), band: *band, signal: *sig, changed_at: i64::MAX / 2, ..Default::default() });
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
    s.min_connect_signal = 0; // 旧用例不涉及“最低连接信号”，新用例自己设置
    s.disc_grace_ms = 1200;   // 旧用例按 1200 ms 的让路时间写的；新默认值 400 ms 见 s7
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
fn t7_no_duplicate_requests_and_no_failure_cooldown() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    // 换到另一个 WiFi 时不指定接入点（BSSID）——缓存里的接入点地址可能已经变了（手机热点常见）
    let p = d.decide(&w, &s, 1000, false).connect.expect("switch to A");
    assert_eq!((p.ssid.as_str(), p.bssid.is_none(), p.band), ("A", true, 0));
    assert!(d.decide(&w, &s, 1300, false).connect.is_none()); // 请求在途，不重复发
    let r = d.decide(&w, &s, 5200, false); // 4 秒没落定 → 记录失败
    assert!(r.connect.is_none());
    assert!(r.logs.iter().any(|l| l.contains("没有成功")));
    // v1.1.5：没有失败冷却，只受升级冷却（默认 8 秒，从上次发起切换算起）约束，冷却一过就再次尝试
    assert!(d.decide(&w, &s, 6000, false).connect.is_none());
    assert_eq!(d.decide(&w, &s, 9500, false).connect.map(|p| p.ssid), Some("A".to_string()));
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
fn t17_failed_request_is_rate_limited_but_not_a_cooldown() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    let p = d.decide(&w, &s, 1000, false).connect.expect("switch to A");
    d.connect_result(&p, false, 1000); // 系统直接拒绝了请求
    // 只隔 3 秒防止每 250 ms 空转；不拉黑网络，也不占用升级冷却
    assert!(d.decide(&w, &s, 1100, false).connect.is_none());
    assert!(d.decide(&w, &s, 3900, false).connect.is_none());
    assert!(d.decide(&w, &s, 4100, false).connect.is_some());
}

#[test]
fn t21_no_internet_falls_back_then_retries_without_ban() {
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    let on_hot = st(Some(("Hotspot", 2, 80)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&on_hot, &s, 1000, false).connect.is_none());
    // 联网检测报告：热点连上了但无法上网 → 立即回退到下一个网络
    d.report_no_internet("Hotspot");
    let p = d.decide(&on_hot, &s, 1500, false).connect.expect("fall back");
    assert_eq!(p.ssid, "Home");
    // 回到 Home 之后热点不被拉黑：升级冷却（默认 8 秒）一过就再次连接
    let on_home = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&on_home, &s, 2000, false).connect.is_none());
    assert_eq!(d.decide(&on_home, &s, 9600, false).connect.map(|p| p.ssid), Some("Hotspot".to_string()));
}

#[test]
fn t22_stale_cached_hotspot_is_not_a_target() {
    // 热点已经关了，但 Windows 缓存里还留着它的旧条目（信号还很高）：不能把它当目标，更不能一直重试
    let mut w = st(Some(("B", 2, 70)), &[("A", 2, 90), ("B", 2, 70)]);
    for b in w.nearby.iter_mut().find(|n| n.ssid == "A").unwrap().bss.iter_mut() {
        b.stale = true;
    }
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let mut d = Decider::new();
    assert!(d.decide(&w, &s, 1000, false).connect.is_none());
    // 关掉“忽略已消失的网络”后恢复旧行为
    let mut s2 = cfg("Global", "5G", 0, &["A", "B"]);
    s2.stale_filter = false;
    let mut d = Decider::new();
    assert_eq!(d.decide(&w, &s2, 1000, false).connect.map(|p| p.ssid), Some("A".to_string()));
}

#[test]
fn t23_min_connect_signal_gates_every_new_connection() {
    let w = st(Some(("B", 2, 70)), &[("A", 2, 65), ("B", 2, 70)]);
    // 最低连接信号 70：A 只有 65%，不连（即使满足升级阈值 60%），并且日志说明原因
    let mut s = cfg("Global", "5G", 0, &["A", "B"]);
    s.min_connect_signal = 70;
    let mut d = Decider::new();
    let r = d.decide(&w, &s, 1000, false);
    assert!(r.connect.is_none());
    assert!(r.logs.iter().any(|l| l.contains("最低连接信号")));
    // 调到 50：A 的 65% 够了，立即切
    s.min_connect_signal = 50;
    let mut d = Decider::new();
    assert_eq!(d.decide(&w, &s, 1000, false).connect.map(|p| p.ssid), Some("A".to_string()));
    // 未连接 + 回落场景同样受限：唯一的候选信号 45%，最低连接信号 50 → 不连；设为 0 → 连
    let off = st(None, &[("B", 2, 45)]);
    let mut d = Decider::new();
    assert!(d.decide(&off, &s, 1000, false).connect.is_none());
    assert!(d.decide(&off, &s, 2300, false).connect.is_none());
    s.min_connect_signal = 0;
    let mut d = Decider::new();
    assert!(d.decide(&off, &s, 1000, false).connect.is_none()); // 刚断开的 1.2 秒内先等
    assert!(d.decide(&off, &s, 2300, false).connect.is_some());
}

#[test]
fn f1_ghost_entry_is_flagged_after_scans() {
    let mut f = Freshness::new(0);
    // 启动时缓存里就有的条目：未确认
    let mut s = ghost_state(100);
    f.apply(&mut s, 100);
    assert!(s.nearby[0].bss[0].stale);
    assert_eq!(s.nearby[0].bss[0].changed_at, 0);
    // 扫描完成，时间戳变了 → 确认在场
    f.scan_done(3000);
    let mut s = ghost_state(200);
    f.apply(&mut s, 3100);
    assert!(!s.nearby[0].bss[0].stale);
    assert_eq!(s.nearby[0].bss[0].changed_at, 3100);
    // 热点关了：时间戳不再变化。再完成两次扫描 → 判为已消失
    f.scan_done(6000);
    f.scan_done(9000);
    let mut s = ghost_state(200);
    f.apply(&mut s, 9100);
    assert!(s.nearby[0].bss[0].stale);
    // 热点又开了：时间戳变化 → 立刻恢复在场
    let mut s = ghost_state(300);
    f.apply(&mut s, 9200);
    assert!(!s.nearby[0].bss[0].stale);
}

#[test]
fn f2_new_entry_after_scan_is_fresh_and_no_scan_notification_relaxes() {
    // 启动之后才第一次出现的条目一定是刚收到的帧，算在场
    let mut f = Freshness::new(0);
    f.scan_done(1000);
    let mut s = ghost_state(500);
    f.apply(&mut s, 2000);
    assert!(!s.nearby[0].bss[0].stale);
    // 一直收不到“扫描完成”通知：15 秒内先当未确认，之后放宽（避免永远不切换）
    let mut g = Freshness::new(0);
    let mut s = ghost_state(5);
    g.apply(&mut s, 100);
    assert!(s.nearby[0].bss[0].stale);
    let mut s = ghost_state(5);
    g.apply(&mut s, 16_000);
    assert!(!s.nearby[0].bss[0].stale);
}

#[test]
fn f3_current_bssid_and_missing_timestamp_are_never_stale() {
    let mut f = Freshness::new(0);
    let mut s = ghost_state(100);
    s.cur_bssid = Some("AA:BB:CC:DD:EE:01".to_string()); // 大小写不同也要认出来
    f.apply(&mut s, 100);
    assert!(!s.nearby[0].bss[0].stale);
    let mut s = ghost_state(0); // 驱动没给时间戳
    f.apply(&mut s, 200);
    assert!(!s.nearby[0].bss[0].stale);
    assert_eq!(s.nearby[0].bss[0].changed_at, -1);
}

#[test]
fn t24_offline_failure_tries_next_candidate_instead_of_hammering() {
    // 断网状态下：热点（最高优先级）连失败后，下一轮先试第二优先级，不是原地死磕
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    let off = st(None, &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&off, &s, 1000, false).connect.is_none()); // 刚断开，先等 1.2 秒
    let p = d.decide(&off, &s, 2300, false).connect.expect("try hotspot");
    assert_eq!(p.ssid, "Hotspot");
    let r = d.decide(&off, &s, 6500, false); // 4 秒还是没连上（仍未连接）
    assert!(r.logs.iter().any(|l| l.contains("没有成功")));
    assert_eq!(r.connect.map(|p| p.ssid), Some("Home".to_string()));
}

#[test]
fn t25_unplanned_disconnect_requires_fresh_evidence_before_reconnect() {
    // 连着热点时热点被关掉：断开后缓存里还留着它的旧条目（信号还是 80，时间戳不再更新）。
    // 不能立刻去重连它，要先连第二优先级；等扫描证明热点又出现了（条目更新了）才允许再连。
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    let on = st(Some(("Hotspot", 2, 80)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&on, &s, 500, false).connect.is_none());
    let mut off = st(None, &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    set_changed(&mut off, "Hotspot", 500); // 热点最后一次更新是断开之前
    assert!(d.decide(&off, &s, 1000, false).connect.is_none()); // 刚断开，先等 1.2 秒
    let p = d.decide(&off, &s, 2300, false).connect.expect("fall to Home");
    assert_eq!(p.ssid, "Home");
    // 连上 Home 之后，热点还是只有旧条目：不理它
    let mut on_home = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    set_changed(&mut on_home, "Hotspot", 500);
    assert!(d.decide(&on_home, &s, 2600, false).connect.is_none());
    // 热点其实又开了：被扫描到、条目更新（时间在断开之后）→ 证据到了，立即升级
    set_changed(&mut on_home, "Hotspot", 3000);
    assert_eq!(d.decide(&on_home, &s, 2800, false).connect.map(|p| p.ssid), Some("Hotspot".to_string()));
}

#[test]
fn t26_failed_attempt_is_not_retried_without_new_evidence() {
    // 升级到热点失败（热点其实已经不在）：回到原网络后，不再反复折腾，直到热点又被扫描到
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    let mut w = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    set_changed(&mut w, "Hotspot", 900);
    let p = d.decide(&w, &s, 1000, false).connect.expect("try hotspot");
    assert_eq!(p.ssid, "Hotspot");
    // 4 秒后还连着 Home（没连上热点）→ 记失败，之后不再重试
    let r = d.decide(&w, &s, 5200, false);
    assert!(r.logs.iter().any(|l| l.contains("没有成功")));
    assert!(r.connect.is_none());
    for t in [6000, 9500, 20_000, 60_000] {
        assert!(d.decide(&w, &s, t, false).connect.is_none(), "t={}", t);
    }
    // 热点又被扫描到（条目在失败之后更新过）→ 允许再试
    set_changed(&mut w, "Hotspot", 61_000);
    assert_eq!(d.decide(&w, &s, 62_000, false).connect.map(|p| p.ssid), Some("Hotspot".to_string()));
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
fn s7_min_connect_signal_and_stale_filter_defaults() {
    let mut s = Settings::fresh();
    assert_eq!((s.min_connect_signal, s.stale_filter), (50, true));
    s.min_connect_signal = 500;
    s.sanitize();
    assert_eq!(s.min_connect_signal, 100);
    // 旧配置文件没有这两个字段 → 取默认值
    let old = Settings::from_json(r#"{"SchemaVersion":3,"Priorities":["Home"]}"#).unwrap();
    assert_eq!((old.min_connect_signal, old.stale_filter), (50, true));
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

fn set_changed(w: &mut WifiState, ssid: &str, at: i64) {
    for b in w.nearby.iter_mut().find(|n| n.ssid == ssid).unwrap().bss.iter_mut() {
        b.changed_at = at;
    }
}

fn ghost_state(ts: u64) -> WifiState {
    let mut s = WifiState::new();
    s.nearby.push(NearbyNetwork {
        ssid: "Hot".to_string(),
        bss: vec![BssInfo { bssid: "aa:bb:cc:dd:ee:01".to_string(), host_ts: ts, band: 2, signal: 80, ..Default::default() }],
        ..Default::default()
    });
    s
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

// ---------------- v1.1.8：链路监视 / 快速失败 / 退避 ----------------
use crate::link::{self, Hw, IfSnapshot, LinkState, Verdict};
use std::cell::Cell;
use std::net::Ipv4Addr;
use std::time::Duration;

struct FakeHw {
    snap: Cell<IfSnapshot>,
    gw: Cell<Option<Ipv4Addr>>,
    ping_ok: Cell<bool>,
    wan: Cell<bool>,
}

impl Hw for FakeHw {
    fn read_if(&self, _g: &[u8; 16]) -> Option<IfSnapshot> {
        Some(self.snap.get())
    }
    fn gateway(&self, _i: u32) -> Option<Ipv4Addr> {
        self.gw.get()
    }
    fn ping(&self, _ip: Ipv4Addr, _ms: u32) -> bool {
        self.ping_ok.get()
    }
    fn wan_ok(&self, _i: u32, _t: Duration) -> bool {
        self.wan.get()
    }
}

fn fake() -> FakeHw {
    FakeHw {
        snap: Cell::new(IfSnapshot { if_index: 7, oper_up: true, media_up: true, in_octets: 1000, out_octets: 1000 }),
        gw: Cell::new(Some(Ipv4Addr::new(192, 168, 1, 1))),
        ping_ok: Cell::new(true),
        wan: Cell::new(true),
    }
}

const G: [u8; 16] = [0; 16];

#[test]
fn l1_media_down_alone_is_not_a_verdict_until_it_persists() {
    // 我们自己切换网络时旧连接必然短暂掉线：链路断开不能一出现就判死（否则会把刚离开的好网络写进退避）
    let hw = fake();
    let s = Settings::fresh();
    let mut st = LinkState::new("X", 0, 0);
    let mut sn = hw.snap.get();
    sn.media_up = false;
    hw.snap.set(sn);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 100, false), Verdict::Pending);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 1500, false), Verdict::Pending);
    // 持续 2 秒以上、引擎仍认为连着同一个网络：才算异常
    assert!(matches!(link::evaluate(&hw, &mut st, &G, &s, 2200, false), Verdict::LinkDead(_)));
    // 链路恢复：计时清零
    let mut sn = hw.snap.get();
    sn.media_up = true;
    hw.snap.set(sn);
    assert!(!matches!(link::evaluate(&hw, &mut st, &G, &s, 2400, false), Verdict::LinkDead(_)));
}

#[test]
fn l2_dhcp_timeout_replaces_fixed_12s_wait() {
    let hw = fake();
    hw.gw.set(None);
    let s = Settings::fresh(); // ip_wait_ms = 8000
    let mut st = LinkState::new("X", 0, 0);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 1000, false), Verdict::Pending);
    assert!(matches!(link::evaluate(&hw, &mut st, &G, &s, 9000, false), Verdict::LinkDead(_)));
}

#[test]
fn l3_no_reply_for_stall_ms_then_gateway_silent_is_dead_in_about_1s() {
    let hw = fake();
    let s = Settings::fresh(); // stall_ms = 1000：从“最后一次收到数据”起算（墙钟）
    let mut st = LinkState::new("X", 0, 0);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 0, false), Verdict::Healthy); // 网关就绪：ping 过一次（应答），公网也通
    // 之后一直在发包（out 增长），却一个字节都没收到
    for t in [250, 500, 750] {
        let mut sn = hw.snap.get();
        sn.out_octets += 1000;
        hw.snap.set(sn);
        assert!(matches!(link::evaluate(&hw, &mut st, &G, &s, t, false), Verdict::Pending | Verdict::Healthy), "t={}", t);
    }
    let mut sn = hw.snap.get();
    sn.out_octets += 1000;
    hw.snap.set(sn);
    hw.ping_ok.set(false); // 热点没了：网关不再应答，公网也不通
    hw.wan.set(false);
    // 1000 ms 没有回音 → 怀疑 → 网关此前回过 ICMP，现在不通 → 直接判死（不再串行等公网复核）
    assert!(matches!(link::evaluate(&hw, &mut st, &G, &s, 1000, false), Verdict::LinkDead(_)));
}

#[test]
fn l3b_idle_link_uses_heartbeat_not_traffic() {
    let hw = fake();
    let s = Settings::fresh(); // heartbeat_ms = 1500
    let mut st = LinkState::new("X", 0, 0);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 0, false), Verdict::Healthy);
    hw.ping_ok.set(false);
    hw.wan.set(false);
    // 完全空闲（没有发出任何东西）：1500 ms 没收到任何数据后用心跳去确认网关
    assert!(!matches!(link::evaluate(&hw, &mut st, &G, &s, 1000, false), Verdict::LinkDead(_)));
    assert!(matches!(link::evaluate(&hw, &mut st, &G, &s, 1500, false), Verdict::LinkDead(_)));
}

#[test]
fn l4_gateway_ignores_icmp_but_wan_ok_is_not_dead() {
    let hw = fake();
    hw.ping_ok.set(false); // 这个网关从来不回 ICMP（公网是通的）
    let s = Settings::fresh();
    let mut st = LinkState::new("X", 0, 0);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 0, false), Verdict::Healthy);
    for t in [250, 500, 750] {
        let mut sn = hw.snap.get();
        sn.out_octets += 1000;
        hw.snap.set(sn);
        assert!(matches!(link::evaluate(&hw, &mut st, &G, &s, t, false), Verdict::Pending | Verdict::Healthy), "t={}", t);
    }
    let mut sn = hw.snap.get();
    sn.out_octets += 1000;
    hw.snap.set(sn);
    // 没有回音达到阈值：ICMP 不通，但此前也从没应答过 → 走保守流程：公网通 → 判健康，并记住该网关不回 ICMP
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 1000, false), Verdict::Healthy);
    assert!(st.gw_no_icmp);
}

#[test]
fn l5_wan_down_needs_two_consecutive_failures() {
    let hw = fake();
    hw.wan.set(false);
    let s = Settings::fresh();
    let mut st = LinkState::new("X", 0, 0);
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 0, false), Verdict::Pending); // 第 1 次失败：还不下结论
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 500, false), Verdict::NoInternet); // 0.5 秒后复测，第 2 次失败
    hw.wan.set(true); // 恢复
    assert_eq!(link::evaluate(&hw, &mut st, &G, &s, 1000, false), Verdict::Healthy);
}

#[test]
fn l6_backoff_is_exponential_and_capped() {
    assert_eq!(link::backoff_ms(15, 0), 15_000);
    assert_eq!(link::backoff_ms(15, 1), 30_000);
    assert_eq!(link::backoff_ms(15, 2), 60_000);
    assert_eq!(link::backoff_ms(15, 9), 300_000);
    assert_eq!(link::backoff_ms(0, 3), 0); // 0 = 关闭退避（等同 v1.1.5 行为）
}

#[test]
fn t23_driver_attempt_fail_notification_fails_fast() {
    let mut d = Decider::new();
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let w = st(Some(("B", 2, 70)), &[("A", 2, 80), ("B", 2, 70)]);
    assert!(d.decide(&w, &s, 1000, false).connect.is_some());
    // 驱动在 0.5 秒后就通知“连接请求失败”：不用等满 4 秒
    d.note_attempt_failed(1500);
    let r = d.decide(&w, &s, 1600, false);
    assert!(r.logs.iter().any(|l| l.contains("没有成功")));
}

#[test]
fn t24_weak_signal_disconnect_does_not_wait_for_grace() {
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    // 掉线前信号已经很弱（< 回落阈值）：系统自己在漫游 / 网络快没了，立刻连下一个
    let mut d = Decider::new();
    assert!(d.decide(&st(Some(("B", 2, 30)), &[("B", 2, 30)]), &s, 500, false).connect.is_none());
    let off = st(None, &[("A", 2, 80)]);
    assert!(d.decide(&off, &s, 1000, false).connect.is_some());
    // 掉线前信号很好：仍然给用户手动换网让路 1.2 秒
    let mut d = Decider::new();
    assert!(d.decide(&st(Some(("B", 2, 80)), &[("B", 2, 80)]), &s, 500, false).connect.is_none());
    assert!(d.decide(&off, &s, 1000, false).connect.is_none());
    assert!(d.decide(&off, &s, 2300, false).connect.is_some());
}

#[test]
fn t25_unusable_backoff_blocks_target_then_expires() {
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    d.unusable_handle().lock().unwrap().insert("Hotspot".to_string(), Unusable { until: 10_000, level: 0, dead_at: 0 });
    let on_home = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&on_home, &s, 9000, false).connect.is_none()); // 退避期内：不把它当作切换目标
    assert_eq!(d.decide(&on_home, &s, 10_500, false).connect.map(|p| p.ssid), Some("Hotspot".to_string())); // 到期立即恢复
}

#[test]
fn s7_internet_check_default_on_migration() {
    let s = Settings::from_json(r#"{"SchemaVersion":3,"InternetCheck":false}"#).unwrap();
    assert!(s.internet_check);
    assert_eq!(s.schema_version, CURRENT_SCHEMA);
    // 新版本里用户自己关掉的，不再被改回来
    let s = Settings::from_json(r#"{"SchemaVersion":4,"InternetCheck":false}"#).unwrap();
    assert!(!s.internet_check);
    assert_eq!(s.disc_grace_ms, 400);
    // v1.2.0 迁移：还停留在旧默认值上的设置改成新默认值；用户自己改过的不动
    let s = Settings::from_json(r#"{"SchemaVersion":4,"StallMs":1500,"HeartbeatMs":2000,"DiscGraceMs":1200}"#).unwrap();
    assert_eq!((s.stall_ms, s.heartbeat_ms, s.disc_grace_ms), (1000, 1500, 400));
    let s = Settings::from_json(r#"{"SchemaVersion":4,"StallMs":3000,"DiscGraceMs":2000}"#).unwrap();
    assert_eq!((s.stall_ms, s.disc_grace_ms), (3000, 2000));
}

// ---------------- v1.1.9：热点开/关要立刻反应、流量计费 ----------------
use crate::link::CurLink;
use crate::traffic::{Acct, Store};

#[test]
fn t26_hotspot_turned_on_connects_immediately_without_confirm_or_cooldown() {
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 5000, &["Hotspot", "Home"]); // 确认时间故意设得很长
    let mut w0 = st(Some(("Home", 2, 70)), &[("Home", 2, 70)]);
    w0.profiles.insert("Hotspot".to_string()); // 热点配置已保存，但现在看不到
    assert!(d.decide(&w0, &s, 1000, false).connect.is_none());
    assert!(d.hunting(), "有更高优先级的已保存网络没出现：进入猎取模式，引擎会用更短间隔扫描");
    // 热点打开，下一次扫描看到它：不等确认时间，立即切换
    let w1 = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    let p = d.decide(&w1, &s, 3000, false).connect.expect("刚出现的更高优先级网络应立即连接");
    assert_eq!(p.ssid, "Hotspot");
}

#[test]
fn t27_already_visible_higher_priority_still_waits_for_confirm() {
    // 程序启动时就在场的网络、或者信号在阈值线上抖动的网络，不算“刚出现”：仍按确认时间防抖
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 5000, &["Hotspot", "Home"]);
    let w = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&w, &s, 1000, false).connect.is_none());
    assert!(d.decide(&w, &s, 3000, false).connect.is_none());
    assert!(d.decide(&w, &s, 6500, false).connect.is_some());
}

#[test]
fn t28_hotspot_turned_off_scan_proves_it_is_gone_no_grace() {
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let mut d = Decider::new();
    assert!(d.decide(&st(Some(("A", 2, 80)), &[("A", 2, 80), ("B", 2, 70)]), &s, 500, false).connect.is_none());
    // A（热点）被关：断开，附近网络里已经没有 A
    let off = st(None, &[("B", 2, 70)]);
    assert!(d.decide(&off, &s, 1000, false).connect.is_none()); // 刚断开、还没有扫描证据：先让路（可能是手动换网）
    d.note_scan_done(1300); // 断线后完成了一次扫描，A 没有回应
    assert_eq!(d.decide(&off, &s, 1400, false).connect.map(|p| p.ssid), Some("B".to_string()));
}

#[test]
fn t29_scan_shows_old_network_still_there_keeps_grace() {
    // 用户在 Windows 里手动换网：断开后原网络仍然在扫描结果里（时间戳还在更新）→ 继续让路，不抢着连
    let s = cfg("Global", "5G", 0, &["A", "B"]);
    let mut d = Decider::new();
    assert!(d.decide(&st(Some(("A", 2, 80)), &[("A", 2, 80), ("B", 2, 70)]), &s, 500, false).connect.is_none());
    let off = st(None, &[("A", 2, 80), ("B", 2, 70)]);
    assert!(d.decide(&off, &s, 1000, false).connect.is_none());
    d.note_scan_done(1300);
    assert!(d.decide(&off, &s, 1400, false).connect.is_none());
}

#[test]
fn f1_current_ap_silent_after_scan_is_flagged() {
    let mut f = Freshness::new(0);
    let mk = |ts: u64| {
        let mut w = st(Some(("Hotspot", 2, 80)), &[("Hotspot", 2, 80)]);
        for n in w.nearby.iter_mut() {
            for b in n.bss.iter_mut() {
                b.host_ts = ts;
            }
        }
        w
    };
    f.scan_done(1000);
    let mut w = mk(7);
    f.apply(&mut w, 1000);
    assert!(!f.cur_silent);
    f.scan_done(3500);
    let mut w = mk(7); // 时间戳一直没变，而且之后又扫描过
    f.apply(&mut w, 4000);
    assert!(f.cur_silent, "当前接入点静默：可能是热点刚被关，链路监视应立刻去 ping 网关");
    let mut w = mk(8); // 时间戳又更新了
    f.apply(&mut w, 4200);
    assert!(!f.cur_silent);
}

#[test]
fn tr1_traffic_split_by_connection_and_network() {
    use std::cell::Cell;
    let counters = Cell::new((1000u64, 500u64));
    let read = |_: &[u8; 16]| {
        let (i, o) = counters.get();
        Some(link::IfSnapshot { if_index: 1, oper_up: true, media_up: true, in_octets: i, out_octets: o })
    };
    let cl = |ssid: &str, at: i64| CurLink { ssid: ssid.to_string(), guid: [0; 16], connected_at: at };
    let mut a = Acct::new(Store::default());
    let home = cl("Home", 0);
    a.feed(Some(&home), &read, 0, "2026-11-13", 100); // 建立基线
    counters.set((3000, 900));
    let s = a.feed(Some(&home), &read, 1000, "2026-11-13", 101);
    assert_eq!((s.sess_rx, s.sess_tx), (2000, 400));
    assert_eq!(s.rx_bps, 1200); // 0.6 × 2000 B/s（轻度平滑）
    // 换到热点：断开前的最后增量记在 Home 头上，热点从 0 开始
    counters.set((5000, 1000));
    let hot = cl("Hotspot", 5000);
    let s2 = a.feed(Some(&hot), &read, 2000, "2026-11-13", 102);
    assert_eq!(a.store.nets["Home"].total_rx, 4000);
    assert_eq!(a.store.nets["Home"].total_tx, 500);
    assert_eq!((s2.sess_rx, s2.sess_tx, s2.ssid.as_str()), (0, 0, "Hotspot"));
    assert_eq!(a.store.recent.len(), 1);
    counters.set((5600, 1100));
    let s3 = a.feed(Some(&hot), &read, 3000, "2026-11-13", 103);
    assert_eq!((s3.sess_rx, s3.total_rx), (600, 600));
    // 跨天：今日清零，累计保留
    counters.set((5700, 1100));
    let s4 = a.feed(Some(&hot), &read, 4000, "2026-11-14", 104);
    assert_eq!((s4.today_rx, s4.total_rx), (100, 700));
    // 网卡计数器被重置（禁用再启用）：从 0 重新计，不会出现巨大的负数 / 溢出
    counters.set((10, 5));
    let s5 = a.feed(Some(&hot), &read, 5000, "2026-11-14", 105);
    assert_eq!(s5.sess_rx, 700 + 10);
    // 断开：快照清空，本次连接进入“最近连接”
    let s6 = a.feed(None, &read, 6000, "2026-11-14", 106);
    assert!(!s6.connected);
    assert_eq!(a.store.recent.len(), 2);
}

// ---------------- v1.2.0：回落更快 ----------------
#[test]
fn t30_link_monitor_verdict_skips_confirm_delay() {
    // 确认时间故意设得很长：链路监视已经用多层证据判死了当前网络，回落不该再等确认
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 5000, &["Hotspot", "Home"]);
    let w = st(Some(("Hotspot", 2, 80)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&w, &s, 1000, false).connect.is_none()); // 一切正常
    d.report_no_internet("Hotspot"); // 链路监视判死
    let p = d.decide(&w, &s, 1100, false).connect.expect("判死后应立即回落，不等确认时间");
    assert_eq!(p.ssid, "Home");
}

#[test]
fn t31_reappearing_network_clears_its_backoff() {
    // 热点被判死后写了 15 秒退避；它消失又重新出现（用户关了又开）= 重启过，退避作废，立即连
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    d.unusable_handle().lock().unwrap().insert("Hotspot".to_string(), Unusable { until: 100_000, level: 1, dead_at: 0 });
    let w1 = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    assert!(d.decide(&w1, &s, 1000, false).connect.is_none()); // 退避期内：不当作切换目标
    let w0 = st(Some(("Home", 2, 70)), &[("Home", 2, 70)]); // 热点消失
    d.decide(&w0, &s, 2000, false);
    let p = d.decide(&w1, &s, 3000, false).connect.expect("重新出现后应立即连接");
    assert_eq!(p.ssid, "Hotspot");
    assert!(d.unusable_handle().lock().unwrap().get("Hotspot").is_none());
}

// ---------------- v1.2.1：日志里的 bug ----------------
#[test]
fn t32_backoff_expiry_does_not_reconnect_to_ghost_that_was_never_seen_again() {
    // 日志 13:33:15：热点已经关了，但缓存里的旧条目还没过期；退避 15 秒一到就切回热点，白白断网 10 秒。
    // 现在：退避到期后还要求它在判死之后又被扫描更新过
    let mut d = Decider::new();
    let s = cfg("Same", "5G", 0, &["Hotspot", "Home"]);
    d.unusable_handle().lock().unwrap().insert("Hotspot".to_string(), Unusable { until: 5_000, level: 0, dead_at: 4_000 });
    let mut ghost = st(Some(("Home", 2, 70)), &[("Hotspot", 2, 80), ("Home", 2, 70)]);
    for n in ghost.nearby.iter_mut() {
        if n.ssid == "Hotspot" {
            for b in n.bss.iter_mut() {
                b.changed_at = 3_000; // 判死之前的最后一次更新：判死之后再没变化过 = 幽灵
            }
        }
    }
    assert!(d.decide(&ghost, &s, 20_000, false).connect.is_none()); // 退避早已到期，但没有新的证据
    let mut alive = ghost.clone();
    for n in alive.nearby.iter_mut() {
        for b in n.bss.iter_mut() {
            b.changed_at = 19_000; // 判死之后又被扫描更新过：真的还在
        }
    }
    assert_eq!(d.decide(&alive, &s, 20_100, false).connect.map(|p| p.ssid), Some("Hotspot".to_string()));
}
