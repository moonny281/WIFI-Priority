//! 直接调用 Windows 原生 WLAN API（wlanapi.dll）：
//!  - 读取当前连接 / 附近网络 / BSSID 列表只需毫秒级的进程内调用（以前是每次启动 3 个 netsh 进程）
//!  - 订阅 WLAN 通知：断线、连上、扫描完成时立刻唤醒引擎
//!  - 直接 WlanConnect，可以指定 BSSID（用来在同名的 5G / 2.4G 之间选频段）
//! 结构体偏移按 x64 布局硬编码，并带有合理性校验；校验失败时 read_state 返回 None，引擎回退到 netsh。

use crate::model::*;
use crate::parse;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::{null_mut, read_unaligned};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Instant;

#[repr(C)]
struct ConnParams {
    mode: i32,                 // 0 = wlan_connection_mode_profile
    profile: *const u16,       // LPCWSTR
    ssid: *mut c_void,         // 可空
    bssid_list: *mut c_void,   // 可空：PDOT11_BSSID_LIST
    bss_type: i32,             // 1 = infrastructure
    flags: u32,
}

#[link(name = "wlanapi")]
extern "system" {
    fn WlanOpenHandle(version: u32, reserved: *mut c_void, negotiated: *mut u32, handle: *mut *mut c_void) -> u32;
    fn WlanCloseHandle(handle: *mut c_void, reserved: *mut c_void) -> u32;
    fn WlanEnumInterfaces(handle: *mut c_void, reserved: *mut c_void, list: *mut *mut c_void) -> u32;
    fn WlanScan(handle: *mut c_void, guid: *const u8, ssid: *mut c_void, ie: *mut c_void, reserved: *mut c_void) -> u32;
    fn WlanGetAvailableNetworkList(handle: *mut c_void, guid: *const u8, flags: u32, reserved: *mut c_void, list: *mut *mut c_void) -> u32;
    fn WlanGetNetworkBssList(handle: *mut c_void, guid: *const u8, ssid: *mut c_void, bss_type: i32, secure: i32, reserved: *mut c_void, list: *mut *mut c_void) -> u32;
    fn WlanGetProfileList(handle: *mut c_void, guid: *const u8, reserved: *mut c_void, list: *mut *mut c_void) -> u32;
    fn WlanQueryInterface(handle: *mut c_void, guid: *const u8, opcode: i32, reserved: *mut c_void, size: *mut u32, data: *mut *mut c_void, value_type: *mut c_void) -> u32;
    fn WlanConnect(handle: *mut c_void, guid: *const u8, params: *const ConnParams, reserved: *mut c_void) -> u32;
    fn WlanRegisterNotification(
        handle: *mut c_void,
        source: u32,
        ignore_dup: i32,
        callback: Option<unsafe extern "system" fn(*mut c_void, *mut c_void)>,
        context: *mut c_void,
        reserved: *mut c_void,
        prev_source: *mut u32,
    ) -> u32;
    fn WlanFreeMemory(p: *mut c_void);
}

const SOURCE_ACM: u32 = 0x8;
const ERR_ACCESS_DENIED: u32 = 5;
const IFACE_INFO_SIZE: usize = 532; // WLAN_INTERFACE_INFO：GUID(16) + WCHAR[256] + state(4)
const AVAIL_NET_SIZE: usize = 628; // WLAN_AVAILABLE_NETWORK
const BSS_ENTRY_SIZE: usize = 360; // WLAN_BSS_ENTRY
const PROFILE_INFO_SIZE: usize = 516; // WLAN_PROFILE_INFO：WCHAR[256] + flags(4)

/// 通知回调要往引擎线程发消息，回调是 C 函数指针，只能走全局变量。
static NOTIFY_TX: Mutex<Option<Sender<u32>>> = Mutex::new(None);

unsafe extern "system" fn on_notify(data: *mut c_void, _ctx: *mut c_void) {
    if data.is_null() {
        return;
    }
    let p = data as *const u8;
    let src = read_unaligned(p as *const u32);
    let code = read_unaligned(p.add(4) as *const u32);
    if src != SOURCE_ACM {
        return;
    }
    if let Ok(g) = NOTIFY_TX.lock() {
        if let Some(tx) = g.as_ref() {
            let _ = tx.send(code);
        }
    }
}

// ---------- 读内存的小工具 ----------
unsafe fn rd_u32(p: *const u8, off: usize) -> u32 {
    read_unaligned(p.add(off) as *const u32)
}
unsafe fn rd_u64(p: *const u8, off: usize) -> u64 {
    read_unaligned(p.add(off) as *const u64)
}
unsafe fn rd_i32(p: *const u8, off: usize) -> i32 {
    read_unaligned(p.add(off) as *const i32)
}
unsafe fn utf16_at(p: *const u8, off: usize, max: usize) -> String {
    let mut v: Vec<u16> = Vec::new();
    for i in 0..max {
        let c = read_unaligned(p.add(off + i * 2) as *const u16);
        if c == 0 {
            break;
        }
        v.push(c);
    }
    String::from_utf16_lossy(&v)
}
fn decode_ssid(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        // 很多国产路由器的 SSID 是 GBK 编码
        Err(_) => encoding_rs::GB18030.decode(b).0.into_owned(),
    }
}
unsafe fn ssid_at(p: *const u8, off: usize) -> Option<String> {
    let len = rd_u32(p, off) as usize;
    if len == 0 || len > 32 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(p.add(off + 4), len);
    Some(decode_ssid(bytes))
}
unsafe fn mac_at(p: *const u8, off: usize) -> String {
    let mut s = String::with_capacity(17);
    for i in 0..6 {
        if i > 0 {
            s.push(':');
        }
        s.push_str(&format!("{:02x}", *p.add(off + i)));
    }
    s
}
fn band_from_khz(khz: u32) -> i32 {
    if khz < 3_000_000 {
        2
    } else if khz < 5_925_000 {
        5
    } else {
        6
    }
}

pub struct Client {
    h: *mut c_void,
    ifaces: Vec<[u8; 16]>,
    iface_at: Instant,
    profiles: HashSet<String>,
    profiles_at: Option<Instant>,
    pub broken: bool,
}

// 句柄只在引擎线程里使用；通知回调通过全局 Sender 通信
unsafe impl Send for Client {}

impl Client {
    pub fn open(tx: Sender<u32>) -> Option<Client> {
        unsafe {
            let mut h: *mut c_void = null_mut();
            let mut neg = 0u32;
            if WlanOpenHandle(2, null_mut(), &mut neg, &mut h) != 0 || h.is_null() {
                return None;
            }
            if let Ok(mut g) = NOTIFY_TX.lock() {
                *g = Some(tx);
            }
            let mut prev = 0u32;
            WlanRegisterNotification(h, SOURCE_ACM, 0, Some(on_notify), null_mut(), null_mut(), &mut prev);
            let mut c = Client {
                h,
                ifaces: Vec::new(),
                iface_at: Instant::now(),
                profiles: HashSet::new(),
                profiles_at: None,
                broken: false,
            };
            c.refresh_ifaces();
            Some(c)
        }
    }

    fn refresh_ifaces(&mut self) {
        self.iface_at = Instant::now();
        unsafe {
            let mut list: *mut c_void = null_mut();
            if WlanEnumInterfaces(self.h, null_mut(), &mut list) != 0 || list.is_null() {
                return;
            }
            let p = list as *const u8;
            let n = (rd_u32(p, 0) as usize).min(16);
            let mut fresh = Vec::new();
            for i in 0..n {
                let mut g = [0u8; 16];
                std::ptr::copy_nonoverlapping(p.add(8 + i * IFACE_INFO_SIZE), g.as_mut_ptr(), 16);
                fresh.push(g);
            }
            self.ifaces = fresh;
            WlanFreeMemory(list);
        }
    }

    /// 请求所有网卡立即扫描（异步，完成时会收到通知 code = 7）。
    /// 返回 true = 至少有一块网卡接受了扫描请求（之后会收到通知 7/8）；false = 没有收到任何通知的可能，调用方不要空等。
    pub fn scan(&self) -> bool {
        let mut any = false;
        for g in &self.ifaces {
            unsafe {
                if WlanScan(self.h, g.as_ptr(), null_mut(), null_mut(), null_mut()) == 0 {
                    any = true;
                }
            }
        }
        any
    }

    /// 定向扫描某个 SSID（发送指向该 SSID 的探测请求）：比泛扫描更快，也能发现隐藏/弱信号网络。
    pub fn scan_directed(&self, ssid: &str) {
        let bytes = ssid.as_bytes();
        if bytes.is_empty() || bytes.len() > 32 {
            return;
        }
        // DOT11_SSID：uSSIDLength(4) + ucSSID[32]
        let mut buf = [0u8; 36];
        buf[0..4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf[4..4 + bytes.len()].copy_from_slice(bytes);
        for g in &self.ifaces {
            unsafe {
                WlanScan(self.h, g.as_ptr(), buf.as_mut_ptr() as *mut c_void, null_mut(), null_mut());
            }
        }
    }

    fn refresh_profiles(&mut self, g: &[u8; 16]) {
        if let Some(t) = self.profiles_at {
            if t.elapsed().as_secs() < 20 && !self.profiles.is_empty() {
                return;
            }
        }
        self.profiles_at = Some(Instant::now());
        unsafe {
            let mut list: *mut c_void = null_mut();
            if WlanGetProfileList(self.h, g.as_ptr(), null_mut(), &mut list) != 0 || list.is_null() {
                return;
            }
            let p = list as *const u8;
            let n = (rd_u32(p, 0) as usize).min(512);
            let mut set = HashSet::new();
            for i in 0..n {
                let name = utf16_at(p, 8 + i * PROFILE_INFO_SIZE, 256);
                if !name.is_empty() {
                    set.insert(name);
                }
            }
            self.profiles = set;
            WlanFreeMemory(list);
        }
    }

    /// 读取完整状态（通常 < 5 ms）。结构体校验失败返回 None。
    pub fn read_state(&mut self) -> Option<WifiState> {
        if self.h.is_null() || self.broken {
            return None;
        }
        if self.iface_at.elapsed().as_secs() > 10 {
            self.refresh_ifaces();
        }
        let mut st = WifiState::new();
        st.fast = true;
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, NearbyNetwork> = HashMap::new();
        let (mut total_bss, mut valid_bss) = (0usize, 0usize);
        let ifaces = self.ifaces.clone();

        unsafe {
            for (i, g) in ifaces.iter().enumerate() {
                // 1) 当前连接
                let mut data: *mut c_void = null_mut();
                let mut size = 0u32;
                let r = WlanQueryInterface(self.h, g.as_ptr(), 7, null_mut(), &mut size, &mut data, null_mut());
                if r == ERR_ACCESS_DENIED {
                    st.location_denied = true;
                } else if r == 0 && !data.is_null() {
                    let p = data as *const u8;
                    let state = rd_i32(p, 0);
                    if state == 1 && !st.connected {
                        if let Some(ssid) = ssid_at(p, 520) {
                            st.connected = true;
                            st.ssid = ssid;
                            st.cur_iface = i as i32;
                            let prof = utf16_at(p, 8, 256);
                            st.cur_profile = if prof.is_empty() { None } else { Some(prof) };
                            st.cur_bssid = Some(mac_at(p, 560));
                            st.signal = (rd_u32(p, 576) as i64).min(100);
                        }
                    } else if state == 5 || state == 6 || state == 7 {
                        st.connecting = true; // 关联/发现/认证中
                    }
                    WlanFreeMemory(data);
                }

                // 2) 已保存配置名（可见网络自带 profile 名，比 SSID 对应关系更准）
                let mut prof_by_ssid: HashMap<String, String> = HashMap::new();
                let mut alist: *mut c_void = null_mut();
                let r2 = WlanGetAvailableNetworkList(self.h, g.as_ptr(), 0, null_mut(), &mut alist);
                if r2 == ERR_ACCESS_DENIED {
                    st.location_denied = true;
                } else if r2 == 0 && !alist.is_null() {
                    let p = alist as *const u8;
                    let n = (rd_u32(p, 0) as usize).min(512);
                    for k in 0..n {
                        let base = 8 + k * AVAIL_NET_SIZE;
                        let ssid = ssid_at(p, base + 512);
                        let prof = utf16_at(p, base, 256);
                        if let Some(s) = ssid {
                            if !prof.is_empty() {
                                prof_by_ssid.insert(s, prof);
                            }
                        }
                    }
                    WlanFreeMemory(alist);
                }

                // 3) BSSID 列表：信号、频率（→ 频段）
                let mut blist: *mut c_void = null_mut();
                let r3 = WlanGetNetworkBssList(self.h, g.as_ptr(), null_mut(), 3, 0, null_mut(), &mut blist);
                if r3 == ERR_ACCESS_DENIED {
                    st.location_denied = true;
                } else if r3 == 0 && !blist.is_null() {
                    let p = blist as *const u8;
                    let n = (rd_u32(p, 4) as usize).min(1024);
                    for k in 0..n {
                        let base = 8 + k * BSS_ENTRY_SIZE;
                        total_bss += 1;
                        let rssi = rd_i32(p, base + 56);
                        let lq = rd_u32(p, base + 60);
                        let khz = rd_u32(p, base + 92);
                        // 合理性校验：不符合说明结构体偏移不对
                        if lq > 100 || rssi > 0 || rssi < -127 || khz < 2_300_000 || khz > 7_200_000 {
                            continue;
                        }
                        valid_bss += 1;
                        let ssid = match ssid_at(p, base) {
                            Some(s) => s,
                            None => continue, // 隐藏网络
                        };
                        if !map.contains_key(&ssid) {
                            map.insert(ssid.clone(), NearbyNetwork { ssid: ssid.clone(), ..Default::default() });
                            order.push(ssid.clone());
                        }
                        let net = map.get_mut(&ssid).unwrap();
                        if let Some(pn) = prof_by_ssid.get(&ssid) {
                            net.profile = Some(pn.clone());
                        }
                        net.bss.push(BssInfo {
                            bssid: mac_at(p, base + 40),
                            signal: lq as i64,
                            band: band_from_khz(khz),
                            chan: 0,
                            rssi,
                            iface: i as i32,
                            host_ts: {
                                // 优先用 TSF（接入点自己的计时器，收到它的新帧才会变），没有再用 host timestamp
                                let tsf = rd_u64(p, base + 72);
                                if tsf != 0 { tsf } else { rd_u64(p, base + 80) }
                            },
                            changed_at: 0, // 由 Freshness 在每次读取后填写
                            stale: false,
                        });
                    }
                    WlanFreeMemory(blist);
                }

                self.refresh_profiles(g);
            }
        }

        // 有 BSS 条目但一条都通不过校验 → 偏移不对，放弃原生读取
        if total_bss > 0 && valid_bss == 0 {
            self.broken = true;
            return None;
        }

        for name in &order {
            let mut n = map.remove(name).unwrap();
            parse::summarize(&mut n);
            st.nearby.push(n);
        }
        st.profiles = self.profiles.clone();

        // 当前连接的频段：用 BSSID 反查
        if st.connected {
            if let Some(cb) = st.cur_bssid.clone() {
                'outer: for n in &st.nearby {
                    if n.ssid != st.ssid {
                        continue;
                    }
                    for b in &n.bss {
                        if b.bssid == cb {
                            st.cur_band = b.band;
                            break 'outer;
                        }
                    }
                }
            }
        }
        Some(st)
    }

    /// 发出连接请求（异步完成）。bssid 不为空时指定接入点。
    pub fn connect(&self, profile: &str, bssid: Option<&str>, iface: i32) -> bool {
        if self.h.is_null() || profile.is_empty() || self.ifaces.is_empty() {
            return false;
        }
        let idx = if iface >= 0 && (iface as usize) < self.ifaces.len() { iface as usize } else { 0 };
        let guid = self.ifaces[idx];
        let wide: Vec<u16> = profile.encode_utf16().chain(std::iter::once(0)).collect();

        // DOT11_BSSID_LIST：NDIS 头(4) + uNumOfEntries(4) + uTotalNumOfEntries(4) + MAC(6) + 对齐(2) = 20 字节
        let mut list = [0u8; 20];
        let mut have_list = false;
        if let Some(b) = bssid {
            let parts: Vec<&str> = b.split(|c| c == ':' || c == '-').collect();
            if parts.len() == 6 {
                let mut ok = true;
                for (i, part) in parts.iter().enumerate() {
                    match u8::from_str_radix(part, 16) {
                        Ok(v) => list[12 + i] = v,
                        Err(_) => ok = false,
                    }
                }
                if ok {
                    list[0] = 0x80; // NDIS_OBJECT_TYPE_DEFAULT
                    list[1] = 1; // DOT11_BSSID_LIST_REVISION_1
                    list[2..4].copy_from_slice(&20u16.to_le_bytes()); // Size
                    list[4..8].copy_from_slice(&1u32.to_le_bytes()); // uNumOfEntries
                    list[8..12].copy_from_slice(&1u32.to_le_bytes()); // uTotalNumOfEntries
                    have_list = true;
                }
            }
        }

        let params = ConnParams {
            mode: 0,
            profile: wide.as_ptr(),
            ssid: null_mut(),
            bssid_list: if have_list { list.as_mut_ptr() as *mut c_void } else { null_mut() },
            bss_type: 1,
            flags: 0,
        };
        unsafe { WlanConnect(self.h, guid.as_ptr(), &params, null_mut()) == 0 }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !self.h.is_null() {
            unsafe {
                WlanCloseHandle(self.h, null_mut());
            }
            self.h = null_mut();
        }
    }
}

// ---------- 屏幕工作区（悬浮框吸附角落用） ----------
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut c_void, win_ini: u32) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
}

/// 主显示器工作区（不含任务栏）：left, top, right, bottom（物理像素）。
pub fn work_area() -> (i32, i32, i32, i32) {
    let mut r = Rect { left: 0, top: 0, right: 1920, bottom: 1040 };
    unsafe {
        if SystemParametersInfoW(0x0030, 0, &mut r as *mut Rect as *mut c_void, 0) == 0 {
            return (0, 0, 1920, 1040);
        }
    }
    (r.left, r.top, r.right, r.bottom)
}

/// 虚拟屏幕（所有显示器）：x, y, width, height（物理像素）。
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe { (GetSystemMetrics(76), GetSystemMetrics(77), GetSystemMetrics(78), GetSystemMetrics(79)) }
}

// ---------- 悬浮框层级（置顶 / 沉到桌面） ----------
#[link(name = "user32")]
extern "system" {
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
}

const HWND_TOPMOST: isize = -1;
const HWND_NOTOPMOST: isize = -2;
const HWND_BOTTOM: isize = 1;
// SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS（异步：UI 线程忙时不会把调用线程卡住）
const LAYER_FLAGS: u32 = 0x0001 | 0x0002 | 0x0010 | 0x4000;

/// top = true：置顶（可重复调用来“重申”）；false：取消置顶并沉到最底层。不抢焦点、不改位置和大小。
pub fn set_layer(hwnd: isize, top: bool) {
    unsafe {
        if top {
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, LAYER_FLAGS); // 不带 SHOWWINDOW：已隐藏的悬浮框不会被重申动作重新弹出来
        } else {
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, LAYER_FLAGS);
            SetWindowPos(hwnd, HWND_BOTTOM, 0, 0, 0, 0, LAYER_FLAGS);
        }
    }
}
