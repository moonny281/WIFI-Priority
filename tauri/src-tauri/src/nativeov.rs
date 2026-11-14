//! 原生（GDI）悬浮框（v1.2.0，降低后台内存占用）。
//!
//! WebView2 哪怕只显示一个很小的悬浮框，也要常驻一整棵进程树（浏览器进程 + 渲染进程 + 网络服务等），
//! 合计一百多 MB。悬浮框的内容其实只有几行文字和一个四格信号图标，所以这里直接用 Win32 / GDI 画：
//! 常驻内存只有几 MB。主窗口仍然是 WebView2，但只在打开时才创建、关闭就销毁（见 winmgr.rs）。
//!
//! 外观和行为与网页版悬浮框保持一致：主题（跟随系统 / 浅色 / 深色）、大小、透明度、四角吸附 / 自由位置、
//! 拖动后记住位置、锁定（鼠标穿透）、置顶 / 沉到桌面、实时流量两行。创建失败时自动回退到网页版悬浮框。
//!
//! 线程模型：悬浮框窗口由一个专用线程创建并运行消息循环；其他线程只通过 PostMessage 通知它刷新（WM_APP_REFRESH），
//! 内容由窗口线程自己从缓存 + 设置里合成并绘制。

use crate::model::EngineStatus;
use crate::native;
use crate::state::Shared;
use crate::traffic::TrafficSnap;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::AppHandle;

// ---------------------------------------------------------------------------
// Win32 FFI（手写声明，全部用整数类型，不依赖 windows-sys 的类型版本）
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}
#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
    l_private: u32,
}
#[repr(C)]
struct PaintStruct {
    hdc: isize,
    f_erase: i32,
    rc_paint: Rect,
    f_restore: i32,
    f_inc_update: i32,
    rgb_reserved: [u8; 32],
}
#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}
type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;
#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: Option<WndProc>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: isize,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: isize,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(c: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        inst: isize,
        param: *mut c_void,
    ) -> isize;
    fn DefWindowProcW(h: isize, m: u32, w: usize, l: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, h: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostMessageW(h: isize, m: u32, w: usize, l: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn DestroyWindow(h: isize) -> i32;
    fn ShowWindow(h: isize, cmd: i32) -> i32;
    fn SetWindowPos(h: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn GetWindowRect(h: isize, r: *mut Rect) -> i32;
    fn SetLayeredWindowAttributes(h: isize, key: u32, alpha: u8, flags: u32) -> i32;
    fn SetWindowLongPtrW(h: isize, idx: i32, v: isize) -> isize;
    fn GetWindowLongPtrW(h: isize, idx: i32) -> isize;
    fn SetWindowRgn(h: isize, rgn: isize, redraw: i32) -> i32;
    fn InvalidateRect(h: isize, r: *const Rect, erase: i32) -> i32;
    fn BeginPaint(h: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(h: isize, ps: *const PaintStruct) -> i32;
    fn GetDC(h: isize) -> isize;
    fn ReleaseDC(h: isize, dc: isize) -> i32;
    fn DrawTextW(dc: isize, text: *const u16, len: i32, r: *mut Rect, fmt: u32) -> i32;
    fn FillRect(dc: isize, r: *const Rect, brush: isize) -> i32;
    fn LoadCursorW(inst: isize, name: *const u16) -> isize;
    fn GetDpiForWindow(h: isize) -> u32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateFontW(
        h: i32,
        w: i32,
        esc: i32,
        orient: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike: u32,
        charset: u32,
        out: u32,
        clip: u32,
        quality: u32,
        pitch: u32,
        face: *const u16,
    ) -> isize;
    fn SelectObject(dc: isize, obj: isize) -> isize;
    fn DeleteObject(obj: isize) -> i32;
    fn SetBkMode(dc: isize, mode: i32) -> i32;
    fn SetTextColor(dc: isize, color: u32) -> u32;
    fn CreateSolidBrush(color: u32) -> isize;
    fn CreatePen(style: i32, width: i32, color: u32) -> isize;
    fn CreateCompatibleDC(dc: isize) -> isize;
    fn CreateCompatibleBitmap(dc: isize, w: i32, h: i32) -> isize;
    fn BitBlt(dst: isize, x: i32, y: i32, w: i32, h: i32, src: isize, sx: i32, sy: i32, rop: u32) -> i32;
    fn DeleteDC(dc: isize) -> i32;
    fn CreateRoundRectRgn(l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> isize;
    fn GetTextExtentPoint32W(dc: isize, s: *const u16, n: i32, sz: *mut Size) -> i32;
    fn GetStockObject(i: i32) -> isize;
    fn RoundRect(dc: isize, l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        hkey: isize,
        sub: *const u16,
        val: *const u16,
        flags: u32,
        ty: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
}

const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const GWL_EXSTYLE: i32 = -20;
const LWA_ALPHA: u32 = 0x2;
const SW_SHOWNOACTIVATE: i32 = 4;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_NCHITTEST: u32 = 0x0084;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const WM_APP: u32 = 0x8000;
const WM_APP_REFRESH: u32 = WM_APP + 1;
const WM_APP_CLOSE: u32 = WM_APP + 2;
const HTCAPTION: isize = 2;
const DT_LEFT: u32 = 0x0000;
const DT_VCENTER: u32 = 0x0004;
const DT_SINGLELINE: u32 = 0x0020;
const DT_NOPREFIX: u32 = 0x0800;
const DT_END_ELLIPSIS: u32 = 0x8000;
const TRANSPARENT_BK: i32 = 1;
const SRCCOPY: u32 = 0x00CC_0020;
const NULL_BRUSH: i32 = 5;
const PS_SOLID: i32 = 0;
const FW_NORMAL: i32 = 400;
const FW_SEMIBOLD: i32 = 600;
const DEFAULT_CHARSET: u32 = 1;
const CLEARTYPE_QUALITY: u32 = 5;
const IDC_ARROW: usize = 32512;

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

static HWND_VAL: AtomicIsize = AtomicIsize::new(0);
static STARTING: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
static LOCKED: AtomicBool = AtomicBool::new(false);
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static CACHE: Mutex<Cache> = Mutex::new(Cache { status: None, traffic: None });
static LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);

/// 状态缓存里只留绘制需要的字段（不依赖 EngineStatus 是否 Clone）。
#[derive(Clone)]
struct StatusLite {
    connected: bool,
    ssid: String,
    signal: i64,
    band: i32,
    location_denied: bool,
    paused: bool,
    connecting: bool,
    target: Option<String>,
}

struct Cache {
    status: Option<StatusLite>,
    traffic: Option<TrafficSnap>,
}

/// 要画的内容（由 compose() 从缓存 + 设置合成）。
#[derive(Clone)]
struct View {
    ssid: String,
    sub: String,
    signal: i64,
    show_bars: bool,
    traf1: Option<(String, String)>, // (↓ 下行速度, ↑ 上行速度)
    traf2: String,
    dark: bool,
    scale: f64,
    opacity: f64,
    locked: bool,
}

/// 量好的布局（设备像素）。
struct Layout {
    view: View,
    w: i32,
    h: i32,
    px: i32, // 主字号（像素）
    radius: i32,
    bars_x: i32,
    bars_bottom: i32,
    bar_w: i32,
    bar_gap: i32,
    bar_unit_h: i32, // 1em 对应的像素（条高 = 比例 × 它）
    text_x: i32,
    y_ssid: i32,
    h_ssid: i32,
    ssid_w: i32,
    y_sub: i32,
    h_sub: i32,
    y_t1: i32,
    h_t1: i32,
    dn_col: i32,
    gap1: i32,
    y_t2: i32,
    h_t2: i32,
    text_w: i32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

// ---------------------------------------------------------------------------
// 对外接口
// ---------------------------------------------------------------------------

/// 原生悬浮框创建失败过（之后一律用网页版悬浮框）。
pub fn failed() -> bool {
    FAILED.load(Ordering::Relaxed)
}

/// 确保原生悬浮框存在（不存在就创建）；已存在就通知它按最新设置刷新。
pub fn ensure(app: &AppHandle, sh: &Arc<Shared>) {
    let _ = SHARED.set(sh.clone());
    if HWND_VAL.load(Ordering::Relaxed) != 0 || STARTING.swap(true, Ordering::SeqCst) {
        refresh();
        return;
    }
    let (app, sh) = (app.clone(), sh.clone());
    let _ = std::thread::Builder::new()
        .name("wifi-overlay".into())
        .stack_size(512 * 1024)
        .spawn(move || thread_main(app, sh));
}

/// 关闭原生悬浮框（销毁窗口，线程随之退出）。
pub fn close() {
    let h = HWND_VAL.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h, WM_APP_CLOSE, 0, 0);
        }
    }
}

/// 通知窗口线程重新合成内容并绘制（设置、状态、流量变化都走这里）。
pub fn refresh() {
    let h = HWND_VAL.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h, WM_APP_REFRESH, 0, 0);
        }
    }
}

/// 引擎状态更新。
pub fn on_status(s: &EngineStatus) {
    if HWND_VAL.load(Ordering::Relaxed) == 0 {
        // 窗口还没有：先缓存，创建后第一次刷新就能画出来
    }
    if let Ok(mut c) = CACHE.lock() {
        c.status = Some(StatusLite {
            connected: s.connected,
            ssid: s.ssid.clone().unwrap_or_default(),
            signal: s.signal,
            band: s.cur_band,
            location_denied: s.location_denied,
            paused: s.paused,
            connecting: s.connecting,
            target: s.target.clone(),
        });
    }
    refresh();
}

/// 实时流量更新。
pub fn on_traffic(t: &TrafficSnap) {
    if let Ok(mut c) = CACHE.lock() {
        c.traffic = Some(t.clone());
    }
    refresh();
}

/// 重申 / 应用层级（置顶或沉到桌面），不改位置和大小。
pub fn set_layer(top: bool) {
    let h = HWND_VAL.load(Ordering::Relaxed);
    if h != 0 {
        native::set_layer(h, top);
    }
}

/// 原生悬浮框当前是否存在。
pub fn running() -> bool {
    HWND_VAL.load(Ordering::Relaxed) != 0
}

// ---------------------------------------------------------------------------
// 内容合成（与网页版 overlay.ts 的 render() 一致）
// ---------------------------------------------------------------------------

fn band_name(b: i32) -> &'static str {
    match b {
        6 => "6G",
        5 => "5G",
        2 => "2.4G",
        _ => "",
    }
}

fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{} B", n);
    }
    let u = ["KB", "MB", "GB", "TB"];
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < u.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    let s = if v >= 100.0 {
        format!("{:.0}", v)
    } else if v >= 10.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.2}", v)
    };
    format!("{} {}", s, u[i])
}

fn level(signal: i64) -> usize {
    if signal >= 75 {
        4
    } else if signal >= 55 {
        3
    } else if signal >= 35 {
        2
    } else if signal > 0 {
        1
    } else {
        0
    }
}

/// 跟随系统时读注册表：AppsUseLightTheme == 0 表示深色。
fn system_dark() -> bool {
    unsafe {
        let sub = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let val = wide("AppsUseLightTheme");
        let mut data: u32 = 1;
        let mut size: u32 = 4;
        let hkcu = 0x8000_0001u32 as i32 as isize; // HKEY_CURRENT_USER
        let r = RegGetValueW(hkcu, sub.as_ptr(), val.as_ptr(), 0x10, null_mut(), &mut data as *mut u32 as *mut c_void, &mut size);
        r == 0 && data == 0
    }
}

fn compose(sh: &Arc<Shared>) -> View {
    let s = sh.settings.lock().unwrap().clone();
    let (st, tr) = match CACHE.lock() {
        Ok(c) => (c.status.clone(), c.traffic.clone()),
        Err(_) => (None, None),
    };
    let mut ssid = "WiFi 检测中…".to_string();
    let mut sub = String::new();
    let mut signal = 0i64;
    if let Some(st) = &st {
        if st.connecting && st.target.is_some() {
            ssid = format!("正在切换到 {}…", st.target.clone().unwrap_or_default());
            sub = if st.connected { format!("当前 {}", st.ssid) } else { "正在连接".to_string() };
        } else if st.location_denied {
            ssid = "需要开启位置服务".to_string();
            sub = "Windows 需授权才能读取 WiFi".to_string();
        } else if st.connected {
            ssid = st.ssid.clone();
            let band = if st.band > 0 { format!(" · {}", band_name(st.band)) } else { String::new() };
            sub = format!(
                "{}{}{}",
                if s.overlay_show_signal { format!("信号 {}%", st.signal) } else { "已连接".to_string() },
                band,
                if st.paused { " · 已暂停" } else { "" }
            );
            signal = st.signal;
        } else {
            ssid = "未连接".to_string();
            sub = if st.paused { "已暂停自动切换".to_string() } else { "正在寻找网络…".to_string() };
        }
    }
    // 实时流量：只在“已连接、而且流量快照就是这个网络的”时显示
    let mut traf1 = None;
    let mut traf2 = String::new();
    if let (Some(st), Some(t)) = (&st, &tr) {
        if s.overlay_show_traffic && st.connected && t.connected && t.ssid == st.ssid {
            traf1 = Some((format!("↓ {}/s", fmt_bytes(t.rx_bps)), format!("↑ {}/s", fmt_bytes(t.tx_bps))));
            traf2 = format!(
                "本次 {} · 今日 {} · 累计 {}",
                fmt_bytes(t.sess_rx + t.sess_tx),
                fmt_bytes(t.today_rx + t.today_tx),
                fmt_bytes(t.total_rx + t.total_tx)
            );
        }
    }
    let dark = match s.theme.as_str() {
        "Dark" => true,
        "Light" => false,
        _ => system_dark(),
    };
    View {
        ssid,
        sub,
        signal,
        show_bars: s.overlay_show_signal,
        traf1,
        traf2,
        dark,
        scale: s.overlay_scale.clamp(0.5, 3.0),
        opacity: s.overlay_opacity.clamp(0.2, 1.0),
        locked: s.overlay_locked,
    }
}

// ---------------------------------------------------------------------------
// 布局与绘制
// ---------------------------------------------------------------------------

struct Fonts {
    main: isize,
    sub: isize,
    t2: isize,
}

unsafe fn make_font(px: i32, weight: i32) -> isize {
    let face = wide("Microsoft YaHei UI");
    CreateFontW(-px.max(6), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0, face.as_ptr())
}

unsafe fn make_fonts(px: i32) -> Fonts {
    Fonts {
        main: make_font(px, FW_SEMIBOLD),
        sub: make_font((px as f64 * 0.82).round() as i32, FW_NORMAL),
        t2: make_font((px as f64 * 0.76).round() as i32, FW_NORMAL),
    }
}

unsafe fn free_fonts(f: &Fonts) {
    DeleteObject(f.main);
    DeleteObject(f.sub);
    DeleteObject(f.t2);
}

/// 量一段文字的像素宽高。
unsafe fn measure(dc: isize, font: isize, text: &str) -> (i32, i32) {
    let old = SelectObject(dc, font);
    let w: Vec<u16> = text.encode_utf16().collect();
    let mut sz = Size { cx: 0, cy: 0 };
    GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32, &mut sz);
    SelectObject(dc, old);
    (sz.cx, sz.cy)
}

unsafe fn build_layout(view: View, dpi: u32) -> Layout {
    let px = (14.0 * view.scale * dpi.max(96) as f64 / 96.0).round() as i32;
    let em = px as f64;
    let f = make_fonts(px);
    let dc = GetDC(0);

    let (ssid_w_full, h_ssid) = measure(dc, f.main, &view.ssid);
    let ssid_w = ssid_w_full.min((17.0 * em) as i32); // 与网页版一致：最宽 17em，超出省略
    let (sub_w, h_sub) = measure(dc, f.sub, &view.sub);
    let sub_em = 0.82 * em;
    let mut text_w = ssid_w.max(sub_w);
    let (mut h_t1, mut h_t2, mut dn_col, mut gap1) = (0, 0, 0, 0);
    if let Some((dn, up)) = &view.traf1 {
        let (dn_w, th) = measure(dc, f.sub, dn);
        let (up_w, _) = measure(dc, f.sub, up);
        h_t1 = th;
        gap1 = (0.9 * sub_em) as i32;
        dn_col = dn_w.max((6.6 * sub_em) as i32); // 下行列最小宽度：速度数字变化时窗口不抖
        let t1_w = (dn_col + gap1 + up_w).max((14.0 * sub_em) as i32);
        let (t2_w, th2) = measure(dc, f.t2, &view.traf2);
        h_t2 = th2;
        text_w = text_w.max(t1_w).max(t2_w);
    }
    ReleaseDC(0, dc);
    free_fonts(&f);

    let pad_x = (1.0 * em) as i32;
    let pad_y = (0.7 * em) as i32;
    let gap = (0.85 * em) as i32;
    let bar_w = ((0.28 * em).round() as i32).max(2);
    let bar_gap = ((0.14 * em).round() as i32).max(1);
    let bars_w = if view.show_bars { 4 * bar_w + 3 * bar_gap } else { 0 };
    let left = pad_x + if view.show_bars { bars_w + gap } else { 0 };

    let mut text_h = h_ssid + h_sub + 1;
    if view.traf1.is_some() {
        text_h += 3 + h_t1 + 1 + h_t2;
    }
    let w = left + text_w + pad_x;
    let h = pad_y * 2 + text_h;
    let y0 = pad_y;
    let y_ssid = y0;
    let y_sub = y_ssid + h_ssid + 1;
    let y_t1 = y_sub + h_sub + 3;
    let y_t2 = y_t1 + h_t1 + 1;
    let bar_h_full = (1.15 * em).round() as i32;
    let bars_bottom = h / 2 + bar_h_full / 2;

    Layout {
        view,
        w,
        h,
        px,
        radius: (0.85 * em) as i32,
        bars_x: pad_x,
        bars_bottom,
        bar_w,
        bar_gap,
        bar_unit_h: px,
        text_x: left,
        y_ssid,
        h_ssid,
        ssid_w,
        y_sub,
        h_sub,
        y_t1,
        h_t1,
        dn_col,
        gap1,
        y_t2,
        h_t2,
        text_w,
    }
}

fn colors(dark: bool) -> (u32, u32, u32, u32, u32, u32, u32, u32, u32) {
    // (背景, 边框, 主文字, 次文字, 条轨道, 好, 警告, 差, —)
    if dark {
        (
            rgb(22, 26, 35),
            rgb(69, 72, 79),
            rgb(230, 233, 240),
            rgb(138, 147, 166),
            rgb(69, 72, 79),
            rgb(61, 220, 151),
            rgb(255, 200, 87),
            rgb(255, 107, 107),
            0,
        )
    } else {
        (
            rgb(255, 255, 255),
            rgb(224, 224, 224),
            rgb(27, 31, 39),
            rgb(102, 112, 133),
            rgb(217, 217, 217),
            rgb(18, 183, 106),
            rgb(247, 144, 9),
            rgb(240, 68, 56),
            0,
        )
    }
}

unsafe fn draw_text(dc: isize, font: isize, color: u32, text: &str, x: i32, y: i32, w: i32, h: i32) {
    let old = SelectObject(dc, font);
    SetTextColor(dc, color);
    let t: Vec<u16> = text.encode_utf16().collect();
    let mut r = Rect { left: x, top: y, right: x + w, bottom: y + h };
    DrawTextW(dc, t.as_ptr(), t.len() as i32, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    SelectObject(dc, old);
}

unsafe fn paint(hwnd: isize) {
    let mut ps: PaintStruct = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc == 0 {
        return;
    }
    if let Ok(g) = LAYOUT.lock() {
        if let Some(l) = g.as_ref() {
            draw(hdc, l);
        }
    }
    EndPaint(hwnd, &ps);
}

unsafe fn draw(hdc: isize, l: &Layout) {
    let (bg, border, tx, tx2, track, good, warn, bad, _) = colors(l.view.dark);
    // 双缓冲：先画到内存位图，再一次性贴到窗口，避免闪烁
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, l.w, l.h);
    let old_bmp = SelectObject(mem, bmp);

    let full = Rect { left: 0, top: 0, right: l.w, bottom: l.h };
    let bg_brush = CreateSolidBrush(bg);
    FillRect(mem, &full, bg_brush);
    DeleteObject(bg_brush);
    // 边框（圆角；窗口区域本身也是同样的圆角，见 apply）
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_pen = SelectObject(mem, pen);
    let old_brush = SelectObject(mem, GetStockObject(NULL_BRUSH));
    RoundRect(mem, 0, 0, l.w, l.h, l.radius * 2, l.radius * 2);
    SelectObject(mem, old_brush);
    SelectObject(mem, old_pen);
    DeleteObject(pen);

    SetBkMode(mem, TRANSPARENT_BK);
    let f = make_fonts(l.px);

    // 信号强度四格
    if l.view.show_bars {
        let lv = level(l.view.signal);
        let fill = match lv {
            1 => bad,
            2 => warn,
            _ => good,
        };
        let ratios = [0.36, 0.62, 0.88, 1.15];
        for i in 0..4usize {
            let bh = (ratios[i] * l.bar_unit_h as f64).round() as i32;
            let x = l.bars_x + i as i32 * (l.bar_w + l.bar_gap);
            let r = Rect { left: x, top: l.bars_bottom - bh, right: x + l.bar_w, bottom: l.bars_bottom };
            let b = CreateSolidBrush(if i < lv { fill } else { track });
            FillRect(mem, &r, b);
            DeleteObject(b);
        }
    }

    draw_text(mem, f.main, tx, &l.view.ssid, l.text_x, l.y_ssid, l.ssid_w + 2, l.h_ssid);
    draw_text(mem, f.sub, tx2, &l.view.sub, l.text_x, l.y_sub, l.text_w, l.h_sub);
    if let Some((dn, up)) = &l.view.traf1 {
        draw_text(mem, f.sub, good, dn, l.text_x, l.y_t1, l.dn_col, l.h_t1);
        draw_text(mem, f.sub, warn, up, l.text_x + l.dn_col + l.gap1, l.y_t1, l.text_w - l.dn_col - l.gap1, l.h_t1);
        draw_text(mem, f.t2, tx2, &l.view.traf2, l.text_x, l.y_t2, l.text_w, l.h_t2);
    }
    free_fonts(&f);

    BitBlt(hdc, 0, 0, l.w, l.h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    DeleteObject(bmp);
    DeleteDC(mem);
}

// ---------------------------------------------------------------------------
// 窗口：位置 / 大小 / 透明度 / 锁定
// ---------------------------------------------------------------------------

/// 重新合成内容、量布局，应用大小 / 位置 / 透明度 / 锁定 / 层级，并重绘。只在窗口线程调用。
unsafe fn relayout(hwnd: isize) {
    let sh = match SHARED.get() {
        Some(s) => s.clone(),
        None => return,
    };
    let view = compose(&sh);
    let dpi = GetDpiForWindow(hwnd);
    let l = build_layout(view, dpi);
    let (w, h) = (l.w.max(40), l.h.max(30));

    // 位置（与网页版 overlay::reposition 一致）
    let s = sh.settings.lock().unwrap().clone();
    let scale = dpi.max(96) as f64 / 96.0;
    let m = (6.0 * scale) as i32;
    let (wl, wt, wr, wb) = native::work_area();
    let (x, y) = match s.overlay_anchor.as_str() {
        "TopLeft" => (wl + m, wt + m),
        "BottomLeft" => (wl + m, wb - h - m),
        "BottomRight" => (wr - w - m, wb - h - m),
        "Free" => {
            let (vx, vy, vw, vh) = native::virtual_screen();
            let fx = (s.overlay_x * scale) as i32;
            let fy = (s.overlay_y * scale) as i32;
            (fx.clamp(vx, (vx + vw - w).max(vx)), fy.clamp(vy, (vy + vh - h).max(vy)))
        }
        _ => (wr - w - m, wt + m), // TopRight
    };

    LOCKED.store(l.view.locked, Ordering::Relaxed);
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let want = if l.view.locked { ex | WS_EX_TRANSPARENT as isize } else { ex & !(WS_EX_TRANSPARENT as isize) };
    if want != ex {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want); // 锁定 = 鼠标穿透
    }
    SetLayeredWindowAttributes(hwnd, 0, (l.view.opacity * 255.0).round() as u8, LWA_ALPHA);

    let mut cur = Rect { left: 0, top: 0, right: 0, bottom: 0 };
    GetWindowRect(hwnd, &mut cur);
    let resized = cur.right - cur.left != w || cur.bottom - cur.top != h;
    let moved = cur.left != x || cur.top != y;
    if resized || moved {
        SetWindowPos(hwnd, 0, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
    if resized {
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, l.radius * 2, l.radius * 2);
        SetWindowRgn(hwnd, rgn, 1); // 区域交给系统，不用自己释放
    }
    let top = s.overlay_layer != "Desktop";
    if let Ok(mut g) = LAYOUT.lock() {
        *g = Some(l);
    }
    native::set_layer(hwnd, top);
    InvalidateRect(hwnd, null(), 0);
}

/// 用户拖动结束：切换为“自由位置”并记住（存逻辑像素，与网页版 / WPF 版一致）。
unsafe fn user_moved(hwnd: isize) {
    let sh = match SHARED.get() {
        Some(s) => s.clone(),
        None => return,
    };
    let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
    if GetWindowRect(hwnd, &mut r) == 0 {
        return;
    }
    let scale = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
    if let Ok(mut s) = sh.settings.lock() {
        s.overlay_anchor = "Free".to_string();
        s.overlay_x = r.left as f64 / scale;
        s.overlay_y = r.top as f64 / scale;
    }
    sh.settings_dirty.store(true, Ordering::Relaxed);
}

unsafe extern "system" fn wnd_proc(h: isize, m: u32, w: usize, l: isize) -> isize {
    match m {
        WM_PAINT => {
            paint(h);
            0
        }
        WM_ERASEBKGND => 1,
        WM_NCHITTEST => {
            if LOCKED.load(Ordering::Relaxed) {
                DefWindowProcW(h, m, w, l)
            } else {
                HTCAPTION // 整块区域都能拖动
            }
        }
        WM_EXITSIZEMOVE => {
            user_moved(h);
            0
        }
        WM_APP_REFRESH => {
            relayout(h);
            0
        }
        WM_APP_CLOSE => {
            DestroyWindow(h);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

fn thread_main(app: AppHandle, sh: Arc<Shared>) {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = wide("WifiPriorityOverlay");
        let wc = WndClassExW {
            cb_size: size_of::<WndClassExW>() as u32,
            style: 0,
            lpfn_wnd_proc: Some(wnd_proc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: hinst,
            h_icon: 0,
            h_cursor: LoadCursorW(0, IDC_ARROW as *const u16),
            hbr_background: 0,
            lpsz_menu_name: null(),
            lpsz_class_name: class.as_ptr(),
            h_icon_sm: 0,
        };
        RegisterClassExW(&wc); // 第二次启动线程时类已存在，返回 0，不影响后面创建窗口
        let title = wide("WiFi 悬浮框");
        let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
        let hwnd = CreateWindowExW(ex, class.as_ptr(), title.as_ptr(), WS_POPUP, 0, 0, 10, 10, 0, 0, hinst, null_mut());
        if hwnd == 0 {
            // 创建失败：以后都用网页版悬浮框
            FAILED.store(true, Ordering::Relaxed);
            STARTING.store(false, Ordering::SeqCst);
            crate::overlay::apply(&app, &sh);
            return;
        }
        HWND_VAL.store(hwnd, Ordering::Relaxed);
        STARTING.store(false, Ordering::SeqCst);
        relayout(hwnd);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut msg: Msg = zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = HWND_VAL.compare_exchange(hwnd, 0, Ordering::SeqCst, Ordering::SeqCst);
        if let Ok(mut g) = LAYOUT.lock() {
            *g = None;
        }
    }
    // 窗口因为关闭被销毁了：如果设置里仍然要显示（比如快速开关了一次），按当前设置重新应用
    crate::overlay::apply(&app, &sh);
}
