using System;
using System.Collections.Generic;
using System.Linq;
using WifiPriority.Models;

namespace WifiPriority.Services
{
    /// <summary>一次需要执行的连接。</summary>
    public class Plan
    {
        public string Ssid;
        public int Band;            // 0 = 不区分
        public string Bssid;        // 非空：指定接入点（用来选频段）
        public int Iface = -1;
        public string Profile;
        public string Reason, FromLabel, ToLabel;
    }

    public class Decision
    {
        public Plan Connect;
        public int PauseMinutes;                         // > 0：检测到手动换网，需要暂停这么多分钟
        public List<string> Logs = new List<string>();
    }

    /// <summary>
    /// 纯决策逻辑（不做任何 IO，时间由调用方以毫秒传入，便于测试）。
    ///
    /// 排序规则（BandPolicy）：Off 仅按列表顺序；Same 先按列表顺序、同一网络内偏好频段；Global 偏好频段的网络整体优先。
    /// 升级：更优候选信号 &gt;= UpThreshold，持续 ConfirmMs 毫秒，且过了冷却时间。
    /// 回落：当前候选信号 &lt; DownThreshold 或消失，持续 ConfirmMs 毫秒后切到次优候选（此时只要求 &gt;= DownThreshold，不受冷却限制）。
    /// 未连接：立刻连最优候选（不等确认）。
    /// </summary>
    public class Decider
    {
        class Cand
        {
            public string Ssid, Bssid, Profile;
            public int Band, Signal, Rank, Iface;
            public string Key => Ssid + "|" + Band;
        }

        const long Never = long.MinValue / 4;
        const long SettleMs = 4000;      // 发出连接请求后，最多等这么久看结果

        string _pendingKey;
        long _pendingSince;
        string _prev;
        string _expected;
        long _lastSwitch = Never;
        Plan _lastPlan;
        long _lastPlanAt;
        readonly Dictionary<string, long> _bad = new Dictionary<string, long>();

        public void MarkBad(string key, long untilMs) { lock (_bad) _bad[key] = untilMs; }

        bool IsBadKey(string key, long now)
        {
            lock (_bad) return _bad.TryGetValue(key, out var u) && u > now;
        }

        /// <summary>引擎执行连接后回报结果（false = 系统没有接受连接请求）。</summary>
        public void ConnectResult(Plan p, bool ok, long now)
        {
            if (ok) return;
            _lastPlan = null;
            MarkBad(p.Ssid, now + 120000);
        }

        void ResetPending() { _pendingKey = null; }

        static string Label(string ssid, int band) =>
            band > 0 && ssid != null ? $"{ssid}（{Wifi.BandName(band)}）" : ssid;

        static List<Cand> BuildCands(WifiState st, List<string> pri, string policy)
        {
            var list = new List<Cand>();
            for (int i = 0; i < pri.Count; i++)
            {
                var n = st.Nearby.FirstOrDefault(x => x.Ssid == pri[i]);
                if (n == null) continue;

                if (policy == "Off" || n.Bss.Count == 0)
                {
                    var any = n.Bss.OrderByDescending(x => x.Signal).FirstOrDefault();
                    list.Add(new Cand
                    {
                        Ssid = n.Ssid, Band = 0, Bssid = null, Rank = i, Profile = n.Profile ?? n.Ssid,
                        Iface = any != null ? any.Iface : -1,
                        Signal = st.Connected && n.Ssid == st.Ssid ? st.Signal : n.Signal
                    });
                    continue;
                }
                // 每个频段取信号最强的接入点
                foreach (var g in n.Bss.GroupBy(b => b.Band))
                {
                    var b = g.OrderByDescending(x => x.Signal).First();
                    list.Add(new Cand
                    {
                        Ssid = n.Ssid, Band = b.Band, Bssid = b.Bssid, Signal = b.Signal, Rank = i,
                        Iface = b.Iface, Profile = n.Profile ?? n.Ssid
                    });
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

        public Decision Decide(WifiState st, AppSettings s, long now, bool paused)
        {
            var d = new Decision();
            string cur = st.Connected ? st.Ssid : null;

            // 1) 上一次连接请求的结果（不阻塞：每次评估时顺带检查）
            if (_lastPlan != null)
            {
                bool onTarget = st.Connected && st.Ssid == _lastPlan.Ssid;
                bool bandOk = _lastPlan.Band == 0 || st.CurBand == 0 || st.CurBand == _lastPlan.Band;
                long el = now - _lastPlanAt;
                if (onTarget && bandOk && !st.Connecting)
                {
                    _lastPlan = null;                                   // 成功
                }
                else if (el >= SettleMs && !st.Connecting)
                {
                    if (onTarget)
                    {
                        MarkBad(_lastPlan.Key(), now + 600000);
                        d.Logs.Add($"未能切换到 {Label(_lastPlan.Ssid, _lastPlan.Band)}（路由器或网卡驱动没有接受），10 分钟内不再尝试该频段");
                    }
                    else
                    {
                        MarkBad(_lastPlan.Ssid, now + 30000);
                        d.Logs.Add($"连接 {_lastPlan.Ssid} 没有成功，30 秒内不再尝试");
                    }
                    _lastPlan = null;
                }
            }

            // 2) 手动换网检测：用户在 Windows 里自己换了网络 → 暂停一段时间，避免和用户对着干
            if (st.Connected)
            {
                if (_prev != null && st.Ssid != _prev && st.Ssid != _expected && s.ManualPauseMinutes > 0)
                {
                    d.PauseMinutes = s.ManualPauseMinutes;
                    paused = true;
                    ResetPending();
                    d.Logs.Add($"检测到网络被手动切换到 {st.Ssid}，自动切换暂停 {s.ManualPauseMinutes} 分钟");
                }
                _prev = st.Ssid;
            }
            else _prev = null;

            if (st.LocationDenied || paused) { ResetPending(); return d; }

            // 3) 连接请求还在进行中：不打断
            if (st.Connecting || _lastPlan != null) return d;

            var pri = s.Priorities.ToList();
            string policy = s.BandPolicy == "Off" || s.BandPolicy == "Same" ? s.BandPolicy : "Global";
            bool prefer24 = s.PreferBand == "2.4G";
            var cands = BuildCands(st, pri, policy);

            bool IsCurrent(Cand c) =>
                cur != null && c.Ssid == cur && (policy == "Off" || st.CurBand == 0 || c.Band == st.CurBand);

            bool IsBad(Cand c) => IsBadKey(c.Ssid, now) || IsBadKey(c.Key, now);

            var curCand = cands.Where(IsCurrent).OrderByDescending(c => c.Signal).FirstOrDefault();
            bool curListed = cur != null && pri.Contains(cur);
            bool curOk = curCand != null && !IsBad(curCand) && curCand.Signal >= s.DownThreshold;

            // 未连接，或当前（列表内）网络已经不行了：只要求候选 >= 回落阈值，避免卡在烂信号上
            bool relaxed = cur == null || (curListed && !curOk);

            bool Usable(Cand c)
            {
                if (IsBad(c)) return false;
                bool isCur = IsCurrent(c);
                bool saved = !string.IsNullOrEmpty(c.Profile) && (st.Profiles.Contains(c.Profile) || st.Profiles.Contains(c.Ssid));
                if (!isCur && !saved) return false;                     // 没有已保存配置，无法连接
                int th = (isCur || relaxed) ? s.DownThreshold : s.UpThreshold;
                return c.Signal >= th;
            }

            var best = Order(cands, policy, prefer24).FirstOrDefault(Usable);
            if (best == null || IsCurrent(best)) { ResetPending(); return d; }
            if (cur != null && !curListed && !s.TakeoverUnlisted) { ResetPending(); return d; }

            bool sameSsid = cur != null && best.Ssid == cur;
            string reason = cur == null ? "当前未连接"
                : !curListed ? "当前网络不在优先级列表中"
                : curOk ? (sameSsid ? $"同一网络有更优频段（{Wifi.BandName(best.Band)}）" : "出现更高优先级网络")
                : (sameSsid ? "当前频段信号丢失或过弱，回落到另一频段" : "当前网络信号丢失或过弱，回落到次优先级");

            // 4) 防抖：未连接时立刻连；否则候选必须持续最优 ConfirmMs 毫秒，升级还要过冷却时间
            if (cur != null)
            {
                if (_pendingKey != best.Key) { _pendingKey = best.Key; _pendingSince = now; }
                bool waitConfirm = now - _pendingSince < Math.Max(0, s.ConfirmMs);
                bool cooling = (curOk || !curListed) && now - _lastSwitch < Math.Max(0, s.CooldownSec) * 1000L;
                if (waitConfirm || cooling) return d;
            }

            string fromLabel = cur == null ? null : Label(cur, st.CurBand);
            var plan = new Plan
            {
                Ssid = best.Ssid, Band = best.Band, Iface = best.Iface, Profile = best.Profile,
                Bssid = policy != "Off" && best.Band > 0 ? best.Bssid : null,
                Reason = reason, FromLabel = fromLabel, ToLabel = Label(best.Ssid, best.Band)
            };
            ResetPending();
            _lastSwitch = now;
            _lastPlan = plan;
            _lastPlanAt = now;
            _expected = plan.Ssid;
            d.Connect = plan;
            return d;
        }
    }

    static class PlanExt
    {
        public static string Key(this Plan p) => p.Ssid + "|" + p.Band;
    }
}
