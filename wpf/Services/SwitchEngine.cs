using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.NetworkInformation;
using System.Threading;
using System.Threading.Tasks;
using WifiPriority.Models;

namespace WifiPriority.Services
{
    /// <summary>
    /// 核心切换引擎：周期扫描 → 把“优先级列表 × 频段”展开成候选项 → 挑出当前可用的最优候选 → 带防抖地切换。
    ///
    /// 排序规则（BandPolicy）：
    ///   Off    仅按列表顺序，不区分频段；
    ///   Same   先按列表顺序，同一个 WiFi 有多个频段时优先偏好频段（默认偏好 5G，6G &gt; 5G &gt; 2.4G）；
    ///   Global 先看频段：只要有可用的偏好频段网络，就优先于所有其它频段的网络，再按列表顺序。
    ///
    /// 升级：更优候选信号 &gt;= UpThreshold，连续 ConfirmCount 次，且过了冷却时间。
    /// 回落：当前候选信号 &lt; DownThreshold 或消失，连续 ConfirmCount 次后切到次优候选（此时只要求 &gt;= DownThreshold）。
    /// </summary>
    public class SwitchEngine
    {
        class Cand
        {
            public string Ssid;
            public int Band;        // 0 = 不区分/未知
            public string Bssid;
            public int Signal;
            public int Rank;        // 在优先级列表中的位置（0 最高）
            public string Key => Ssid + "|" + Band;
        }

        readonly AppSettings _s;
        CancellationTokenSource _cts;
        readonly SemaphoreSlim _wake = new SemaphoreSlim(0, 1);

        DateTime _lastSwitch = DateTime.MinValue;
        DateTime _pausedUntil = DateTime.MinValue;
        string _pending;
        int _pendingCount;
        string _prev;       // 上一轮连接的 SSID
        string _expected;   // 本程序最近一次主动切换的目标 SSID
        int _pingFail;
        // 临时不可用：key 为 SSID（整网）或 "SSID|频段"（某一频段）
        readonly Dictionary<string, DateTime> _bad = new Dictionary<string, DateTime>();

        public volatile bool ManualPause;
        public bool Paused => ManualPause || DateTime.Now < _pausedUntil;
        public DateTime? PausedUntil => (!ManualPause && DateTime.Now < _pausedUntil) ? _pausedUntil : (DateTime?)null;

        public event Action<string> Log;
        public event Action<EngineStatus> Updated;
        public event Action<string, string, string> Switched;   // from, to, reason

        public SwitchEngine(AppSettings s) { _s = s; }

        public void Start()
        {
            _cts = new CancellationTokenSource();
            var token = _cts.Token;
            Task.Run(() => LoopAsync(token));
        }

        public void Stop() { _cts?.Cancel(); }

        /// <summary>立即触发一轮扫描（唤醒、网络变化、用户点击“立即扫描”等）。</summary>
        public void TriggerNow()
        {
            try { _wake.Release(); } catch (SemaphoreFullException) { }
        }

        public void ResumeNow()
        {
            ManualPause = false;
            _pausedUntil = DateTime.MinValue;
            TriggerNow();
        }

        async Task LoopAsync(CancellationToken ct)
        {
            while (!ct.IsCancellationRequested)
            {
                try { await TickAsync(); }
                catch (Exception ex) { Log?.Invoke("引擎错误：" + ex.Message); }

                try { await _wake.WaitAsync(TimeSpan.FromSeconds(Math.Max(3, _s.ScanIntervalSec)), ct); }
                catch (OperationCanceledException) { break; }
            }
        }

        // ---------- 工具 ----------
        static string Label(string ssid, int band) =>
            band > 0 && ssid != null ? $"{ssid}（{Wifi.BandName(band)}）" : ssid;

        /// <summary>确保“当前连接”出现在附近网络里，并以网卡上报的信号为准。</summary>
        static void EnsureCurrent(WifiState st)
        {
            if (!st.Connected) return;
            var n = st.Nearby.FirstOrDefault(x => x.Ssid == st.Ssid);
            if (n == null) { n = new NearbyNetwork { Ssid = st.Ssid }; st.Nearby.Add(n); }

            BssInfo b = null;
            if (st.CurBssid != null)
                b = n.Bss.FirstOrDefault(x => string.Equals(x.Bssid, st.CurBssid, StringComparison.OrdinalIgnoreCase));
            if (b == null)
            {
                b = new BssInfo { Bssid = st.CurBssid, Band = st.CurBand };
                n.Bss.Add(b);
            }
            b.Signal = st.Signal;
            if (b.Band == 0) b.Band = st.CurBand;
            Wifi.Summarize(n);
        }

        void ResetPending() { _pending = null; _pendingCount = 0; }

        static List<Cand> BuildCands(WifiState st, List<string> pri, string policy)
        {
            var list = new List<Cand>();
            for (int i = 0; i < pri.Count; i++)
            {
                var n = st.Nearby.FirstOrDefault(x => x.Ssid == pri[i]);
                if (n == null) continue;

                if (policy == "Off" || n.Bss.Count == 0)
                {
                    list.Add(new Cand
                    {
                        Ssid = n.Ssid, Band = 0, Bssid = null, Rank = i,
                        Signal = st.Connected && n.Ssid == st.Ssid ? st.Signal : n.Signal
                    });
                    continue;
                }
                // 每个频段取信号最强的那个接入点
                foreach (var g in n.Bss.GroupBy(b => b.Band))
                {
                    var b = g.OrderByDescending(x => x.Signal).First();
                    list.Add(new Cand { Ssid = n.Ssid, Band = b.Band, Bssid = b.Bssid, Signal = b.Signal, Rank = i });
                }
            }
            return list;
        }

        static IEnumerable<Cand> Order(List<Cand> c, string policy, bool prefer24)
        {
            int BR(int b) => prefer24
                ? (b == 2 ? 0 : b == 5 ? 1 : b == 6 ? 2 : 3)
                : (b == 6 ? 0 : b == 5 ? 1 : b == 2 ? 2 : 3);

            if (policy == "Global") return c.OrderBy(x => BR(x.Band)).ThenBy(x => x.Rank);
            if (policy == "Same") return c.OrderBy(x => x.Rank).ThenBy(x => BR(x.Band));
            return c.OrderBy(x => x.Rank);
        }

        // ---------- 一轮扫描与决策 ----------
        async Task TickAsync()
        {
            await Native.TriggerScanAsync();
            var st = await Wifi.ReadStateAsync();
            EnsureCurrent(st);
            var now = DateTime.Now;
            string cur = st.Connected ? st.Ssid : null;

            // 检测用户在 Windows 里手动换了网络 → 暂停一段时间，避免和用户对着干
            if (st.Connected)
            {
                if (_prev != null && st.Ssid != _prev && st.Ssid != _expected && _s.ManualPauseMinutes > 0)
                {
                    _pausedUntil = now.AddMinutes(_s.ManualPauseMinutes);
                    ResetPending();
                    Log?.Invoke($"检测到网络被手动切换到 {st.Ssid}，自动切换暂停 {_s.ManualPauseMinutes} 分钟");
                }
                _prev = st.Ssid;
            }
            else _prev = null;

            if (st.LocationDenied || Paused) { Publish(st); return; }

            var pri = _s.Priorities.ToList();
            string policy = _s.BandPolicy == "Off" || _s.BandPolicy == "Same" ? _s.BandPolicy : "Global";
            bool prefer24 = _s.PreferBand == "2.4G";

            // 可选：联网检测
            if (cur != null && _s.InternetCheck)
            {
                if (await HasInternetAsync()) _pingFail = 0;
                else if (++_pingFail >= 3)
                {
                    _pingFail = 0;
                    _bad[cur] = now.AddMinutes(5);
                    Log?.Invoke($"{cur} 已连接但无法上网，5 分钟内视为不可用");
                }
            }

            var cands = BuildCands(st, pri, policy);

            bool IsCurrent(Cand c) =>
                cur != null && c.Ssid == cur && (policy == "Off" || st.CurBand == 0 || c.Band == st.CurBand);

            bool IsBad(Cand c) =>
                (_bad.TryGetValue(c.Ssid, out var u1) && u1 > now) ||
                (_bad.TryGetValue(c.Key, out var u2) && u2 > now);

            var curCand = cands.Where(IsCurrent).OrderByDescending(c => c.Signal).FirstOrDefault();
            bool curListed = cur != null && pri.Contains(cur);
            bool curOk = curCand != null && !IsBad(curCand) && curCand.Signal >= _s.DownThreshold;

            // 未连接，或当前（列表内）网络已经不行了：只要求候选 >= 回落阈值，避免卡在烂信号上
            bool relaxed = cur == null || (curListed && !curOk);

            bool Usable(Cand c)
            {
                if (IsBad(c)) return false;
                bool isCur = IsCurrent(c);
                if (!isCur && !st.Profiles.Contains(c.Ssid)) return false;   // 没有已保存配置，无法连接
                int th = (isCur || relaxed) ? _s.DownThreshold : _s.UpThreshold;
                return c.Signal >= th;
            }

            var best = Order(cands, policy, prefer24).FirstOrDefault(Usable);
            if (best == null || IsCurrent(best)) { ResetPending(); Publish(st); return; }

            if (cur != null && !curListed && !_s.TakeoverUnlisted) { ResetPending(); Publish(st); return; }

            bool sameSsid = cur != null && best.Ssid == cur;
            string reason = cur == null ? "当前未连接"
                : !curListed ? "当前网络不在优先级列表中"
                : curOk ? (sameSsid ? $"同一网络有更优频段（{Wifi.BandName(best.Band)}）" : "出现更高优先级网络")
                : (sameSsid ? "当前频段信号丢失或过弱，回落到另一频段" : "当前网络信号丢失或过弱，回落到次优先级");

            if (cur == null)
            {
                // 刚发出连接请求，等它连上，不要重复发
                if ((now - _lastSwitch).TotalSeconds < 15) { Publish(st); return; }
            }
            else
            {
                if (_pending == best.Key) _pendingCount++; else { _pending = best.Key; _pendingCount = 1; }
                bool waitConfirm = _pendingCount < Math.Max(1, _s.ConfirmCount);
                bool cooling = curOk && (now - _lastSwitch).TotalSeconds < _s.CooldownSec;
                if (waitConfirm || cooling) { Publish(st); return; }
            }

            string fromLabel = cur == null ? null : Label(cur, st.CurBand);
            string toLabel = Label(best.Ssid, best.Band);
            Log?.Invoke($"切换：{fromLabel ?? "(未连接)"} → {toLabel}（{reason}）");

            // 有明确频段时，用 BSSID 指定接入点，才能真正落在想要的频段上
            string bssid = policy != "Off" && best.Band > 0 ? best.Bssid : null;
            bool ok = await Wifi.ConnectAsync(best.Ssid, best.Ssid, bssid);
            _lastSwitch = DateTime.Now;
            ResetPending();

            if (ok)
            {
                _expected = best.Ssid;
                Switched?.Invoke(fromLabel, toLabel, reason);
            }
            else
            {
                _bad[best.Ssid] = DateTime.Now.AddMinutes(2);
                Log?.Invoke($"连接 {best.Ssid} 失败（配置文件名需与 WiFi 名称一致，2 分钟后重试）");
            }

            // 等连接稳定（最多约 10 秒），顺便验证频段是否真的切过去了
            WifiState st2 = null;
            for (int i = 0; i < 4; i++)
            {
                await Task.Delay(2500);
                st2 = await Wifi.ReadStateAsync();
                EnsureCurrent(st2);
                if (st2.Connected && st2.Ssid == best.Ssid && (best.Band == 0 || st2.CurBand == best.Band)) break;
            }
            if (ok && bssid != null && st2 != null && st2.Connected && st2.Ssid == best.Ssid
                && st2.CurBand > 0 && st2.CurBand != best.Band)
            {
                _bad[best.Key] = DateTime.Now.AddMinutes(10);
                Log?.Invoke($"未能切换到 {toLabel}（路由器或网卡驱动没有接受），10 分钟内不再尝试该频段");
            }
            _prev = st2 != null && st2.Connected ? st2.Ssid : null;
            Publish(st2 ?? st);
        }

        static async Task<bool> HasInternetAsync()
        {
            foreach (var host in new[] { "223.5.5.5", "1.1.1.1" })
            {
                try
                {
                    using (var p = new Ping())
                    {
                        var r = await p.SendPingAsync(host, 2000);
                        if (r.Status == IPStatus.Success) return true;
                    }
                }
                catch { }
            }
            return false;
        }

        void Publish(WifiState st)
        {
            Updated?.Invoke(new EngineStatus
            {
                Connected = st.Connected,
                Ssid = st.Ssid,
                Signal = st.Signal,
                CurBand = st.CurBand,
                CurBssid = st.CurBssid,
                LocationDenied = st.LocationDenied,
                Paused = Paused,
                PausedUntil = PausedUntil,
                Nearby = st.Nearby.OrderByDescending(x => x.Signal).ToList(),
                Profiles = st.Profiles,
                Message = st.LocationDenied ? "位置服务未开启"
                    : (st.Nearby.Count == 0 && !st.Connected ? "未扫描到任何网络" : null)
            });
        }
    }
}
