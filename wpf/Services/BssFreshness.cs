using System;
using System.Collections.Generic;
using WifiPriority.Models;

namespace WifiPriority.Services
{
    /// <summary>
    /// 识别 Windows BSS 缓存里“早已消失”的接入点。
    ///
    /// 背景：WlanGetNetworkBssList 读的是缓存。热点关掉后，它的条目（连同当时的信号强度）会在缓存里留好一阵子，
    /// 程序会以为它还在，于是一直尝试连接一个根本不存在的网络。
    ///
    /// 做法（不依赖时间戳的单位，只看“变没变”）：每个接入点条目都有 ullHostTimestamp（收到它最近一帧的时间）。
    /// 接入点还在时，每次扫描完成它的时间戳都会更新；关掉后时间戳就不再变化。
    /// 所以：最近两次扫描完成之后，时间戳都没变化的条目 = 已经不在了（Stale）。
    ///  - 启动时缓存里就有的条目先算“未确认”，等第一次扫描完成后时间戳变化了才确认在场；
    ///  - 启动之后才第一次出现的条目一定是刚收到的帧，直接算在场（换了地址的热点会立刻被认出来）；
    ///  - 驱动没给时间戳（0）时无法判断，按“在”处理；
    ///  - 超过 15 秒收不到任何扫描完成通知，说明这个机制拿不到依据，放宽为“在”，避免永远不切换；
    ///  - 当前连着的接入点一定在。
    /// </summary>
    public class BssFreshness
    {
        class Rec { public ulong Ts; public long ChangedAt; }

        readonly object _lock = new object();
        readonly long _startAt;
        readonly Dictionary<string, Rec> _ts = new Dictionary<string, Rec>(StringComparer.OrdinalIgnoreCase);
        readonly List<long> _done = new List<long>();       // 最近 3 次扫描完成的时间（升序）

        public BssFreshness(long now) { _startAt = now; }

        public void ScanDone(long now)
        {
            lock (_lock)
            {
                _done.Add(now);
                if (_done.Count > 3) _done.RemoveAt(0);
            }
        }

        public void Apply(WifiState st, long now)
        {
            lock (_lock)
            {
                var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
                foreach (var n in st.Nearby)
                {
                    foreach (var b in n.Bss)
                    {
                        b.Stale = false;
                        if (string.IsNullOrEmpty(b.Bssid)) continue;
                        seen.Add(b.Bssid);
                        if (b.HostTs == 0) { b.ChangedAt = -1; continue; }   // 驱动没给时间戳：无法判断，按“在”处理

                        Rec r;
                        if (!_ts.TryGetValue(b.Bssid, out r))
                        {
                            bool startup = _done.Count == 0 && now - _startAt < 5000;
                            r = new Rec { Ts = b.HostTs, ChangedAt = startup ? 0 : now };
                            _ts[b.Bssid] = r;
                        }
                        else if (r.Ts != b.HostTs) { r.Ts = b.HostTs; r.ChangedAt = now; }

                        if (st.CurBssid != null && string.Equals(st.CurBssid, b.Bssid, StringComparison.OrdinalIgnoreCase)) { b.ChangedAt = now; continue; }   // 当前连着的接入点一定在
                        b.ChangedAt = r.ChangedAt;
                        b.Stale = IsStale(r.ChangedAt, now);
                    }
                }
                // 从列表里消失的条目：忘掉（以后再出现就是新条目，算在场）
                var gone = new List<string>();
                foreach (var k in _ts.Keys) if (!seen.Contains(k)) gone.Add(k);
                foreach (var k in gone) _ts.Remove(k);
            }
        }

        bool IsStale(long changedAt, long now)
        {
            int n = _done.Count;
            if (n == 0) return changedAt == 0 && now - _startAt <= 15000;
            long thr = n >= 3 ? _done[0] + 300 : 0;                 // 最近两次扫描之前的那次完成时间（+ 观察延迟余量）
            return changedAt <= thr;
        }
    }
}
