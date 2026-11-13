using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net.NetworkInformation;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace WifiPriority.Services
{
    /// <summary>发给界面的实时快照（与 Tauri 版 TrafficSnap 一致）。</summary>
    public class TrafficSnap
    {
        public bool Connected { get; set; }
        public string Ssid { get; set; } = "";
        public long RxBps { get; set; }       // 实时下行（字节/秒）
        public long TxBps { get; set; }
        public long SessRx { get; set; }      // 本次连接
        public long SessTx { get; set; }
        public long SessSecs { get; set; }
        public long TodayRx { get; set; }     // 该网络今日
        public long TodayTx { get; set; }
        public long TotalRx { get; set; }     // 该网络累计
        public long TotalTx { get; set; }
    }

    public class NetStat
    {
        public long TotalRx { get; set; }
        public long TotalTx { get; set; }
        public long TodayRx { get; set; }
        public long TodayTx { get; set; }
        public string Day { get; set; } = "";   // yyyy-MM-dd：Today* 对应的日期
        public long LastSeen { get; set; }      // unix 秒
        public int Sessions { get; set; }
    }

    public class SessionRec
    {
        public string Ssid { get; set; } = "";
        public long Start { get; set; }         // unix 秒
        public long Secs { get; set; }
        public long Rx { get; set; }
        public long Tx { get; set; }
        public bool Active { get; set; }        // 仅用于界面展示，不保存
    }

    public class TrafficStore
    {
        public Dictionary<string, NetStat> Nets { get; set; } = new Dictionary<string, NetStat>();
        public List<SessionRec> Recent { get; set; } = new List<SessionRec>();
    }

    /// <summary>
    /// 实时流量与用量统计（v1.1.9），与 Tauri 版 traffic.rs 一一对应。
    /// 数据来自 WiFi 网卡驱动的收发字节计数器（硬件累计值）——采样间隔拉长不会丢数据，只是速度粒度变粗，
    /// 所以没人看的时候降到很低的频率。按“网络(SSID)”和“连接(一次连接)”区分：
    ///  - 网络：今日 / 累计，持久化到 %AppData%\WifiPriority\traffic.json；
    ///  - 连接：本次连接用量，最近 30 次连接的记录也会保存。
    /// </summary>
    public class TrafficService
    {
        const int MaxNets = 200, MaxRecent = 30;

        class Cur
        {
            public string Ssid; public long ConnAt; public Guid Guid;
            public long LastIn, LastOut, LastAt, SessRx, SessTx, StartUnix, StartMs;
            public double RxBps, TxBps;
        }

        readonly object _lock = new object();
        TrafficStore _store;
        Cur _cur;
        bool _dirty, _saveSoon;

        public event Action<TrafficSnap> Updated;
        public TrafficSnap Snap { get; private set; } = new TrafficSnap();
        public long BusyBps;   // 当前总流量（字节/秒）：大流量时引擎不加速扫描，免得打断视频通话/下载

        public Func<CurLink> GetCurrent;
        public Func<bool> IsWatching;   // 悬浮框开着并显示流量，或主窗口开着

        static string FilePath => Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "WifiPriority", "traffic.json");

        public TrafficService()
        {
            try { _store = JsonSerializer.Deserialize<TrafficStore>(File.ReadAllText(FilePath)) ?? new TrafficStore(); }
            catch { _store = new TrafficStore(); }
        }

        // ---------- 读网卡硬件计数器 ----------
        NetworkInterface _ni;
        Guid _niGuid;
        long _niAt = -100000;

        (long In, long Out)? ReadCounters(Guid guid, long now)
        {
            try
            {
                if (_ni == null || _niGuid != guid || now - _niAt > 1500)
                {
                    string id = guid.ToString("B");
                    _ni = NetworkInterface.GetAllNetworkInterfaces()
                        .FirstOrDefault(n => string.Equals(n.Id, id, StringComparison.OrdinalIgnoreCase));
                    _niGuid = guid; _niAt = now;
                }
                if (_ni == null) return null;
                var st = _ni.GetIPStatistics();
                return (st.BytesReceived, st.BytesSent);
            }
            catch { return null; }
        }

        // ---------- 计费（纯逻辑，读数函数由调用方给） ----------
        static long Delta(long prev, long now) => now >= prev ? now - prev : now;   // 计数器回零：从 0 重新计

        void Add(string ssid, long rx, long tx, string today, long unix)
        {
            if (rx == 0 && tx == 0) return;
            if (!_store.Nets.ContainsKey(ssid) && _store.Nets.Count >= MaxNets)
            {
                var old = _store.Nets.OrderBy(kv => kv.Value.LastSeen).First().Key;   // 记录太多：丢掉最久没见过的
                _store.Nets.Remove(old);
            }
            if (!_store.Nets.TryGetValue(ssid, out var n)) { n = new NetStat(); _store.Nets[ssid] = n; }
            if (n.Day != today) { n.Day = today; n.TodayRx = 0; n.TodayTx = 0; }
            n.TotalRx += rx; n.TotalTx += tx; n.TodayRx += rx; n.TodayTx += tx; n.LastSeen = unix;
            _dirty = true;
        }

        void Sample(Func<Guid, (long In, long Out)?> read, long now, string today, long unix)
        {
            if (_cur == null) return;
            var snap = read(_cur.Guid);
            if (snap == null) return;
            long dIn = Delta(_cur.LastIn, snap.Value.In), dOut = Delta(_cur.LastOut, snap.Value.Out);
            long dt = Math.Max(1, now - _cur.LastAt);
            _cur.LastIn = snap.Value.In; _cur.LastOut = snap.Value.Out; _cur.LastAt = now;
            _cur.SessRx += dIn; _cur.SessTx += dOut;
            _cur.RxBps = 0.6 * (dIn * 1000.0 / dt) + 0.4 * _cur.RxBps;   // 轻度平滑，免得数字乱跳
            _cur.TxBps = 0.6 * (dOut * 1000.0 / dt) + 0.4 * _cur.TxBps;
            Add(_cur.Ssid, dIn, dOut, today, unix);
        }

        void CloseSession(Func<Guid, (long In, long Out)?> read, long now, string today, long unix)
        {
            Sample(read, now, today, unix);   // 最后读一次，把断开前的增量记到它头上
            if (_cur == null) return;
            long secs = (now - _cur.StartMs) / 1000;
            if (_cur.SessRx + _cur.SessTx > 0 || secs >= 5)
            {
                _store.Recent.Add(new SessionRec { Ssid = _cur.Ssid, Start = _cur.StartUnix, Secs = secs, Rx = _cur.SessRx, Tx = _cur.SessTx });
                if (_store.Recent.Count > MaxRecent) _store.Recent.RemoveRange(0, _store.Recent.Count - MaxRecent);
            }
            _cur = null;
            _dirty = true; _saveSoon = true;
        }

        public TrafficSnap Feed(CurLink cl, Func<Guid, (long In, long Out)?> read, long now, string today, long unix)
        {
            lock (_lock)
            {
                bool changed = (_cur != null && cl != null) ? (_cur.Ssid != cl.Ssid || _cur.ConnAt != cl.ConnectedAt) : (_cur != null || cl != null);
                if (changed && _cur != null) CloseSession(read, now, today, unix);
                if (_cur == null)
                {
                    if (cl != null)
                    {
                        var s = read(cl.IfGuid);
                        if (s != null)
                        {
                            _cur = new Cur { Ssid = cl.Ssid, ConnAt = cl.ConnectedAt, Guid = cl.IfGuid, LastIn = s.Value.In, LastOut = s.Value.Out, LastAt = now, StartUnix = unix, StartMs = now };
                            if (!_store.Nets.TryGetValue(cl.Ssid, out var n)) { n = new NetStat(); _store.Nets[cl.Ssid] = n; }
                            n.Sessions++; n.LastSeen = unix;
                            if (n.Day != today) { n.Day = today; n.TodayRx = 0; n.TodayTx = 0; }
                            _dirty = true;
                        }
                    }
                }
                else Sample(read, now, today, unix);
                return Snapshot(now, today);
            }
        }

        TrafficSnap Snapshot(long now, string today)
        {
            if (_cur == null) return new TrafficSnap();
            _store.Nets.TryGetValue(_cur.Ssid, out var n);
            n = n ?? new NetStat();
            bool sameDay = n.Day == today;
            return new TrafficSnap
            {
                Connected = true, Ssid = _cur.Ssid,
                RxBps = (long)Math.Round(_cur.RxBps), TxBps = (long)Math.Round(_cur.TxBps),
                SessRx = _cur.SessRx, SessTx = _cur.SessTx, SessSecs = (now - _cur.StartMs) / 1000,
                TodayRx = sameDay ? n.TodayRx : 0, TodayTx = sameDay ? n.TodayTx : 0,
                TotalRx = n.TotalRx, TotalTx = n.TotalTx,
            };
        }

        /// <summary>界面用：按网络的汇总（累计用量降序）+ 按连接的记录（进行中的排第一）。</summary>
        public (List<KeyValuePair<string, NetStat>> Nets, List<SessionRec> Sessions, string Current) Stats(long now)
        {
            lock (_lock)
            {
                var nets = _store.Nets.OrderByDescending(kv => kv.Value.TotalRx + kv.Value.TotalTx).ToList();
                var sess = new List<SessionRec>();
                if (_cur != null)
                    sess.Add(new SessionRec { Ssid = _cur.Ssid, Start = _cur.StartUnix, Secs = (now - _cur.StartMs) / 1000, Rx = _cur.SessRx, Tx = _cur.SessTx, Active = true });
                for (int i = _store.Recent.Count - 1; i >= 0; i--) sess.Add(_store.Recent[i]);
                return (nets, sess, _cur?.Ssid);
            }
        }

        /// <summary>清零某个网络（null = 全部）的用量统计。当前连接的“本次用量”不动。</summary>
        public void Reset(string ssid)
        {
            lock (_lock)
            {
                if (ssid == null) { _store.Nets.Clear(); _store.Recent.Clear(); }
                else { _store.Nets.Remove(ssid); _store.Recent.RemoveAll(r => r.Ssid == ssid); }
                _dirty = true; _saveSoon = true;
            }
            Save();
        }

        public void Save()
        {
            string json;
            lock (_lock)
            {
                if (!_dirty) return;
                json = JsonSerializer.Serialize(_store);
                _dirty = false; _saveSoon = false;
            }
            try
            {
                Directory.CreateDirectory(Path.GetDirectoryName(FilePath));
                string tmp = FilePath + ".tmp";
                File.WriteAllText(tmp, json);
                File.Move(tmp, FilePath, true);
            }
            catch { lock (_lock) _dirty = true; }
        }

        // ---------- 采样线程 ----------
        readonly System.Diagnostics.Stopwatch _clock = System.Diagnostics.Stopwatch.StartNew();
        /// <summary>本服务自己的单调时钟（毫秒）；Stats() 的 now 参数用它。</summary>
        public long NowMs => _clock.ElapsedMilliseconds;

        public async Task RunAsync(CancellationToken ct)
        {
            TrafficSnap lastEmit = new TrafficSnap();
            long lastSave = _clock.ElapsedMilliseconds;
            while (!ct.IsCancellationRequested)
            {
                bool fast = IsWatching?.Invoke() ?? false;
                try { await Task.Delay(fast ? 1000 : 3000, ct); } catch (OperationCanceledException) { break; }
                try
                {
                    long now = _clock.ElapsedMilliseconds;
                    long unix = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
                    string today = DateTime.Now.ToString("yyyy-MM-dd", System.Globalization.CultureInfo.InvariantCulture);
                    var snap = Feed(GetCurrent?.Invoke(), g => ReadCounters(g, now), now, today, unix);
                    if (_saveSoon || now - lastSave > 60000) { Save(); lastSave = now; }
                    BusyBps = snap.RxBps + snap.TxBps;
                    Snap = snap;
                    // 速度/用量有变化才推送（SessSecs 每秒都变，不算）
                    bool changed = snap.Connected != lastEmit.Connected || snap.Ssid != lastEmit.Ssid || snap.RxBps != lastEmit.RxBps
                        || snap.TxBps != lastEmit.TxBps || snap.SessRx != lastEmit.SessRx || snap.SessTx != lastEmit.SessTx
                        || snap.TodayRx != lastEmit.TodayRx || snap.TodayTx != lastEmit.TodayTx || snap.TotalRx != lastEmit.TotalRx || snap.TotalTx != lastEmit.TotalTx;
                    if (fast && changed) Updated?.Invoke(snap);
                    lastEmit = snap;
                }
                catch { /* 采样线程不能因为一次异常退出 */ }
            }
            Save();
        }

        // ---------- 格式化（悬浮框 / 主窗口共用） ----------
        public static string FmtBytes(long n)
        {
            if (n < 1024) return n + " B";
            string[] u = { "KB", "MB", "GB", "TB" };
            double v = n / 1024.0; int i = 0;
            while (v >= 1024 && i < u.Length - 1) { v /= 1024; i++; }
            return (v >= 100 ? v.ToString("0") : v >= 10 ? v.ToString("0.0") : v.ToString("0.00")) + " " + u[i];
        }

        public static string FmtRate(long bps) => FmtBytes(bps) + "/s";

        public static string FmtDuration(long secs)
        {
            secs = Math.Max(0, secs);
            long h = secs / 3600, m = secs % 3600 / 60;
            return h > 0 ? $"{h} 小时 {m} 分" : m > 0 ? $"{m} 分 {secs % 60} 秒" : $"{secs} 秒";
        }
    }
}
