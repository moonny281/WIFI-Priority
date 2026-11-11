using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net.NetworkInformation;
using System.Threading;
using System.Threading.Tasks;
using WifiPriority.Models;

namespace WifiPriority.Services
{
    /// <summary>
    /// 切换引擎（事件驱动）：
    ///  - 原生 WLAN 接口：每 250 ms 用进程内调用读一次缓存数据（&lt; 5 ms），WLAN 通知（断线/连上/扫描完成）会立刻唤醒；
    ///  - 后台按 ScanIntervalSec 主动请求扫描（0 = 连续扫描），扫描结果一出来就评估；
    ///  - 决策逻辑在 Decider（纯函数），这里只负责 IO；
    ///  - 原生读取失败时自动回退到旧的 netsh 模式（慢，但兼容）。
    /// </summary>
    public class SwitchEngine
    {
        readonly AppSettings _s;
        readonly WlanClient _wlan = new WlanClient();
        readonly Decider _dec = new Decider();
        readonly Stopwatch _clock = Stopwatch.StartNew();
        readonly SemaphoreSlim _wake = new SemaphoreSlim(0, 1);
        CancellationTokenSource _cts;

        bool _native;
        DateTime _pausedUntil = DateTime.MinValue;
        long _lastScanAt = -100000;
        volatile bool _scanBusy;
        string _lastSig = "";
        long _lastPublishAt;
        volatile string _curSsid;
        long _connectingSeenAt;   // 最近一次看到“正在连接”的时间

        public volatile bool ManualPause;
        public bool Paused => ManualPause || DateTime.Now < _pausedUntil;
        public DateTime? PausedUntil => (!ManualPause && DateTime.Now < _pausedUntil) ? _pausedUntil : (DateTime?)null;
        public bool NativeMode => _native;

        public event Action<string> Log;
        public event Action<EngineStatus> Updated;
        public event Action<string, string, string> Switched;   // from, to, reason

        long Now => _clock.ElapsedMilliseconds;

        public SwitchEngine(AppSettings s) { _s = s; }

        public void Start()
        {
            _native = _wlan.Open();
            if (_native)
            {
                _wlan.Notified += OnWlanEvent;
                Log?.Invoke("已启用原生 WLAN 接口：事件驱动，毫秒级响应");
            }
            else Log?.Invoke("无法打开原生 WLAN 接口，使用 netsh 兼容模式（切换较慢）");

            _cts = new CancellationTokenSource();
            var token = _cts.Token;
            Task.Run(() => LoopAsync(token));
            Task.Run(() => InternetLoopAsync(token));
        }

        public void Stop()
        {
            try { _cts?.Cancel(); } catch { }
            try { _wlan.Dispose(); } catch { }
        }

        void OnWlanEvent(uint code)
        {
            if (code == 7 || code == 8) _scanBusy = false;   // 扫描完成 / 失败
            TriggerNow();
        }

        /// <summary>立即触发一轮评估（唤醒、网络变化、用户点击“立即扫描”等）。</summary>
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
                int waitMs;
                try { waitMs = await TickAsync(); }
                catch (Exception ex) { Log?.Invoke("引擎错误：" + ex.Message); waitMs = 2000; }

                try { await _wake.WaitAsync(waitMs, ct); }
                catch (OperationCanceledException) { break; }
            }
        }

        void StartScanIfDue(long now)
        {
            long gap = _s.ScanIntervalSec <= 0 ? 400 : _s.ScanIntervalSec * 1000L;
            if (now - _lastScanAt < gap) return;
            // 正在连接时不扫描：用户在 Windows 里手动连、或我们自己刚发出的连接请求。
            // 有些网卡驱动在关联过程中被扫描打断会直接报“无法连接网络”（手机热点响应慢，更容易中招）。
            if (_connectingSeenAt != 0 && now - _connectingSeenAt < 4000) return;
            if (_dec.ActiveTarget != null) return;
            if (_scanBusy && now - _lastScanAt < 6000) return;      // 上一次扫描还没完成
            _lastScanAt = now;
            bool ok = _wlan.Scan();

            // 对排在当前网络前面的优先网络做定向扫描（最多 5 个），更快发现新出现的高优先级网络
            var pri = _s.Priorities.ToList();
            string cur = _curSsid;
            int limit = cur == null ? pri.Count : pri.IndexOf(cur);
            if (limit < 0) limit = pri.Count;
            for (int i = 0; i < limit && i < 5; i++) _wlan.ScanDirected(pri[i]);

            _scanBusy = ok;   // 请求没被接受就不会有“扫描完成”通知，别挂着空等
        }

        /// <summary>确保“当前连接”出现在附近网络里，并以网卡上报的信号为准。</summary>
        static void EnsureCurrent(WifiState st)
        {
            if (!st.Connected) return;
            var n = st.Nearby.FirstOrDefault(x => x.Ssid == st.Ssid);
            if (n == null) { n = new NearbyNetwork { Ssid = st.Ssid }; st.Nearby.Add(n); }
            if (n.Profile == null && !string.IsNullOrEmpty(st.CurProfile)) n.Profile = st.CurProfile;

            BssInfo b = null;
            if (st.CurBssid != null)
                b = n.Bss.FirstOrDefault(x => string.Equals(x.Bssid, st.CurBssid, StringComparison.OrdinalIgnoreCase));
            if (b == null)
            {
                b = new BssInfo { Bssid = st.CurBssid, Band = st.CurBand, Iface = st.CurIface };
                n.Bss.Add(b);
            }
            b.Signal = st.Signal;
            if (b.Band == 0) b.Band = st.CurBand;
            Wifi.Summarize(n);
        }

        /// <summary>一轮：读状态 → 决策 → 执行。返回下一轮前的等待毫秒数。</summary>
        async Task<int> TickAsync()
        {
            var t0 = Stopwatch.StartNew();
            WifiState st = null;

            if (_native)
            {
                StartScanIfDue(Now);
                st = _wlan.ReadState();
                if (st == null)
                {
                    _native = false;
                    Log?.Invoke("原生 WLAN 读取校验失败，已回退到 netsh 兼容模式（切换较慢）");
                }
            }
            if (st == null)
            {
                await Native.TriggerScanAsync();
                st = await Wifi.ReadStateAsync();
            }

            EnsureCurrent(st);
            _curSsid = st.Connected ? st.Ssid : null;
            if (st.Connecting) _connectingSeenAt = Now;

            var dec = _dec.Decide(st, _s, Now, Paused);
            foreach (var l in dec.Logs) Log?.Invoke(l);
            if (dec.PauseMinutes > 0) _pausedUntil = DateTime.Now.AddMinutes(dec.PauseMinutes);

            if (dec.Connect != null) await ExecuteAsync(dec.Connect, t0);

            Publish(st, dec.Connect != null);
            return _native ? 250 : Math.Max(3000, _s.ScanIntervalSec * 1000);
        }

        async Task ExecuteAsync(Plan plan, Stopwatch t0)
        {
            Log?.Invoke($"切换：{plan.FromLabel ?? "(未连接)"} → {plan.ToLabel}（{plan.Reason}）");

            bool ok = false;
            if (_native) ok = _wlan.Connect(plan.Profile, plan.Bssid, plan.Iface);
            if (!ok) ok = await Wifi.ConnectAsync(plan.Profile ?? plan.Ssid, plan.Ssid, plan.Bssid);   // 兜底

            Log?.Invoke(ok
                ? $"连接请求已发出，从读取状态到发出请求用时 {t0.ElapsedMilliseconds} ms（之后是路由器握手时间）"
                : $"系统没有接受连接 {plan.ToLabel} 的请求，2 分钟后重试");

            _dec.ConnectResult(plan, ok, Now);
            if (ok) Switched?.Invoke(plan.FromLabel, plan.ToLabel, plan.Reason);
        }

        // ---------- 联网检测（独立后台任务，不阻塞切换） ----------
        async Task InternetLoopAsync(CancellationToken ct)
        {
            int fail = 0;
            while (!ct.IsCancellationRequested)
            {
                try { await Task.Delay(4000, ct); } catch (OperationCanceledException) { break; }
                if (!_s.InternetCheck) { fail = 0; continue; }
                string cur = _curSsid;
                if (cur == null) { fail = 0; continue; }

                if (await HasInternetAsync()) fail = 0;
                else if (++fail >= 2)
                {
                    fail = 0;
                    _dec.MarkBad(cur, Now + 300000);
                    Log?.Invoke($"{cur} 已连接但无法上网，5 分钟内视为不可用");
                    TriggerNow();
                }
            }
        }

        static async Task<bool> HasInternetAsync()
        {
            foreach (var host in new[] { "223.5.5.5", "1.1.1.1" })
            {
                try
                {
                    using (var p = new Ping())
                    {
                        var r = await p.SendPingAsync(host, 1500);
                        if (r.Status == IPStatus.Success) return true;
                    }
                }
                catch { }
            }
            return false;
        }

        // ---------- 发布给界面（限频：状态没变化时每 2 秒一次） ----------
        void Publish(WifiState st, bool force)
        {
            string target = _dec.ActiveTarget;
            bool connecting = st.Connecting || target != null;
            string sig = $"{st.Connected}|{st.Ssid}|{st.CurBand}|{st.Signal}|{Paused}|{st.LocationDenied}|{connecting}|{target}|"
                + string.Join(",", st.Nearby.Select(n => n.Ssid + ":" + n.Signal));
            long now = Now;
            if (!force && sig == _lastSig && now - _lastPublishAt < 2000) return;
            _lastSig = sig;
            _lastPublishAt = now;

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
                Connecting = connecting,
                Target = target,
                Nearby = st.Nearby.OrderByDescending(x => x.Signal).ToList(),
                Profiles = st.Profiles,
                Message = st.LocationDenied ? "位置服务未开启"
                    : (st.Nearby.Count == 0 && !st.Connected ? "未扫描到任何网络" : null)
            });
        }
    }
}
