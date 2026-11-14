using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;

namespace WifiPriority.Models
{
    public class AppSettings
    {
        // 优先级列表（越靠前优先级越高，保存的是 WiFi 名称 SSID，要求与系统配置文件名一致）
        public List<string> Priorities { get; set; } = new List<string>();

        // 切换策略
        public int ScanIntervalSec { get; set; } = 3;     // 主动扫描间隔（秒），0 = 连续扫描（最快）
        public int UpThreshold { get; set; } = 60;        // 目标网络信号 >= 此值才会切过去（百分比）
        public int DownThreshold { get; set; } = 40;      // 当前网络信号 < 此值视为过弱/丢失
        public int ConfirmMs { get; set; } = 400;         // 候选网络持续保持最优多少毫秒才切换（0 = 立即）
        public int CooldownSec { get; set; } = 8;         // 两次“升级”切换之间的最短间隔
        public int ManualPauseMinutes { get; set; } = 10; // 检测到手动切换后暂停自动切换的分钟数，0 = 不暂停
        public bool TakeoverUnlisted { get; set; } = true; // 当前连接的网络不在列表中时，是否也切到列表中的网络
        public bool InternetCheck { get; set; } = true;    // 联网检测：链路/网关/公网不可用时视为不可用（v1.1.8 起默认开启）
        // v1.1.8 链路健康检测（硬件读数 + 网关 + 公网分层检测）；字段名与 Tauri 版一致，两版共用 settings.json
        public int StallMs { get; set; } = 1000;           // 距最后一次收到数据多久算“怀疑”（墙钟）
        public int HeartbeatMs { get; set; } = 1500;       // 完全空闲时，多久没收到任何数据就去 ping 一次网关
        public int WanIntervalSec { get; set; } = 8;       // 健康时公网探测间隔
        public int WanTimeoutMs { get; set; } = 900;       // 单次公网探测超时
        public int IpWaitMs { get; set; } = 8000;          // 刚连上后等 DHCP/网关的上限
        public int UnusableBackoffSec { get; set; } = 15;  // 判死后退避基数（15→30→…→300 秒），0 = 关闭退避
        public int DiscGraceMs { get; set; } = 400;        // 断开后的让路窗口（信号弱 / 扫描证明原网络已消失时自动为 0）
        // v1.1.9
        public int HuntScanMs { get; set; } = 1500;        // 有更高优先级网络“还没出现”时的扫描间隔（毫秒；热点一开就能连上）；0 = 不加速，沿用 ScanIntervalSec
        public bool OverlayShowTraffic { get; set; } = true;   // 悬浮框显示实时流量与用量
        public bool Notify { get; set; } = true;           // 切换时弹出托盘通知

        // 频段偏好
        // BandPolicy: Off = 不区分频段；Same = 仅同一网络内优先偏好频段；Global = 全局偏好频段的网络优先
        public string BandPolicy { get; set; } = "Same";
        public string PreferBand { get; set; } = "5G";     // 5G（6G > 5G > 2.4G）或 2.4G

        // 外观
        public string Theme { get; set; } = "System";      // System / Light / Dark

        // 配置版本：用来在升级后把旧的“慢速”默认值迁移成新的快速默认值
        public int SchemaVersion { get; set; } = 0;

        // 桌面悬浮框
        public bool OverlayEnabled { get; set; } = true;
        public bool OverlayLocked { get; set; } = false;   // 锁定 = 不可拖动 + 鼠标穿透
        public bool OverlayShowSignal { get; set; } = true;
        public double OverlayOpacity { get; set; } = 0.95;
        public double OverlayScale { get; set; } = 1.0;
        public string OverlayAnchor { get; set; } = "TopRight"; // TopLeft/TopRight/BottomLeft/BottomRight/Free
        public double OverlayX { get; set; } = 0;
        public double OverlayY { get; set; } = 0;
        public int MinConnectSignal { get; set; } = 50;   // 连接任何“新网络”所需的最低信号（%）；低于它不会去连
        public bool StaleFilter { get; set; } = true;     // 忽略 Windows 缓存里已经消失的网络（关掉的热点）
        public string OverlayLayer { get; set; } = "Top";  // Top = 悬浮在所有窗口之上；Desktop = 只在桌面
    }

    public static class SettingsStore
    {
        const int CurrentSchema = 5;

        /// <summary>启动时把旧配置里的“频段策略”从 Global 迁移成了 Same（引擎启动后写一条日志告知用户）。</summary>
        public static bool BandPolicyMigrated;

        /// <summary>启动时把旧配置里的“联网检测”改成了默认开启（引擎启动后写一条日志告知用户）。</summary>
        public static bool InternetCheckMigrated;

        static readonly object Lk = new object();

        static string Dir =>
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "WifiPriority");

        static string FilePath => Path.Combine(Dir, "settings.json");

        public static AppSettings Load()
        {
            try
            {
                if (File.Exists(FilePath))
                {
                    var s = JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(FilePath));
                    if (s != null)
                    {
                        if (s.Priorities == null) s.Priorities = new List<string>();
                        if (s.SchemaVersion < CurrentSchema)
                        {
                            if (s.SchemaVersion < 2)
                            {
                                // v1.1.1：毫秒级切换，旧版的 10 秒/2 次/60 秒默认值会拖慢切换
                                s.ScanIntervalSec = 3;
                                s.ConfirmMs = 400;
                                s.CooldownSec = 8;
                            }
                            // v1.1.3：旧默认“全局偏好频段优先”会让当前的 5G 网络排到列表里更靠前的 2.4G 网络之前，
                            // 于是更高优先级的网络出现时“最优”仍是当前网络，永远不切换。改为列表顺序绝对优先。
                            if (s.BandPolicy == "Global")
                            {
                                s.BandPolicy = "Same";
                                BandPolicyMigrated = true;
                            }
                            // v1.1.8：新的联网检测不会再把刚连上的热点误判成“无法上网”，默认开启
                            if (s.SchemaVersion < 4 && !s.InternetCheck)
                            {
                                s.InternetCheck = true;
                                InternetCheckMigrated = true;
                            }
                            // v1.2.0：回落更快。只迁移还停留在旧默认值上的设置，用户自己改过的不动
                            if (s.SchemaVersion < 5)
                            {
                                if (s.StallMs == 1500) s.StallMs = 1000;
                                if (s.HeartbeatMs == 2000) s.HeartbeatMs = 1500;
                                if (s.DiscGraceMs == 1200) s.DiscGraceMs = 400;
                            }
                            s.SchemaVersion = CurrentSchema;
                        }
                        Sanitize(s);
                        return s;
                    }
                }
            }
            catch { }
            return new AppSettings { SchemaVersion = CurrentSchema };
        }

        static int Clamp(int v, int lo, int hi) => v < lo ? lo : (v > hi ? hi : v);

        /// <summary>把 v1.1.8 新增的各项限制在合法范围内（与 Tauri 版 Settings::sanitize 一致）。</summary>
        static void Sanitize(AppSettings s)
        {
            s.StallMs = Clamp(s.StallMs, 500, 10000);
            s.HeartbeatMs = Clamp(s.HeartbeatMs, 500, 30000);
            s.WanIntervalSec = Clamp(s.WanIntervalSec, 2, 120);
            s.WanTimeoutMs = Clamp(s.WanTimeoutMs, 300, 5000);
            s.IpWaitMs = Clamp(s.IpWaitMs, 2000, 30000);
            s.UnusableBackoffSec = Clamp(s.UnusableBackoffSec, 0, 600);
            s.DiscGraceMs = Clamp(s.DiscGraceMs, 0, 5000);
            if (s.HuntScanMs != 0) s.HuntScanMs = Clamp(s.HuntScanMs, 300, 10000);
        }

        public static void Save(AppSettings s)
        {
            lock (Lk)
            {
                try
                {
                    Directory.CreateDirectory(Dir);
                    File.WriteAllText(FilePath, JsonSerializer.Serialize(s, new JsonSerializerOptions { WriteIndented = true }));
                }
                catch { }
            }
        }
    }
}
