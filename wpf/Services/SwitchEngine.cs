using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net.NetworkInformation;
using System.Net.Sockets;
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
        LinkMonitor _link;        // v1.1.8 链路健康检测（原生模式）
        volatile CurLink _curLink;
        string _connKey = "";     // ssid|bssid：变了就算“新的一次连接”
        long _connAt;
        long _switchUntil;           // 程序刚发出连接请求：这个时间（毫秒）之前旧连接掉线是我们自己造成的，链路监视不评估、不判死
        long _scanAsapAt;            // 断线类通知到来后，“最早可以立即扫描”的时间（0 = 无）
        /// <summary>实时流量与用量统计（硬件计数器）。</summary>
        public TrafficService Traffic { get; } = new TrafficService();
        /// <summary>界面是否有人在看流量（悬浮框显示流量 / 主窗口可见）：有人看才 1 秒采样并推送。</summary>
        public Func<bool> IsWatching;
        volatile bool _urgentScan;   // 链路监视判死当前网络后置位：下一轮无视扫描间隔立即扫描

        public volatile bool ManualPause;
        public bool Paused => ManualPause || DateTime.Now < _pausedUntil;
        public DateTime? PausedUntil => (!ManualPause && DateTime.Now < _pausedUntil) ? _pausedUntil : (DateTime?)null;
        public bool NativeMode => _native;

        public event Action<string> Log;
        public event Action<EngineStatus> Updated;
        public event Action<string, string, string> Switched;   // from, to, reason

        long Now => _clock.ElapsedMilliseconds;

        readonly BssFreshness _fresh;   // 识别缓存里已经消失的接入点

        public SwitchEngine(AppSettings s) { _s = s; _fresh = new BssFreshness(Now); }

        public void Start()
        {
            _native = _wlan.Open();
            if (_native)
            {
                _wlan.Notified += OnWlanEvent;
                Log?.Invoke("已启用原生 WLAN 接口：事件驱动，毫秒级响应");
            }
            else Log?.Invoke("无法打开原生 WLAN 接口，使用 netsh 兼容模式（切换较慢）");

            if (SettingsStore.InternetCheckMigrated)
                Log?.Invoke("v1.1.8：联网检测已默认开启（硬件链路读数 + 网关 + 公网分层检测，不会再把刚连上的热点误判成“无法上网”）。不想要可在 设置 里关闭");

            _link = new LinkMonitor(_s)
            {
                GetCurrent = () => _curLink,
                GetSilent = () => _fresh.CurSilent,
                IsSwitching = () => Now < Interlocked.Read(ref _switchUntil),
                ReportUnusable = (ssid, why, took) =>
                {
                    long wait = _dec.ReportUnusable(ssid, Now, _s.UnusableBackoffSec);
                    int lvl = _dec.UnusableLevel(ssid);
                    _urgentScan = true;   // 立刻扫描，别等 ScanIntervalSec
                    Log?.Invoke($"{ssid} 不可用：{why}（{took}），回落到下一个网络；"
                        + (wait > 0 ? $"{wait / 1000} 秒内不会再把它当作切换目标（第 {lvl + 1} 次）" : "未启用退避，它重新成为最优时会再次尝试"));
                    TriggerNow();
                },
                ReportHealthy = ssid => _dec.ClearNoInternet(ssid),
                ClearBackoff = ssid => _dec.ClearUnusable(ssid),
            };

            Traffic.GetCurrent = () => _curLink;
            Traffic.IsWatching = () => IsWatching?.Invoke() ?? false;

            _cts = new CancellationTokenSource();
            var token = _cts.Token;
            Task.Run(() => Traffic.RunAsync(token));
            Task.Run(() => LoopAsync(token));
            Task.Run(() => _link.RunAsync(token));
            Task.Run(() => InternetLoopAsync(token));   // 旧的联网检测只在 netsh 兼容模式（拿不到网卡 GUID）下工作
        }

        public void Stop()
        {
            try { _cts?.Cancel(); } catch { }
            try { Traffic.Save(); } catch { }
            try { _wlan.Dispose(); } catch { }
        }

        void OnWlanEvent(uint code)
        {
            if (code == 7 || code == 8) _scanBusy = false;   // 扫描完成 / 失败
            if (code == 7) { _fresh.ScanDone(Now); _dec.NoteScanDone(Now); }   // 断线后用它判断原网络是不是真的没了（热点被关）
            // 断线类通知（ACM disconnecting/disconnected、MSM disassociating/disconnected）：300 ms 后立刻扫描一次，
            // 马上知道刚才的网络还在不在、下一个网络是否可用。延后 300 ms 是为了不打断用户手动换网的关联过程。
            if (code == 20 || code == 21 || code == (WlanClient.MsmFlag | 9) || code == (WlanClient.MsmFlag | 10)) _scanAsapAt = Now + 300;
            if (code == WlanClient.AcmConnectionAttemptFail) _dec.NoteAttemptFailed(Now);   // 驱动已经明确告知“连接请求失败”：不再傻等 SettleMs
            _link?.Wake();                                                                    // ACM / MSM 事件都叫醒链路监视
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
            long normalGap = _s.ScanIntervalSec <= 0 ? 400 : _s.ScanIntervalSec * 1000L;
            // 猎取模式（有更高优先级的已保存网络现在看不到，或者当前不可用 / 未连接）：用更短的间隔扫描，热点一打开就能看到并连上。
            // 正在大流量（视频通话 / 下载）时不加速，免得扫描打断它。
            bool busy = Traffic.BusyBps > 1500000;
            long gap = _s.HuntScanMs > 0 && _dec.Hunting && !busy ? Math.Min(normalGap, _s.HuntScanMs) : normalGap;
            bool asap = _scanAsapAt != 0 && now >= _scanAsapAt;
            // 链路监视判死当前网络后置位：此时当前连接已经是死的，立刻扫描，不受扫描间隔与“正在连接”保护限制
            bool urgent = _urgentScan;
            _urgentScan = false;
            if (!urgent && !asap && now - _lastScanAt < gap) return;
            // 正在连接时不扫描：用户在 Windows 里手动连、或我们自己刚发出的连接请求。
            // 有些网卡驱动在关联过程中被扫描打断会直接报“无法连接网络”（手机热点响应慢，更容易中招）。
            if (!urgent && _connectingSeenAt != 0 && now - _connectingSeenAt < 2000) return;
            if (_dec.ActiveTarget != null) return;
            if (_scanBusy && now - _lastScanAt < 6000) return;      // 上一次扫描还没完成
            _lastScanAt = now;
            if (asap) _scanAsapAt = 0;
            bool ok = _wlan.Scan();
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

            long msScan = 0, msRead = 0;
            if (_native)
            {
                var tS = Stopwatch.StartNew();
                StartScanIfDue(Now);
                msScan = tS.ElapsedMilliseconds;
                var tR = Stopwatch.StartNew();
                st = _wlan.ReadState();
                msRead = tR.ElapsedMilliseconds;
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

            _fresh.Apply(st, Now);
            EnsureCurrent(st);
            _curSsid = st.Connected ? st.Ssid : null;
            if (st.Connecting) _connectingSeenAt = Now;

            // 发布“当前连接”给链路监视（SSID 或 BSSID 变了就算新的一次连接，重新计时等 DHCP）
            string key = (st.Ssid ?? "") + "|" + (st.CurBssid ?? "");
            if (st.Connected && key != _connKey) { _connKey = key; _connAt = Now; }
            Guid? ifg = st.Connected && st.Fast ? _wlan.IfaceGuid(st.CurIface) : null;
            _curLink = ifg.HasValue ? new CurLink { Ssid = st.Ssid, IfGuid = ifg.Value, ConnectedAt = _connAt } : null;

            var tD = Stopwatch.StartNew();
            var dec = _dec.Decide(st, _s, Now, Paused);
            foreach (var l in dec.Logs) Log?.Invoke(l);
            if (dec.PauseMinutes > 0) _pausedUntil = DateTime.Now.AddMinutes(dec.PauseMinutes);

            if (dec.Connect != null) await ExecuteAsync(dec.Connect, t0);

            // 本轮耗时异常：说明 WLAN 服务 / 驱动在阻塞（常见于热点刚关、驱动在处理断线的时候）。这段时间引擎无法决策，
            // 即使链路监视已经判死也要等这一轮结束才能回落——把每一段的耗时写出来，才能知道是哪个调用卡住了。
            if (t0.ElapsedMilliseconds > 1500)
                Log?.Invoke($"本轮处理耗时 {t0.ElapsedMilliseconds} ms（启动扫描 {msScan} / 读取状态 {msRead} / 决策与执行 {tD.ElapsedMilliseconds}）——WLAN 服务或驱动响应慢，这段时间无法决策");

            Publish(st, dec.Connect != null);
            // 稳定状态（连着、没在切换、没有在等更高优先级的网络出现）降到 500 ms：事件（通知）依然立刻唤醒
            bool calm = st.Connected && !st.Connecting && !_dec.Hunting && _dec.ActiveTarget == null;
            return _native ? (calm ? 500 : 250) : Math.Max(3000, _s.ScanIntervalSec * 1000);
        }

        async Task ExecuteAsync(Plan plan, Stopwatch t0)
        {
            // 从现在起 3 秒内旧连接掉线是我们自己造成的：链路监视不评估、不判死（否则会把刚离开的好网络写进退避）
            Interlocked.Exchange(ref _switchUntil, Now + 3000);
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
        // 只用来决定“刚连上的网络能不能上网，不能就回退”。不做任何冷却：离开那个网络后不留记忆，
        // 它还在、还是最优，就会再次连接。
        async Task InternetLoopAsync(CancellationToken ct)
        {
            int fail = 0;
            string last = null;
            long since = 0;
            while (!ct.IsCancellationRequested)
            {
                try { await Task.Delay(3000, ct); } catch (OperationCanceledException) { break; }
                string cur = _curSsid;
                // 原生模式下由 LinkMonitor 负责（硬件读数 + 绑定网卡的探测），这里只在 netsh 兼容模式下工作
                if (_native || !_s.InternetCheck || cur == null) { fail = 0; last = null; continue; }

                // 刚连上（DHCP 还没完成、路由还没通）的 12 秒内不检测，避免把正在握手的热点误判成“无法上网”
                if (cur != last) { last = cur; since = Now; fail = 0; continue; }
                if (Now - since < 12000) continue;
                if (_dec.ActiveTarget != null) continue;

                if (await HasInternetAsync())
                {
                    fail = 0;
                    _dec.ClearNoInternet(cur);
                }
                else if (++fail >= 3)       // 连续 3 次（约 9 秒）都不通才回退
                {
                    fail = 0;
                    if (!_dec.IsNoInternet(cur))
                    {
                        _dec.ReportNoInternet(cur);
                        Log?.Invoke($"{cur} 已连接但无法上网，回退到下一个可用网络");
                        TriggerNow();
                    }
                }
            }
        }

        /// <summary>ping 和 TCP 443 同时探测两个公共地址，任何一个通就算能上网（有的热点/运营商会拦 ICMP）。</summary>
        static async Task<bool> HasInternetAsync()
        {
            var tasks = new List<Task<bool>>
            {
                PingOkAsync("223.5.5.5"), PingOkAsync("1.1.1.1"),
                TcpOkAsync("223.5.5.5", 443), TcpOkAsync("1.1.1.1", 443)
            };
            while (tasks.Count > 0)
            {
                var t = await Task.WhenAny(tasks);
                tasks.Remove(t);
                if (t.Result) return true;
            }
            return false;
        }

        static async Task<bool> PingOkAsync(string host)
        {
            try
            {
                using (var p = new Ping())
                {
                    var r = await p.SendPingAsync(host, 2000);
                    return r.Status == IPStatus.Success;
                }
            }
            catch { return false; }
        }

        static async Task<bool> TcpOkAsync(string host, int port)
        {
            try
            {
                using (var c = new TcpClient())
                {
                    var t = c.ConnectAsync(host, port);
                    return await Task.WhenAny(t, Task.Delay(2500)) == t && t.Status == TaskStatus.RanToCompletion;
                }
            }
            catch { return false; }
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
