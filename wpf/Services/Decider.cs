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
        public bool Retried;        // true = 指定接入点失败后“不指定接入点”的重试，不再重试第二次
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
        int _prevSignal = 100;      // 上一次评估时当前网络的信号（判断“换网前原网络是不是已经很差”）
        long _pendingLastSeen;      // 候选最后一次被选为最优的时间（信号在阈值线上抖动时保留确认计时）
        long _lastDiagAt = Never;   // 上一次写“为什么没切”诊断日志的时间
        long _lastOrderDiagAt = Never;  // 上一次写“被频段策略排在后面”诊断日志的时间
        long _discAt;               // 本次“未连接”状态开始的时间（0 = 当前不是未连接）
        const long DiscGraceMs = 1200;  // 刚断开的短暂窗口：用户在 Windows 里手动换网会先断开再开始连接，别抢着发起自己的连接
        readonly Dictionary<string, long> _bad = new Dictionary<string, long>();   // 仅用于“指定频段失败”（频段级，不是网络级）
        volatile string _noNet;     // 当前连着、但检测不到互联网的 WiFi。一旦离开它就清除，不留冷却
        long _notBefore;            // 系统直接拒绝连接请求后，至少隔 3 秒再试（防止每 250 ms 空转，不是网络冷却）

        /// <summary>联网检测报告：当前连着的这个网络无法上网 → 本轮决策把它当作不可用，回退到下一个。</summary>
        public void ReportNoInternet(string ssid) { _noNet = ssid; }
        public void ClearNoInternet(string ssid) { if (_noNet == ssid) _noNet = null; }
        public bool IsNoInternet(string ssid) => _noNet == ssid;

        /// <summary>程序正在切换到的网络（连接请求已发出、结果还没落定），给悬浮框显示“正在切换到 …”。</summary>
        public string ActiveTarget => _lastPlan?.ToLabel;

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
            _lastSwitch = Never;   // 请求根本没发出去，不该占用升级冷却
            _notBefore = now + 3000;
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
            if (st.Connected || st.Connecting) _discAt = 0;
            else if (_discAt == 0) _discAt = now;
            if (_noNet != null && (!st.Connected || st.Ssid != _noNet)) _noNet = null;   // 离开了那个无网络的 WiFi，不留记忆
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
                    if (!onTarget && _lastPlan.Bssid != null && !_lastPlan.Retried)
                    {
                        // 指定接入点（BSSID）没连上：BSS 列表是缓存，那个接入点可能已经不在了。
                        // 先退一步只按配置文件重试一次，再决定要不要拉黑。
                        var lp = _lastPlan;
                        var retry = new Plan
                        {
                            Ssid = lp.Ssid, Band = 0, Bssid = null, Iface = lp.Iface, Profile = lp.Profile,
                            Reason = lp.Reason + "（指定接入点失败，改为不指定重试）",
                            FromLabel = lp.FromLabel, ToLabel = lp.ToLabel, Retried = true
                        };
                        d.Logs.Add($"指定接入点连接 {lp.ToLabel} 未成功，改为只按配置文件重试");
                        _lastPlan = retry;
                        _lastPlanAt = now;
                        d.Connect = retry;
                        return d;
                    }
                    if (onTarget)
                    {
                        MarkBad(_lastPlan.Key(), now + 120000);   // 只针对“指定这个频段”，不影响连接这个网络
                        d.Logs.Add($"未能切换到 {Label(_lastPlan.Ssid, _lastPlan.Band)}（路由器或网卡驱动没有接受），2 分钟内不再尝试指定该频段");
                    }
                    else
                    {
                        // 不做失败冷却：目标网络还在、还是最优，就会再次尝试
                        d.Logs.Add($"连接 {_lastPlan.Ssid} 没有成功，将再次尝试");
                    }
                    _lastPlan = null;
                }
            }

            // 2) 手动换网检测：用户在 Windows 里自己换了网络 → 暂停一段时间，避免和用户对着干
            if (st.Connected)
            {
                bool changed = _prev != null && st.Ssid != _prev;
                bool expected = st.Ssid == _expected;
                // 换网之前原网络信号就已低于回落阈值：这是 Windows 自己在信号变差时漫游，不是用户手动换网
                bool sysRoam = _prevSignal < s.DownThreshold;
                if (changed && !expected && !sysRoam && s.ManualPauseMinutes > 0)
                {
                    d.PauseMinutes = s.ManualPauseMinutes;
                    paused = true;
                    ResetPending();
                    d.Logs.Add($"检测到网络被手动切换到 {st.Ssid}，自动切换暂停 {s.ManualPauseMinutes} 分钟");
                }
                else if (changed && !expected && sysRoam)
                    d.Logs.Add($"系统自行切换到 {st.Ssid}（原网络信号已低于回落阈值），不视为手动换网");
                _prev = st.Ssid;
                _prevSignal = st.Signal;
            }
            else _prev = null;

            if (st.LocationDenied || paused) { ResetPending(); return d; }

            // 3) 连接请求还在进行中：不打断
            if (st.Connecting || _lastPlan != null) return d;

            // 刚断开（<1.2 秒）：可能是用户在 Windows 里手动换网（先断开再开始连接），也可能是系统自己在重连。
            // 这时抢着发起连接会把对方的连接打断，表现为“无法连接网络”。先等一下，看对方是否开始连接。
            if (!st.Connected && now - _discAt < DiscGraceMs) return d;
            if (now < _notBefore) return d;

            var pri = s.Priorities.ToList();
            string policy = s.BandPolicy == "Off" || s.BandPolicy == "Global" ? s.BandPolicy : "Same";
            bool prefer24 = s.PreferBand == "2.4G";
            var cands = BuildCands(st, pri, policy);

            bool IsCurrent(Cand c) =>
                cur != null && c.Ssid == cur && (policy == "Off" || st.CurBand == 0 || c.Band == st.CurBand);

            bool IsBad(Cand c) => IsBadKey(c.Key, now) || (_noNet != null && c.Ssid == _noNet);

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

            string WhyNot(Cand c)
            {
                if (IsBad(c)) return "刚才没连上或无法上网，暂不选它";
                bool saved = !string.IsNullOrEmpty(c.Profile) && (st.Profiles.Contains(c.Profile) || st.Profiles.Contains(c.Ssid));
                if (!saved && !IsCurrent(c)) return "没有已保存的配置文件";
                int th = relaxed ? s.DownThreshold : s.UpThreshold;
                string kind = relaxed ? "回落" : "升级";
                return c.Signal < th ? $"信号 {c.Signal}% 低于{kind}阈值 {th}%" : null;
            }

            var ordered = Order(cands, policy, prefer24).ToList();
            var best = ordered.FirstOrDefault(Usable);

            // 诊断：排在当前网络前面的候选被挡住时，说明原因（30 秒最多一条），免得用户看不出“为什么没切”
            bool held = best == null || IsCurrent(best);
            if (held && now - _lastDiagAt > 30000)
            {
                int cp = ordered.FindIndex(IsCurrent);
                if (cp < 0) cp = ordered.Count;
                for (int i = 0; i < cp; i++)
                {
                    string why = WhyNot(ordered[i]);
                    if (why == null) continue;
                    _lastDiagAt = now;
                    d.Logs.Add($"优先网络 {Label(ordered[i].Ssid, ordered[i].Band)} 暂不切换：{why}");
                    break;
                }
            }

            // 诊断：“全局偏好频段优先”把当前网络排在了列表里更靠前的网络之前（例如当前是 5G，更靠前的网络只有 2.4G），
            // 这会让“出现更高优先级网络”永远不触发切换。这是策略的设计，但用户通常想不到，所以 5 分钟提示一次。
            if (held && policy == "Global" && now - _lastOrderDiagAt > 300000)
            {
                int cp2 = ordered.FindIndex(IsCurrent);
                if (cp2 >= 0)
                {
                    var curC = ordered[cp2];
                    for (int j = cp2 + 1; j < ordered.Count; j++)
                    {
                        if (ordered[j].Rank < curC.Rank && Usable(ordered[j]))
                        {
                            _lastOrderDiagAt = now;
                            d.Logs.Add($"优先网络 {Label(ordered[j].Ssid, ordered[j].Band)} 信号正常，但“全局偏好频段优先”把当前网络排在了它前面，所以没有切换；想让列表顺序绝对优先，请在 设置 → 频段偏好 选“仅同一网络内”");
                            break;
                        }
                    }
                }
            }

            if (best == null || IsCurrent(best))
            {
                // 没有可切换的候选，或最优的就是当前网络。
                // 候选只是短暂掉出阈值（信号在阈值线上下抖动，≤1.5 秒）：保留确认计时，不要清零
                if (_pendingKey != null && now - _pendingLastSeen < 1500) return d;
                ResetPending();
                return d;
            }
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
                _pendingLastSeen = now;
                bool waitConfirm = now - _pendingSince < Math.Max(0, s.ConfirmMs);
                bool cooling = (curOk || !curListed) && now - _lastSwitch < Math.Max(0, s.CooldownSec) * 1000L;
                if (waitConfirm || cooling) return d;
            }

            string fromLabel = cur == null ? null : Label(cur, st.CurBand);

            // 只有“同一个 WiFi 有多个频段、需要在频段之间选”时才指定接入点（BSSID）。
            // 换到另一个 WiFi 时不指定：BSS 列表是缓存，手机热点的接入点地址经常变化，指定缓存里的旧地址会连不上。
            bool multiBand = cands.Where(c => c.Ssid == best.Ssid && c.Band > 0).Select(c => c.Band).Distinct().Count() > 1;
            bool pin = policy != "Off" && best.Band > 0 && multiBand && (cur == null || cur == best.Ssid);
            var plan = new Plan
            {
                Ssid = best.Ssid, Band = pin ? best.Band : 0, Iface = best.Iface, Profile = best.Profile,
                Bssid = pin ? best.Bssid : null,
                Reason = reason, FromLabel = fromLabel, ToLabel = Label(best.Ssid, best.Band)
            };
            ResetPending();
            if (cur != null) _lastSwitch = now;   // 只有“从一个网络换到另一个”才进入升级冷却；从未连接状态连上不算
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
