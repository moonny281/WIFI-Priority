using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Security;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;

namespace WifiPriority.Services
{
    /// <summary>一个接入点（BSSID）的扫描结果。Band：2 = 2.4GHz，5 = 5GHz，6 = 6GHz，0 = 未知。</summary>
    public class BssInfo
    {
        public string Bssid;
        public int Signal;   // 0-100（Windows 信号质量百分比）
        public int Band;
        public int Chan;
        public int Rssi;     // dBm（原生读取时才有）
        public int Iface;    // 来自第几块无线网卡
        public ulong HostTs; // WLAN_BSS_ENTRY.ullHostTimestamp：收到这个接入点最近一帧的时间戳（0 = 驱动没给）
        public long ChangedAt; // 我们观察到该接入点时间戳最近一次变化的时间（引擎时钟 ms）；-1 = 无法判断（驱动没给时间戳）；0 = 启动时缓存里的、从未观察到变化
        public bool Stale;   // true = 缓存里的“幽灵”条目（接入点其实已经不在了），由 BssFreshness 标记
    }

    /// <summary>一个 WiFi 名称（SSID）下所有接入点的汇总。</summary>
    public class NearbyNetwork
    {
        public string Ssid;
        public int Signal;                 // 所有频段中最强的信号
        public List<BssInfo> Bss = new List<BssInfo>();
        public string Bands = "";          // 例如 "5G 82%  ·  2.4G 64%"
        public string BandTags = "";       // 例如 "5G/2.4G"
        public string Profile;             // 已保存的配置文件名（没有则为 null）
    }

    public class WifiState
    {
        public bool Connected;
        public string Ssid;
        public int Signal;
        public string CurBssid;
        public int CurBand;
        public int CurIface = -1;
        public string CurProfile;
        public bool Connecting;            // 正在关联/认证中（原生读取才有）
        public bool Fast;                  // true = 来自原生 WLAN 接口，false = 来自 netsh
        public bool LocationDenied;
        public List<NearbyNetwork> Nearby = new List<NearbyNetwork>();
        public HashSet<string> Profiles = new HashSet<string>();
    }

    public class EngineStatus
    {
        public bool Connected;
        public string Ssid;
        public int Signal;
        public int CurBand;
        public string CurBssid;
        public bool LocationDenied;
        public bool Paused;
        public DateTime? PausedUntil;   // null 且 Paused = 手动无限期暂停
        public bool Connecting;         // 正在关联/认证，或程序刚发出连接请求还没落定
        public string Target;           // 程序正在切换到的网络（含频段标签）
        public string Message;
        public List<NearbyNetwork> Nearby = new List<NearbyNetwork>();
        public HashSet<string> Profiles = new HashSet<string>();
    }

    /// <summary>
    /// 通过 netsh 读取 / 控制 WiFi。解析时只依赖行结构、数字和 "GHz"（不依赖本地化文字），中英文系统都能用。
    /// </summary>
    public static class Wifi
    {
        const string Mac = @"([0-9A-Fa-f]{2}(?:[:\-][0-9A-Fa-f]{2}){5})";

        static readonly Regex SsidNum = new Regex(@"^\s*SSID\s+\d+\s*[:：]\s*(.*?)\s*$", RegexOptions.IgnoreCase | RegexOptions.Compiled);
        static readonly Regex SsidOnly = new Regex(@"^\s*SSID\s*[:：]\s*(.*?)\s*$", RegexOptions.IgnoreCase | RegexOptions.Compiled);
        static readonly Regex BssidNum = new Regex(@"^\s*BSSID\s+\d+\s*[:：]\s*" + Mac, RegexOptions.IgnoreCase | RegexOptions.Compiled);
        // “BSSID”在新版 Windows 里可能叫 “AP BSSID”，所以不认标签，只认“冒号 + MAC 地址 + 行尾”
        static readonly Regex MacLine = new Regex(@"[:：]\s*" + Mac + @"\s*$", RegexOptions.Compiled);
        // 整行以 “: 85%” 结尾才算信号（排除 “Channel Utilization: 25 (9 %)” 这类行）
        static readonly Regex PctLine = new Regex(@"[:：]\s*(\d{1,3})\s*%\s*$", RegexOptions.Compiled);
        static readonly Regex PureInt = new Regex(@"[:：]\s*(\d{1,3})\s*$", RegexOptions.Compiled);
        static readonly Regex GHz = new Regex(@"(\d(?:\.\d)?)\s*GHz", RegexOptions.IgnoreCase | RegexOptions.Compiled);

        public static string BandName(int band) =>
            band == 6 ? "6G" : band == 5 ? "5G" : band == 2 ? "2.4G" : "";

        static int BandFromGHz(string s)
        {
            if (!double.TryParse(s, NumberStyles.Float, CultureInfo.InvariantCulture, out double d)) return 0;
            return d < 3 ? 2 : d < 5.9 ? 5 : 6;
        }

        static string NormMac(string m) => m.Replace('-', ':').ToLowerInvariant();

        /// <summary>重新计算 SSID 汇总信号与频段文字。</summary>
        public static void Summarize(NearbyNetwork n)
        {
            n.Signal = n.Bss.Count > 0 ? n.Bss.Max(b => b.Signal) : 0;
            var groups = n.Bss.Where(b => b.Band > 0).GroupBy(b => b.Band).OrderByDescending(g => g.Key).ToList();
            n.Bands = string.Join("  ·  ", groups.Select(g => BandName(g.Key) + " " + g.Max(x => x.Signal) + "%"));
            n.BandTags = string.Join("/", groups.Select(g => BandName(g.Key)));
        }

        static Task<string> RunNetshAsync(string args)
        {
            return Task.Run(() =>
            {
                try
                {
                    // 先把代码页切到 UTF-8，这样中文 SSID 不会乱码
                    var psi = new ProcessStartInfo("cmd.exe", "/c chcp 65001 >nul & netsh " + args)
                    {
                        UseShellExecute = false,
                        CreateNoWindow = true,
                        RedirectStandardOutput = true,
                        RedirectStandardError = true,
                        StandardOutputEncoding = Encoding.UTF8,
                        StandardErrorEncoding = Encoding.UTF8
                    };
                    using (var p = Process.Start(psi))
                    {
                        string o = p.StandardOutput.ReadToEnd();
                        p.StandardError.ReadToEnd();
                        p.WaitForExit(15000);
                        return o ?? "";
                    }
                }
                catch { return ""; }
            });
        }

        public static async Task<WifiState> ReadStateAsync()
        {
            var tIf = RunNetshAsync("wlan show interfaces");
            var tNet = RunNetshAsync("wlan show networks mode=bssid");
            var tPro = RunNetshAsync("wlan show profiles");
            await Task.WhenAll(tIf, tNet, tPro);
            return ParseState(tIf.Result, tNet.Result, tPro.Result);
        }

        /// <summary>解析三条 netsh 命令的输出（纯函数，可单独测试）。</summary>
        public static WifiState ParseState(string ifOut, string netOut, string proOut)
        {
            var st = new WifiState();

            // 新版 Windows 11 需要开启“位置服务”才允许读取 WiFi 信息
            if (ifOut.Contains("privacy-location") || netOut.Contains("privacy-location"))
                st.LocationDenied = true;

            // ---- 当前连接（SSID / BSSID / 频段 / 信号） ----
            bool sigRead = false;
            int curChan = 0;
            foreach (var raw in ifOut.Split('\n'))
            {
                var line = raw.TrimEnd('\r');
                if (!st.Connected)
                {
                    var m = SsidOnly.Match(line);
                    if (m.Success && m.Groups[1].Value.Length > 0)
                    {
                        st.Ssid = m.Groups[1].Value;
                        st.Connected = true;
                    }
                    continue;
                }
                if (st.CurBssid == null)
                {
                    var mb = MacLine.Match(line);
                    if (mb.Success) { st.CurBssid = NormMac(mb.Groups[1].Value); continue; }
                }
                if (!sigRead)
                {
                    var p = PctLine.Match(line);
                    if (p.Success)
                    {
                        int.TryParse(p.Groups[1].Value, out st.Signal);
                        sigRead = true;
                        continue;
                    }
                }
                if (st.CurBand == 0)
                {
                    var g = GHz.Match(line);
                    if (g.Success) { st.CurBand = BandFromGHz(g.Groups[1].Value); continue; }
                }
                if (curChan == 0)
                {
                    var c = PureInt.Match(line);
                    if (c.Success) int.TryParse(c.Groups[1].Value, out curChan);   // 第一个纯数字行即信道
                }
            }
            if (st.CurBand == 0 && curChan > 0) st.CurBand = curChan <= 14 ? 2 : 5;

            // ---- 附近网络：按 SSID → BSSID 解析 ----
            var map = new Dictionary<string, NearbyNetwork>();
            NearbyNetwork curNet = null;
            BssInfo curBss = null;
            bool sigSet = false;
            foreach (var raw in netOut.Split('\n'))
            {
                var line = raw.TrimEnd('\r');

                var ms = SsidNum.Match(line);
                if (ms.Success)
                {
                    curNet = null; curBss = null;
                    string name = ms.Groups[1].Value;
                    if (name.Length > 0)
                    {
                        if (!map.TryGetValue(name, out curNet))
                        {
                            curNet = new NearbyNetwork { Ssid = name };
                            map[name] = curNet;
                        }
                    }
                    continue;
                }
                if (curNet == null) continue;

                var mb = BssidNum.Match(line);
                if (mb.Success)
                {
                    curBss = new BssInfo { Bssid = NormMac(mb.Groups[1].Value) };
                    curNet.Bss.Add(curBss);
                    sigSet = false;
                    continue;
                }
                if (curBss == null) continue;

                if (!sigSet)
                {
                    var p = PctLine.Match(line);
                    if (p.Success)
                    {
                        int.TryParse(p.Groups[1].Value, out curBss.Signal);
                        sigSet = true;
                        continue;
                    }
                }
                var g = GHz.Match(line);
                if (g.Success) { if (curBss.Band == 0) curBss.Band = BandFromGHz(g.Groups[1].Value); continue; }
                if (sigSet && curBss.Chan == 0)
                {
                    var c = PureInt.Match(line);
                    if (c.Success) int.TryParse(c.Groups[1].Value, out curBss.Chan);
                }
            }

            foreach (var n in map.Values)
            {
                foreach (var b in n.Bss)
                    if (b.Band == 0 && b.Chan > 0) b.Band = b.Chan <= 14 ? 2 : 5;   // 老系统没有 Band 行时按信道推断
                Summarize(n);
            }
            st.Nearby = map.Values.ToList();

            // 用 BSSID 反查当前频段（比解析 show interfaces 更可靠）
            if (st.Connected && st.CurBssid != null)
            {
                foreach (var n in st.Nearby)
                {
                    var b = n.Bss.FirstOrDefault(x => x.Bssid == st.CurBssid);
                    if (b != null && b.Band > 0) { st.CurBand = b.Band; break; }
                }
            }

            // ---- 已保存的配置文件 ----
            foreach (var raw in proOut.Split('\n'))
            {
                var t = raw.TrimEnd('\r');
                if (t.Length == 0 || !char.IsWhiteSpace(t, 0)) continue;   // 缩进行才是配置项
                int i = t.IndexOfAny(new[] { ':', '：' });
                if (i < 0) continue;
                string v = t.Substring(i + 1).Trim();
                if (v.Length > 0) st.Profiles.Add(v);
            }
            return st;
        }

        /// <summary>
        /// 连接到已保存的配置文件（profile 名通常与 SSID 相同）。
        /// 给了 bssid 就先用 Native WiFi API 指定接入点（用来选 5G / 2.4G 频段），失败再退回 netsh。
        /// </summary>
        public static Task<bool> ConnectAsync(string profile, string ssid, string bssid = null)
        {
            return Task.Run(() =>
            {
                if (!string.IsNullOrEmpty(bssid) && Native.ConnectToBssid(profile, bssid)) return true;
                try
                {
                    var psi = new ProcessStartInfo("netsh.exe")
                    {
                        UseShellExecute = false,
                        CreateNoWindow = true,
                        RedirectStandardOutput = true,
                        RedirectStandardError = true
                    };
                    psi.ArgumentList.Add("wlan");
                    psi.ArgumentList.Add("connect");
                    psi.ArgumentList.Add("name=" + profile);
                    psi.ArgumentList.Add("ssid=" + ssid);
                    using (var p = Process.Start(psi))
                    {
                        p.StandardOutput.ReadToEnd();
                        p.StandardError.ReadToEnd();
                        p.WaitForExit(15000);
                        return p.ExitCode == 0;
                    }
                }
                catch { return false; }
            });
        }

        /// <summary>为 WPA2-个人 网络生成并导入配置文件（密码只在临时文件里短暂存在）。</summary>
        public static Task<bool> AddProfileAsync(string ssid, string password)
        {
            return Task.Run(() =>
            {
                string tmp = Path.Combine(Path.GetTempPath(), "wp_" + Guid.NewGuid().ToString("N") + ".xml");
                try
                {
                    string s = SecurityElement.Escape(ssid);
                    string k = SecurityElement.Escape(password);
                    string xml =
$@"<?xml version=""1.0""?>
<WLANProfile xmlns=""http://www.microsoft.com/networking/WLAN/profile/v1"">
  <name>{s}</name>
  <SSIDConfig><SSID><name>{s}</name></SSID></SSIDConfig>
  <connectionType>ESS</connectionType>
  <connectionMode>auto</connectionMode>
  <MSM><security>
    <authEncryption><authentication>WPA2PSK</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption>
    <sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{k}</keyMaterial></sharedKey>
  </security></MSM>
</WLANProfile>";
                    File.WriteAllText(tmp, xml, new UTF8Encoding(false));

                    var psi = new ProcessStartInfo("netsh.exe")
                    {
                        UseShellExecute = false,
                        CreateNoWindow = true,
                        RedirectStandardOutput = true,
                        RedirectStandardError = true
                    };
                    psi.ArgumentList.Add("wlan");
                    psi.ArgumentList.Add("add");
                    psi.ArgumentList.Add("profile");
                    psi.ArgumentList.Add("filename=" + tmp);
                    psi.ArgumentList.Add("user=all");
                    using (var p = Process.Start(psi))
                    {
                        p.StandardOutput.ReadToEnd();
                        p.StandardError.ReadToEnd();
                        p.WaitForExit(15000);
                        return p.ExitCode == 0;
                    }
                }
                catch { return false; }
                finally { try { File.Delete(tmp); } catch { } }
            });
        }
    }
}
