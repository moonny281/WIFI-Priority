using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;
using WifiPriority.Models;

namespace WifiPriority.Services
{
    public enum Verdict { Pending, Healthy, LinkDead, NoInternet }

    /// <summary>引擎发布给链路监视的“当前连接”。</summary>
    public sealed class CurLink
    {
        public string Ssid;
        public Guid IfGuid;        // WLAN 接口 GUID = 网络接口 Id
        public long ConnectedAt;   // 这一次连接开始的时间（SSID 或 BSSID 变了就重新计）
    }

    /// <summary>
    /// 链路健康检测（v1.1.8），与 Tauri 版 link.rs 一一对应。分三层，目的是在“当前网络其实已经用不了”时尽快回落：
    ///  L0 事件推送：WLAN ACM/MSM 通知（由引擎转发）+ 系统网络变化事件 → 立即唤醒；这里只当唤醒信号，不当判断依据。
    ///  L1 网卡硬件读数：每 250 ms 读网卡驱动的链路状态与收发字节计数器，不发包。
    ///     链路状态非 Up → 立即判死；发了包（out 增长）却一个字节都没收到（in 不增长）→ “怀疑”。
    ///  L2 分级主动探测（只在“怀疑”或空闲心跳到期时发包，且都绑定到 WiFi 网卡）：
    ///     ① 网关 ICMP；② 公网 TCP 443（IP_UNICAST_IF 强制走该网卡，三个地址并行）。连续 2 次确认才下结论。
    /// 不使用 Windows 自带的网络状态（NCSI / NLM）：它按系统自己的节奏探测并带滞后。
    /// </summary>
    public sealed class LinkMonitor
    {
        readonly AppSettings _s;
        readonly SemaphoreSlim _wake = new SemaphoreSlim(0, 1);
        readonly Stopwatch _clock = Stopwatch.StartNew();

        /// <summary>由引擎提供：当前连接（没连上 = null）。</summary>
        public Func<CurLink> GetCurrent;
        /// <summary>引擎观察到当前接入点的帧时间戳很久没刷新（热点可能刚被关）：只用来立刻去 ping 网关，不会单独判死。</summary>
        public Func<bool> GetSilent;
        /// <summary>判死：(ssid, 原因, 用时说明)。里面负责写 Decider 的不可用标记/退避表、置紧急扫描、唤醒引擎。</summary>
        public Action<string, string, string> ReportUnusable;
        /// <summary>从“不可用”恢复为健康。</summary>
        public Action<string> ReportHealthy;
        /// <summary>健康满 30 秒：清零该网络的退避等级。</summary>
        public Action<string> ClearBackoff;

        public LinkMonitor(AppSettings s)
        {
            _s = s;
            try
            {
                NetworkChange.NetworkAddressChanged += (_, __) => Wake();
                NetworkChange.NetworkAvailabilityChanged += (_, __) => Wake();
            }
            catch { }
        }

        public void Wake() { try { _wake.Release(); } catch (SemaphoreFullException) { } }
        long Now => _clock.ElapsedMilliseconds;

        // ---- 每次连接一份状态 ----
        string _ssid;
        long _connAt, _lastTick, _lastRx, _stall, _nextHb, _nextWan, _badSince, _healthySince, _recheckAt;
        long _inPrev = -1, _outPrev = -1;
        int _wanFail;
        bool _gwNoIcmp, _reported;
        IPAddress _gw;
        NetworkInterface _ni;
        long _niAt = -100000;
        string _why;

        void Reset(CurLink c, long now)
        {
            _ssid = c.Ssid; _connAt = c.ConnectedAt;
            _lastTick = now; _lastRx = now;
            _stall = _nextHb = _nextWan = _badSince = _healthySince = _recheckAt = 0;
            _inPrev = _outPrev = -1;
            _wanFail = 0;
            _gwNoIcmp = _reported = false;
            _gw = null; _ni = null; _niAt = -100000;
        }

        public async Task RunAsync(CancellationToken ct)
        {
            while (!ct.IsCancellationRequested)
            {
                try { await _wake.WaitAsync(250, ct); }
                catch (OperationCanceledException) { break; }

                try
                {
                    var cur = GetCurrent?.Invoke();
                    if (cur == null || !_s.InternetCheck) { _ssid = null; continue; }
                    long now = Now;
                    if (_ssid != cur.Ssid || _connAt != cur.ConnectedAt) Reset(cur, now);

                    // 已经判死、而且仍然连着（没有别的网络可去）：降低复查频率，别每 250 ms 反复发探测包
                    if (_reported && now < _recheckAt) continue;

                    var v = await EvaluateAsync(cur.IfGuid, now);
                    if (v == Verdict.Healthy)
                    {
                        if (_healthySince == 0) _healthySince = now;
                        if (_reported) { _reported = false; ReportHealthy?.Invoke(_ssid); }
                        if (now - _healthySince > 30000) ClearBackoff?.Invoke(_ssid);
                    }
                    else if (v == Verdict.LinkDead || v == Verdict.NoInternet)
                    {
                        if (!_reported)
                        {
                            _reported = true; _healthySince = 0;
                            string why = v == Verdict.NoInternet ? "已连接，网关可达但公网不通" : _why;
                            string took = _badSince > 0 ? $"发现异常后 {now - _badSince} ms 判定" : "即时判定";
                            ReportUnusable?.Invoke(_ssid, why, took);
                        }
                        _recheckAt = now + 2000;
                    }
                }
                catch { /* 监视线程不能因为一次异常退出 */ }
            }
        }

        async Task<Verdict> EvaluateAsync(Guid guid, long now)
        {
            // —— L1：网卡硬件状态与驱动计数器 ——
            if (_ni == null || now - _niAt >= 500)
            {
                _niAt = now;
                string id = guid.ToString("B");
                _ni = NetworkInterface.GetAllNetworkInterfaces()
                    .FirstOrDefault(n => string.Equals(n.Id, id, StringComparison.OrdinalIgnoreCase));
            }
            if (_ni == null) return Verdict.Pending;
            if (_ni.OperationalStatus != OperationalStatus.Up) { _why = "网卡/驱动报告链路已断开"; return Verdict.LinkDead; }

            IPInterfaceStatistics stats;
            try { stats = _ni.GetIPStatistics(); }
            catch { return Verdict.Pending; }
            long inB = stats.BytesReceived, outB = stats.BytesSent;
            long dt = Math.Min(Math.Max(now - _lastTick, 1), 1000);
            _lastTick = now;
            if (_inPrev >= 0)
            {
                long dIn = inB - _inPrev, dOut = outB - _outPrev;
                if (dIn > 0) { _stall = 0; _lastRx = now; }
                else if (dOut >= 256)
                {
                    _stall += dt;                            // 发了包，却一个字节都没收到
                    if (_badSince == 0) _badSince = now;
                }
            }
            else _lastRx = now;
            _inPrev = inB; _outPrev = outB;

            // —— 刚连上：等 DHCP/网关就绪（事件唤醒 + 上限），取代旧版固定的 12 秒 ——
            if (_gw == null)
            {
                _gw = _ni.GetIPProperties().GatewayAddresses
                    .Where(g => g.Address.AddressFamily == AddressFamily.InterNetwork && !g.Address.Equals(IPAddress.Any))
                    .Select(g => g.Address).FirstOrDefault();
                if (_gw == null)
                {
                    if (now - _connAt > _s.IpWaitMs) { _why = "DHCP 超时，没有拿到网关"; return Verdict.LinkDead; }
                    return Verdict.Pending;
                }
                _nextWan = now;   // 网关一就绪就立刻验一次公网
            }

            int ifIndex;
            try { ifIndex = _ni.GetIPProperties().GetIPv4Properties().Index; }
            catch { return Verdict.Pending; }

            bool suspect = _stall >= _s.StallMs;
            bool idleHb = now >= _nextHb && now - _lastRx >= _s.HeartbeatMs;   // 有入站流量就不用心跳
            bool silentHb = (GetSilent?.Invoke() ?? false) && now >= _nextHb;  // 接入点静默：每个心跳周期去确认一次网关
            bool gwAlive = false;

            // —— L2-①：网关 ICMP（只在怀疑 / 空闲心跳时）——
            if ((suspect || idleHb || silentHb) && !_gwNoIcmp)
            {
                _nextHb = now + _s.HeartbeatMs;
                gwAlive = await PingAsync(_gw, 400) || await PingAsync(_gw, 400);
                if (gwAlive) { _stall = 0; _badSince = 0; }
                else if (await WanOkAsync(ifIndex, _s.WanTimeoutMs))
                {
                    // 网关不回 ICMP（部分热点 / 访客网络），但公网是通的：记住，别再用 ICMP 判死
                    _gwNoIcmp = true; _stall = 0; _badSince = 0;
                    return Verdict.Healthy;
                }
                else { _why = "入站停滞且网关无响应"; return Verdict.LinkDead; }
            }

            // —— L2-②：公网（到期，或刚被怀疑过）——
            if (now >= _nextWan || (suspect && (gwAlive || _gwNoIcmp)))
            {
                if (await WanOkAsync(ifIndex, _s.WanTimeoutMs))
                {
                    _wanFail = 0; _stall = 0; _badSince = 0;
                    _nextWan = now + _s.WanIntervalSec * 1000L;
                    return Verdict.Healthy;
                }
                _wanFail++;
                if (_badSince == 0) _badSince = now;
                _nextWan = now + 500;   // 失败后 0.5 秒内复测，连续 2 次才下结论
                if (_wanFail >= 2) return Verdict.NoInternet;
            }

            // 两次探测之间：没有任何异常迹象 → 沿用“健康”
            if (_wanFail == 0 && _stall == 0 && _badSince == 0 && now < _nextWan) return Verdict.Healthy;
            return Verdict.Pending;
        }

        // ---------------- 探测 ----------------

        static async Task<bool> PingAsync(IPAddress ip, int ms)
        {
            try
            {
                using (var p = new Ping())
                    return (await p.SendPingAsync(ip, ms)).Status == IPStatus.Success;
            }
            catch { return false; }
        }

        static readonly string[] WanTargets = { "223.5.5.5", "1.1.1.1", "114.114.114.114" };

        /// <summary>并行探测多个公网地址，任一通即可。</summary>
        static async Task<bool> WanOkAsync(int ifIndex, int timeoutMs)
        {
            var tasks = WanTargets.Select(t => TcpOnIfAsync(ifIndex, IPAddress.Parse(t), 443, timeoutMs)).ToList();
            while (tasks.Count > 0)
            {
                var t = await Task.WhenAny(tasks);
                tasks.Remove(t);
                if (t.Result) return true;
            }
            return false;
        }

        const int IpUnicastIf = 31;   // IP_UNICAST_IF

        /// <summary>
        /// 绑定到指定网卡的 TCP 连接探测。IP_UNICAST_IF 强制从该网卡发出（IPv4 要求网络字节序的接口索引），
        /// 避免“WiFi 已死、有线/VPN 还通 → 误判检测通过”。只有 connect 成功才算通。
        /// </summary>
        static async Task<bool> TcpOnIfAsync(int ifIndex, IPAddress ip, int port, int timeoutMs)
        {
            using (var s = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
            using (var cts = new CancellationTokenSource(timeoutMs))
            {
                try
                {
                    s.SetSocketOption(SocketOptionLevel.IP, (SocketOptionName)IpUnicastIf, IPAddress.HostToNetworkOrder(ifIndex));
                    await s.ConnectAsync(ip, port, cts.Token);
                    return true;
                }
                catch { return false; }
            }
        }
    }
}
