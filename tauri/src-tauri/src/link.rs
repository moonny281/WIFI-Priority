//! 链路健康检测（v1.1.8）。分三层，目的是在“当前网络其实已经用不了”时尽快回落到下一个网络：
//!
//!  L0 事件推送：WLAN ACM/MSM 通知（由引擎转发）→ 立即唤醒本线程；本线程不做任何判断，只是醒得快。
//!  L1 硬件读数：每 250 ms 用 GetIfEntry2 读网卡驱动（NDIS）的链路状态与 64 位收发字节计数器，不发包。
//!     - 网卡/驱动报告链路断开 → 立即判死；
//!     - 我们发了包（out 增长）却一个字节都没收到（in 不增长）→ “怀疑”。
//!  L2 分级主动探测（只在“怀疑”或空闲心跳到期时发包，且都绑定到 WiFi 网卡，不会测到有线网卡/VPN）：
//!     ① 网关 ICMP（进程内 IcmpSendEcho，不起 ping.exe）；② 公网 TCP 443（IP_UNICAST_IF 强制走该网卡，三个地址并行）。
//!     连续 2 次确认才下结论。
//!
//! 不使用 Windows 自带的网络状态（NCSI / INetworkListManager）：它按系统自己的节奏探测并带滞后。
//!
//! evaluate() 通过 Hw trait 取得硬件数据与探测结果，所以可以用假数据做单元测试（见 tests.rs）。

use crate::decide::{Unusable, UnusableMap};
use crate::model::Settings;
use crate::state::Shared;
use crate::util::now_ms;
use std::ffi::c_void;
use std::mem::zeroed;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::windows::io::AsRawSocket;
use std::ptr::null;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceGuidToLuid, GetAdaptersAddresses, GetIfEntry2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho,
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
};
use windows_sys::Win32::NetworkManagement::Ndis::{IfOperStatusUp, MediaConnectStateConnected, NET_LUID_LH};
use windows_sys::Win32::Networking::WinSock::{setsockopt, AF_INET, IPPROTO_IP, IP_UNICAST_IF, SOCKADDR_IN, SOCKET};

// ---------------------------------------------------------------------------
// L1：网卡硬件读数
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IfSnapshot {
    pub if_index: u32,
    pub oper_up: bool,  // OperStatus == Up
    pub media_up: bool, // MediaConnectState == Connected（驱动/NDIS 报告的链路状态）
    pub in_octets: u64, // 网卡驱动计数器（64 位，不会像旧 MIB_IFROW 那样 32 位回绕）
    pub out_octets: u64,
}

/// 读取网卡的硬件状态与收发字节计数。guid 就是 WLAN 接口 GUID（与 IP Helper 的接口 GUID 相同）。
pub fn read_if(guid: &[u8; 16]) -> Option<IfSnapshot> {
    unsafe {
        let g: GUID = std::ptr::read_unaligned(guid.as_ptr() as *const GUID);
        let mut luid: NET_LUID_LH = zeroed();
        if ConvertInterfaceGuidToLuid(&g, &mut luid) != 0 {
            return None;
        }
        let mut row: MIB_IF_ROW2 = zeroed();
        row.InterfaceLuid = luid;
        if GetIfEntry2(&mut row) != 0 {
            return None;
        }
        Some(IfSnapshot {
            if_index: row.InterfaceIndex,
            oper_up: row.OperStatus == IfOperStatusUp,
            media_up: row.MediaConnectState == MediaConnectStateConnected,
            in_octets: row.InOctets,
            out_octets: row.OutOctets,
        })
    }
}

/// 该网卡的 IPv4 默认网关；没有 = DHCP 还没完成（或失败）。
pub fn gateway_v4(if_index: u32) -> Option<Ipv4Addr> {
    unsafe {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut len = 16 * 1024u32;
        let mut buf = vec![0u8; len as usize];
        let mut r = GetAdaptersAddresses(AF_INET as u32, flags, null(), buf.as_mut_ptr() as *mut _, &mut len);
        if r == ERROR_BUFFER_OVERFLOW {
            buf = vec![0u8; len as usize];
            r = GetAdaptersAddresses(AF_INET as u32, flags, null(), buf.as_mut_ptr() as *mut _, &mut len);
        }
        if r != 0 {
            return None;
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            if (*p).Anonymous1.Anonymous.IfIndex == if_index {
                let mut g = (*p).FirstGatewayAddress;
                while !g.is_null() {
                    let sa = (*g).Address.lpSockaddr as *const SOCKADDR_IN;
                    if !sa.is_null() && (*sa).sin_family == AF_INET {
                        return Some(Ipv4Addr::from((*sa).sin_addr.S_un.S_addr.to_ne_bytes()));
                    }
                    g = (*g).Next;
                }
            }
            p = (*p).Next;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// L2：主动探测（进程内，不起子进程）
// ---------------------------------------------------------------------------

/// 进程内 ICMP 回显（IcmpSendEcho）。网关在同一网段，路由天然走该 WiFi 网卡。
pub fn icmp_ping(dst: Ipv4Addr, timeout_ms: u32) -> bool {
    unsafe {
        let h = IcmpCreateFile();
        if h == INVALID_HANDLE_VALUE {
            return false;
        }
        let payload = [0x57u8; 8];
        let mut reply = [0u8; 128]; // ICMP_ECHO_REPLY + 8 字节数据 + IO_STATUS_BLOCK，留足余量
        let n = IcmpSendEcho(
            h,
            u32::from_ne_bytes(dst.octets()),
            payload.as_ptr() as *const c_void,
            payload.len() as u16,
            null(),
            reply.as_mut_ptr() as *mut c_void,
            reply.len() as u32,
            timeout_ms,
        );
        IcmpCloseHandle(h);
        n > 0
    }
}

/// 绑定到指定网卡的 TCP 连接探测。IP_UNICAST_IF(=31) 强制从该网卡发出（IPv4 要求网络字节序的接口索引），
/// 避免“WiFi 已死、有线/VPN 还通 → 误判检测通过”。只有 connect 成功才算通（RST 可能来自防火墙/认证门户）。
pub fn tcp_ok_on_iface(if_index: u32, ip: Ipv4Addr, port: u16, timeout: Duration) -> bool {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = match Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let idx = if_index.to_be();
    unsafe {
        setsockopt(
            s.as_raw_socket() as SOCKET,
            IPPROTO_IP,
            IP_UNICAST_IF,
            &idx as *const u32 as *const u8,
            4,
        );
    }
    s.connect_timeout(&SocketAddr::from((ip, port)).into(), timeout).is_ok()
}

/// 并行探测多个公网地址，任一通即可。网卡没有地址/路由时 connect 会立即失败，不会白等满超时。
pub fn wan_ok(if_index: u32, timeout: Duration) -> bool {
    const TARGETS: [Ipv4Addr; 3] = [
        Ipv4Addr::new(223, 5, 5, 5),
        Ipv4Addr::new(1, 1, 1, 1),
        Ipv4Addr::new(114, 114, 114, 114),
    ];
    let (tx, rx) = mpsc::channel();
    for ip in TARGETS {
        let tx = tx.clone();
        // 探测线程只做一次 connect，栈开小一点（默认 2 MB 的预留 + 提交对常驻内存没好处）
        let _ = std::thread::Builder::new().stack_size(128 * 1024).spawn(move || {
            let _ = tx.send(tcp_ok_on_iface(if_index, ip, 443, timeout));
        });
    }
    drop(tx);
    rx.iter().any(|ok| ok)
}

// ---------------------------------------------------------------------------
// 硬件抽象（便于单元测试）
// ---------------------------------------------------------------------------

pub trait Hw {
    fn read_if(&self, guid: &[u8; 16]) -> Option<IfSnapshot>;
    fn gateway(&self, if_index: u32) -> Option<Ipv4Addr>;
    fn ping(&self, ip: Ipv4Addr, timeout_ms: u32) -> bool;
    fn wan_ok(&self, if_index: u32, timeout: Duration) -> bool;
}

pub struct RealHw;

impl Hw for RealHw {
    fn read_if(&self, guid: &[u8; 16]) -> Option<IfSnapshot> {
        read_if(guid)
    }
    fn gateway(&self, if_index: u32) -> Option<Ipv4Addr> {
        gateway_v4(if_index)
    }
    fn ping(&self, ip: Ipv4Addr, timeout_ms: u32) -> bool {
        icmp_ping(ip, timeout_ms)
    }
    fn wan_ok(&self, if_index: u32, timeout: Duration) -> bool {
        wan_ok(if_index, timeout)
    }
}

// ---------------------------------------------------------------------------
// 判定
// ---------------------------------------------------------------------------

/// 引擎发布给本线程的“当前连接”。
#[derive(Clone, Debug)]
pub struct CurLink {
    pub ssid: String,
    pub guid: [u8; 16],
    pub connected_at: i64, // 这一次连接开始的时间（SSID 或 BSSID 变了就重新计）
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Verdict {
    Pending,              // 还没有新结论（等 DHCP / 等下一次探测）
    Healthy,              // 确认可用
    LinkDead(&'static str), // 链路层/网关层不可用
    NoInternet,           // 网关可达但公网不通
}

#[derive(Default, Debug)]
pub struct LinkState {
    pub ssid: String,
    pub connected_at: i64,
    prev: Option<IfSnapshot>,
    down_since: i64,       // 网卡报告链路断开的起始时间（0 = 没断）。主动切换网络时旧连接必然短暂掉线，不能一掉线就判死
    seen_tries: u8,        // 试探“网关回不回 ICMP”的次数（最多 3 次）
    next_seen_at: i64,
    last_rx_at: i64,       // 最后一次收到入站数据的时间（墙钟）——判断“多久没收到东西了”的起点
    out_since_rx: u64,     // 最后一次入站之后发出去的字节数（发了东西却没有回音才算“怀疑”）
    gw: Option<Ipv4Addr>,
    pub gw_icmp_seen: bool, // 这次连接里网关回过 ICMP：之后 ICMP 连续不通就直接判死，不再串行等公网复核
    pub gw_no_icmp: bool,   // 该网关不回 ICMP（但公网通）：之后不再用 ICMP 做心跳，避免误判
    next_hb_at: i64,
    next_wan_at: i64,
    wan_fail: u32,
    bad_since: i64, // 第一次发现异常的时间（0 = 没有），用来在日志里写“从发现异常到判死用了多久”
    healthy_since: i64,
    reported: bool,
    recheck_at: i64,
}

impl LinkState {
    pub fn new(ssid: &str, connected_at: i64, now: i64) -> LinkState {
        LinkState {
            ssid: ssid.to_string(),
            connected_at,
            last_rx_at: now,
            ..Default::default()
        }
    }
}

/// 一轮判定。纯逻辑：硬件读数与探测都经 Hw 取得，时间由调用方传入（毫秒）。
/// silent：引擎观察到当前接入点的帧时间戳很久没刷新（热点可能刚被关）——只用来立刻去 ping 网关，不会单独判死。
pub fn evaluate<H: Hw>(hw: &H, st: &mut LinkState, guid: &[u8; 16], s: &Settings, now: i64, silent: bool) -> Verdict {
    // —— L1：硬件说链路断了，直接判死，不发任何包 ——
    let snap = match hw.read_if(guid) {
        Some(x) => x,
        None => return Verdict::Pending,
    };
    if !snap.oper_up || !snap.media_up {
        // 链路断开本身不是“网络不可用”：我们自己切换网络时，旧连接必然短暂掉线；用户手动换网、漫游也是。
        // 真的断了 Windows 会报“已断开”，决策器走断线分支立刻处理。这里只兜底“驱动一直说已连接，但网卡链路持续断开”的异常：
        // 持续 2 秒以上、而且引擎仍然认为连着同一个网络，才算。
        if st.down_since == 0 {
            st.down_since = now.max(1);
        }
        return if now - st.down_since >= 2000 { Verdict::LinkDead("网卡/驱动报告链路持续断开") } else { Verdict::Pending };
    }
    st.down_since = 0;
    match &st.prev {
        Some(p) => {
            let d_in = snap.in_octets.saturating_sub(p.in_octets);
            let d_out = snap.out_octets.saturating_sub(p.out_octets);
            if d_in > 0 {
                st.last_rx_at = now;
                st.out_since_rx = 0;
                st.bad_since = 0;
            } else {
                st.out_since_rx = st.out_since_rx.saturating_add(d_out);
            }
        }
        None => st.last_rx_at = now,
    }
    st.prev = Some(snap);

    // —— 刚连上：等 DHCP/网关就绪（事件唤醒 + 上限），取代旧版固定的 12 秒 ——
    if st.gw.is_none() {
        st.gw = hw.gateway(snap.if_index);
        if st.gw.is_none() {
            return if now - st.connected_at > s.ip_wait_ms {
                Verdict::LinkDead("DHCP 超时，没有拿到网关")
            } else {
                Verdict::Pending
            };
        }
        st.next_wan_at = now; // 网关一就绪就立刻验一次公网
        // 顺便 ping 一次网关：回过 ICMP 的网关，之后 ICMP 不通就是真的不通（快速判死）；不回的网关走保守流程
        st.seen_tries = 1;
        st.next_seen_at = now + 1500;
        if hw.ping(st.gw.unwrap(), 500) {
            st.gw_icmp_seen = true;
            st.last_rx_at = now;
        }
    }
    let gw = st.gw.unwrap();
    // 刚连上时 ARP 还没解析好，第一次 ping 可能超时：之后再试几次（最多 3 次），这样“回过 ICMP 的网关”才能走快速判死
    if !st.gw_icmp_seen && !st.gw_no_icmp && st.seen_tries < 3 && now >= st.next_seen_at {
        st.seen_tries += 1;
        st.next_seen_at = now + 2000;
        if hw.ping(gw, 500) {
            st.gw_icmp_seen = true;
            st.last_rx_at = now;
        }
    }

    // 起点是“最后一次收到数据”（墙钟），不是“发现异常的那一拍”：真实断线到判死的时间才算得准
    let since_rx = now - st.last_rx_at;
    // 发了东西（>= 256 字节）却已经 stall_ms 没有任何回音 → 怀疑；完全空闲时由心跳兜底
    let suspect = since_rx >= s.stall_ms && st.out_since_rx >= 256;
    let wan_timeout = Duration::from_millis(s.wan_timeout_ms.max(1) as u64);
    let probe_due = suspect || (now >= st.next_hb_at && (since_rx >= s.heartbeat_ms || silent));
    let mut gw_alive = false;

    // —— L2-①：网关 ICMP ——
    if probe_due && !st.gw_no_icmp {
        st.next_hb_at = now + s.heartbeat_ms;
        let pm = if st.gw_icmp_seen { 300 } else { 400 };
        gw_alive = hw.ping(gw, pm) || hw.ping(gw, pm);
        if gw_alive {
            st.gw_icmp_seen = true;
            st.last_rx_at = now; // 网关回了应答，链路是活的
            st.out_since_rx = 0;
            st.bad_since = 0;
        } else if st.gw_icmp_seen {
            // 这个网关此前回过 ICMP，现在连续不通：直接判死，不再串行等公网复核（省约 0.9 秒）
            st.bad_since = st.last_rx_at;
            return Verdict::LinkDead("网关不再应答（此前应答过）");
        } else if hw.wan_ok(snap.if_index, wan_timeout) {
            // 网关不回 ICMP（部分热点 / 访客网络），但公网是通的：记住，别再用 ICMP 判死
            st.gw_no_icmp = true;
            st.last_rx_at = now;
            st.out_since_rx = 0;
            st.bad_since = 0;
            return Verdict::Healthy;
        } else {
            st.bad_since = st.last_rx_at;
            return Verdict::LinkDead("入站停滞且网关无响应");
        }
    }

    // —— L2-②：公网（到期，或刚被怀疑过）——
    if now >= st.next_wan_at || (suspect && (gw_alive || st.gw_no_icmp)) {
        if hw.wan_ok(snap.if_index, wan_timeout) {
            st.wan_fail = 0;
            st.last_rx_at = now; // 公网通：链路是活的
            st.out_since_rx = 0;
            st.bad_since = 0;
            st.next_wan_at = now + s.wan_interval_sec * 1000;
            return Verdict::Healthy;
        }
        st.wan_fail += 1;
        if st.bad_since == 0 {
            st.bad_since = now;
        }
        st.next_wan_at = now + 500; // 失败后 0.5 秒内复测，连续 2 次才下结论
        if st.wan_fail >= 2 {
            return Verdict::NoInternet;
        }
    }
    if st.wan_fail == 0 && st.gw.is_some() && st.bad_since == 0 && now < st.next_wan_at {
        // 两次探测之间：沿用“健康”
        return Verdict::Healthy;
    }
    Verdict::Pending
}

pub fn backoff_ms(base_sec: i64, level: u32) -> i64 {
    if base_sec <= 0 {
        return 0;
    }
    (base_sec * 1000).saturating_mul(1i64 << level.min(5)).min(300_000) // 15→30→60→120→240→300 秒
}

/// 链路监视线程：事件（通知）一来立即跑一轮，否则每 250 ms 一轮（平时只读硬件计数器，不发包）。
///  - no_net：现有句柄，标记“当前网络不可用”，Decider 据此立即回落；
///  - unusable：带退避的“暂不重试”表（只在多层证据确认判死后写入，unusable_backoff_sec = 0 即关闭）。
pub fn spawn_monitor(
    sh: Arc<Shared>,
    cur: Arc<Mutex<Option<CurLink>>>,
    no_net: Arc<Mutex<Option<String>>>,
    unusable: UnusableMap,
    rx: mpsc::Receiver<u32>,
    log: impl Fn(&str) + Send + 'static,
) {
    let _ = std::thread::Builder::new().name("wifi-link".into()).spawn(move || {
        let hw = RealHw;
        let mut st = LinkState::default();
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while rx.try_recv().is_ok() {}

            let s = sh.settings.lock().unwrap().clone();
            let c = match cur.lock().unwrap().clone() {
                Some(c) if s.internet_check => c,
                _ => {
                    st = LinkState::default();
                    continue;
                }
            };
            let now = now_ms();
            // 程序刚发出连接请求（正在切换网络）：旧连接掉线是我们自己造成的，这段时间不评估、也不判死
            if now < sh.switch_until.load(Ordering::Relaxed) {
                st = LinkState::default();
                continue;
            }
            if st.ssid != c.ssid || st.connected_at != c.connected_at {
                st = LinkState::new(&c.ssid, c.connected_at, now);
            }
            // 已经判死、而且仍然连着（没有别的网络可去）：降低复查频率，别每 250 ms 反复发探测包
            if st.reported && now < st.recheck_at {
                continue;
            }

            let silent = sh.cur_silent.load(Ordering::Relaxed);
            let verdict = evaluate(&hw, &mut st, &c.guid, &s, now, silent);
            let dead_reason: Option<&'static str> = match verdict {
                Verdict::LinkDead(why) => Some(why),
                Verdict::NoInternet => Some("已连接，网关可达但公网不通"),
                Verdict::Healthy => {
                    if st.healthy_since == 0 {
                        st.healthy_since = now;
                    }
                    if st.reported {
                        st.reported = false;
                        let mut g = no_net.lock().unwrap();
                        if g.as_deref() == Some(c.ssid.as_str()) {
                            *g = None;
                        }
                    }
                    if now - st.healthy_since > 30_000 {
                        unusable.lock().unwrap().remove(&c.ssid); // 健康满 30 秒：清零退避等级
                    }
                    None
                }
                Verdict::Pending => None,
            };

            // 判定期间（探测可能花了一两秒）程序可能已经切走了：确认引擎当前连接仍然是这一个，才报告
            let still_current = cur
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .map_or(false, |x| x.ssid == c.ssid && x.connected_at == c.connected_at)
                && now_ms() >= sh.switch_until.load(Ordering::Relaxed);
            if let (Some(why), true) = (dead_reason, still_current) {
                if !st.reported {
                    st.reported = true;
                    st.healthy_since = 0;
                    let (level, wait) = {
                        let mut map = unusable.lock().unwrap();
                        let level = map.get(&c.ssid).map(|u| u.level + 1).unwrap_or(0);
                        let wait = backoff_ms(s.unusable_backoff_sec, level);
                        map.insert(c.ssid.clone(), Unusable { until: now + wait, level, dead_at: now });
                        (level, wait)
                    };
                    *no_net.lock().unwrap() = Some(c.ssid.clone());
                    sh.urgent_scan.store(true, Ordering::Relaxed); // 立刻扫描，别等 scan_interval
                    let took = if st.bad_since > 0 {
                        // 用判定完成的时刻算（探测本身要花一两秒，不能用这一轮开始的时刻）
                        format!("距最后一次收到数据 {} ms 判定", now_ms() - st.bad_since)
                    } else {
                        "即时判定".to_string()
                    };
                    let again = if wait > 0 {
                        format!("{} 秒内不会再把它当作切换目标（第 {} 次）", wait / 1000, level + 1)
                    } else {
                        "未启用退避，它重新成为最优时会再次尝试".to_string()
                    };
                    log(&format!("{} 不可用：{}（{}），回落到下一个网络；{}", c.ssid, why, took, again));
                    sh.wake();
                }
                st.recheck_at = now + 2000;
            }
        }
    });
}
