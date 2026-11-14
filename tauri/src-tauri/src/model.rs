//! 数据模型。Settings 的 JSON 字段名与 WPF 版（System.Text.Json，PascalCase）完全一致，
//! 所以两个版本可以共用 %AppData%\WifiPriority\settings.json，随时来回切换。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

pub const CURRENT_SCHEMA: i64 = 5;

/// 启动时从旧配置迁移过“频段策略”（Global → Same）的标记，引擎启动后写一条日志告知用户。
pub static BAND_POLICY_MIGRATED: AtomicBool = AtomicBool::new(false);

/// 启动时从旧配置迁移过“联网检测默认开启”（schema < 4）的标记，引擎启动后写一条日志告知用户。
pub static INTERNET_CHECK_MIGRATED: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    pub priorities: Vec<String>,

    // 切换策略
    pub scan_interval_sec: i64,
    pub up_threshold: i64,
    pub min_connect_signal: i64, // 连接任何“新网络”所需的最低信号（%）；低于它不会去连
    pub stale_filter: bool,      // 忽略 Windows 缓存里已经消失的网络（关掉的热点）
    pub down_threshold: i64,
    pub confirm_ms: i64,
    pub cooldown_sec: i64,
    pub manual_pause_minutes: i64,
    pub takeover_unlisted: bool,
    pub internet_check: bool,
    // v1.1.8 链路健康检测（硬件读数 + 网关 + 公网分层检测）
    pub stall_ms: i64,             // 入站字节停滞多久算“怀疑”
    pub heartbeat_ms: i64,         // 空闲时网关心跳间隔（有入站流量时不发）
    pub wan_interval_sec: i64,     // 健康时公网探测间隔
    pub wan_timeout_ms: i64,       // 单次公网探测超时
    pub ip_wait_ms: i64,           // 刚连上后等 DHCP/网关的上限
    pub unusable_backoff_sec: i64, // 判死后退避基数（15→30→60→…→300 秒），0 = 关闭退避
    pub disc_grace_ms: i64,        // 断开后的让路窗口（信号弱 / 扫描证明原网络已消失时自动为 0）
    // v1.1.9
    pub hunt_scan_ms: i64,         // 有更高优先级网络“还没出现”时的扫描间隔（毫秒；热点一开就能连上）；0 = 不加速，沿用 ScanIntervalSec
    pub overlay_show_traffic: bool, // 悬浮框显示实时流量与用量
    pub overlay_native: bool,       // v1.2.0：悬浮框用原生窗口（GDI）绘制，不创建 WebView2——后台内存占用低一个数量级；关掉则用网页版悬浮框
    pub notify: bool,

    // 频段偏好：Off / Same / Global；5G / 2.4G
    pub band_policy: String,
    pub prefer_band: String,

    // 外观与配置版本
    pub theme: String,
    pub schema_version: i64,

    // 桌面悬浮框
    pub overlay_enabled: bool,
    pub overlay_locked: bool,
    pub overlay_show_signal: bool,
    pub overlay_opacity: f64,
    pub overlay_scale: f64,
    pub overlay_anchor: String,
    pub overlay_x: f64,
    pub overlay_y: f64,
    pub overlay_layer: String, // Top = 悬浮在所有窗口之上；Desktop = 只在桌面
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            priorities: Vec::new(),
            scan_interval_sec: 3,
            up_threshold: 60,
            min_connect_signal: 50,
            stale_filter: true,
            down_threshold: 40,
            confirm_ms: 400,
            cooldown_sec: 8,
            manual_pause_minutes: 10,
            takeover_unlisted: true,
            internet_check: true,
            stall_ms: 1000,
            heartbeat_ms: 1500,
            wan_interval_sec: 8,
            wan_timeout_ms: 900,
            ip_wait_ms: 8000,
            unusable_backoff_sec: 15,
            disc_grace_ms: 400,
            hunt_scan_ms: 1500,
            overlay_show_traffic: true,
            overlay_native: true,
            notify: true,
            band_policy: "Same".into(),
            prefer_band: "5G".into(),
            theme: "System".into(),
            schema_version: 0, // 0 = 配置文件里没有这个字段（旧版本写的）
            overlay_enabled: true,
            overlay_locked: false,
            overlay_show_signal: true,
            overlay_opacity: 0.95,
            overlay_scale: 1.0,
            overlay_anchor: "TopRight".into(),
            overlay_x: 0.0,
            overlay_y: 0.0,
            overlay_layer: "Top".into(),
        }
    }
}

impl Settings {
    /// 全新安装用的默认值（已经是最新配置版本）。
    pub fn fresh() -> Settings {
        Settings { schema_version: CURRENT_SCHEMA, ..Settings::default() }
    }

    /// 解析配置文件内容；旧版本（v1.1.0 及以前）的配置会迁移成快速默认值。
    pub fn from_json(text: &str) -> Option<Settings> {
        let mut s: Settings = serde_json::from_str(text).ok()?;
        if s.schema_version < CURRENT_SCHEMA {
            if s.schema_version < 2 {
                // v1.1.1：毫秒级切换，旧版的 10 秒/2 次/60 秒默认值会拖慢切换
                s.scan_interval_sec = 3;
                s.confirm_ms = 400;
                s.cooldown_sec = 8;
            }
            // v1.1.3：旧默认“全局偏好频段优先”会让当前的 5G 网络排到列表里更靠前的 2.4G 网络之前，
            // 于是更高优先级的网络出现时“最优”仍是当前网络，永远不切换。改为列表顺序绝对优先。
            if s.band_policy == "Global" {
                s.band_policy = "Same".into();
                BAND_POLICY_MIGRATED.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // v1.1.8：新的联网检测（硬件读数 + 绑定网卡的分层探测）不会再把刚连上的热点误判成“无法上网”，默认开启。
            if s.schema_version < 4 {
                if !s.internet_check {
                    s.internet_check = true;
                    INTERNET_CHECK_MIGRATED.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            // v1.2.0：回落更快。停滞阈值改为“距最后一次收到数据”的墙钟时间；断线让路 1200 → 400 ms。
            // 只迁移还停留在旧默认值上的设置，用户自己改过的不动。
            if s.schema_version < 5 {
                if s.stall_ms == 1500 {
                    s.stall_ms = 1000;
                }
                if s.heartbeat_ms == 2000 {
                    s.heartbeat_ms = 1500;
                }
                if s.disc_grace_ms == 1200 {
                    s.disc_grace_ms = 400;
                }
            }
            s.schema_version = CURRENT_SCHEMA;
        }
        s.sanitize();
        Some(s)
    }

    /// 把各项限制在合法范围内。
    pub fn sanitize(&mut self) {
        self.scan_interval_sec = self.scan_interval_sec.clamp(0, 600);
        self.min_connect_signal = self.min_connect_signal.clamp(0, 100);
        self.up_threshold = self.up_threshold.clamp(10, 100);
        self.down_threshold = self.down_threshold.clamp(0, 95);
        if self.down_threshold > self.up_threshold {
            self.down_threshold = self.up_threshold;
        }
        self.confirm_ms = self.confirm_ms.clamp(0, 10_000);
        self.cooldown_sec = self.cooldown_sec.clamp(0, 3600);
        self.manual_pause_minutes = self.manual_pause_minutes.clamp(0, 1440);
        self.stall_ms = self.stall_ms.clamp(500, 10_000);
        self.heartbeat_ms = self.heartbeat_ms.clamp(500, 30_000);
        self.wan_interval_sec = self.wan_interval_sec.clamp(2, 120);
        self.wan_timeout_ms = self.wan_timeout_ms.clamp(300, 5_000);
        self.ip_wait_ms = self.ip_wait_ms.clamp(2_000, 30_000);
        self.unusable_backoff_sec = self.unusable_backoff_sec.clamp(0, 600);
        self.disc_grace_ms = self.disc_grace_ms.clamp(0, 5_000);
        if self.hunt_scan_ms != 0 {
            self.hunt_scan_ms = self.hunt_scan_ms.clamp(300, 10_000);
        }
        self.overlay_opacity = self.overlay_opacity.clamp(0.2, 1.0);
        self.overlay_scale = self.overlay_scale.clamp(0.7, 2.0);
        if !matches!(self.band_policy.as_str(), "Off" | "Same" | "Global") {
            self.band_policy = "Same".into();
        }
        if !matches!(self.prefer_band.as_str(), "5G" | "2.4G") {
            self.prefer_band = "5G".into();
        }
        if !matches!(self.theme.as_str(), "System" | "Light" | "Dark") {
            self.theme = "System".into();
        }
        if !matches!(
            self.overlay_anchor.as_str(),
            "TopLeft" | "TopRight" | "BottomLeft" | "BottomRight" | "Free"
        ) {
            self.overlay_anchor = "TopRight".into();
        }
        if !matches!(self.overlay_layer.as_str(), "Top" | "Desktop") {
            self.overlay_layer = "Top".into();
        }
        self.priorities.retain(|s| !s.trim().is_empty());
    }
}

// ---------------------------------------------------------------------------
// WiFi 状态（前端用 camelCase）
// ---------------------------------------------------------------------------

/// 一个接入点（BSSID）。band：2 = 2.4GHz，5 = 5GHz，6 = 6GHz，0 = 未知。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct BssInfo {
    pub bssid: String,
    pub signal: i64, // 0-100
    pub band: i32,
    pub chan: i32,
    pub rssi: i32,
    pub iface: i32,
    #[serde(skip)]
    pub host_ts: u64, // WLAN_BSS_ENTRY.ullHostTimestamp：收到这个接入点最近一帧的时间戳（0 = 驱动没给）
    #[serde(skip)]
    pub changed_at: i64, // 我们观察到该接入点时间戳最近一次变化的时间（引擎时钟 ms）；-1 = 无法判断（驱动没给时间戳）；0 = 启动时缓存里的、从未观察到变化
    #[serde(skip)]
    pub stale: bool, // true = 缓存里的“幽灵”条目（接入点其实已经不在了），由 Freshness 标记
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NearbyNetwork {
    pub ssid: String,
    pub signal: i64,
    pub bss: Vec<BssInfo>,
    pub bands: String,     // "5G 82%  ·  2.4G 64%"
    pub band_tags: String, // "5G/2.4G"
    pub profile: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WifiState {
    pub connected: bool,
    pub connecting: bool,
    pub fast: bool, // true = 原生 WLAN 接口；false = netsh
    pub ssid: String,
    pub signal: i64,
    pub cur_bssid: Option<String>,
    pub cur_band: i32,
    pub cur_iface: i32,
    pub cur_profile: Option<String>,
    pub location_denied: bool,
    pub nearby: Vec<NearbyNetwork>,
    pub profiles: HashSet<String>,
}

impl WifiState {
    pub fn new() -> WifiState {
        WifiState {
            connected: false,
            connecting: false,
            fast: false,
            ssid: String::new(),
            signal: 0,
            cur_bssid: None,
            cur_band: 0,
            cur_iface: -1,
            cur_profile: None,
            location_denied: false,
            nearby: Vec::new(),
            profiles: HashSet::new(),
        }
    }
}

impl Default for WifiState {
    fn default() -> Self {
        WifiState::new()
    }
}

/// 发给前端的状态快照。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub connected: bool,
    pub ssid: Option<String>,
    pub signal: i64,
    pub cur_band: i32,
    pub cur_bssid: Option<String>,
    pub location_denied: bool,
    pub paused: bool,
    pub paused_until: Option<i64>, // unix 秒；None + paused = 手动无限期暂停
    pub connecting: bool,          // 正在关联/认证，或程序刚发出连接请求还没落定
    pub target: Option<String>,    // 程序正在切换到的网络（含频段标签）
    pub message: Option<String>,
    pub native: bool,
    pub nearby: Vec<NearbyNetwork>,
    pub profiles: Vec<String>,
}
