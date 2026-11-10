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
        public bool InternetCheck { get; set; } = false;   // 连上但无法上网时视为不可用
        public bool Notify { get; set; } = true;           // 切换时弹出托盘通知

        // 频段偏好
        // BandPolicy: Off = 不区分频段；Same = 仅同一网络内优先偏好频段；Global = 全局偏好频段的网络优先
        public string BandPolicy { get; set; } = "Global";
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
        public string OverlayLayer { get; set; } = "Top";  // Top = 悬浮在所有窗口之上；Desktop = 只在桌面
    }

    public static class SettingsStore
    {
        const int CurrentSchema = 2;

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
                            // v1.1.1：毫秒级切换，旧版的 10 秒/2 次/60 秒默认值会拖慢切换，统一迁移
                            s.ScanIntervalSec = 3;
                            s.ConfirmMs = 400;
                            s.CooldownSec = 8;
                            s.SchemaVersion = CurrentSchema;
                        }
                        return s;
                    }
                }
            }
            catch { }
            return new AppSettings { SchemaVersion = CurrentSchema };
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
