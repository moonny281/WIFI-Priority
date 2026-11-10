using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace WifiPriority.Services
{
    /// <summary>
    /// 直接调用 Windows 原生 WLAN API（wlanapi.dll）：
    ///  - 读取当前连接 / 附近网络 / BSSID 列表只需要毫秒级的进程内调用（以前是每次启动 3 个 netsh 进程）
    ///  - 订阅 WLAN 通知：断线、连上、扫描完成时立刻唤醒引擎，不用等轮询
    ///  - 直接 WlanConnect，不再 fork netsh
    /// 结构体偏移按 x64 布局硬编码，并带有合理性校验；校验失败时 ReadState 返回 null，引擎回退到 netsh。
    /// </summary>
    public sealed class WlanClient : IDisposable
    {
        // ---------- P/Invoke ----------
        [UnmanagedFunctionPointer(CallingConvention.Winapi)]
        delegate void NotifyCallback(IntPtr data, IntPtr context);

        [DllImport("wlanapi.dll")] static extern uint WlanOpenHandle(uint ver, IntPtr res, out uint negotiated, out IntPtr h);
        [DllImport("wlanapi.dll")] static extern uint WlanCloseHandle(IntPtr h, IntPtr res);
        [DllImport("wlanapi.dll")] static extern uint WlanEnumInterfaces(IntPtr h, IntPtr res, out IntPtr list);
        [DllImport("wlanapi.dll")] static extern uint WlanScan(IntPtr h, ref Guid g, IntPtr ssid, IntPtr ie, IntPtr res);
        [DllImport("wlanapi.dll")] static extern uint WlanGetAvailableNetworkList(IntPtr h, ref Guid g, uint flags, IntPtr res, out IntPtr list);
        [DllImport("wlanapi.dll")] static extern uint WlanGetNetworkBssList(IntPtr h, ref Guid g, IntPtr ssid, int bssType, [MarshalAs(UnmanagedType.Bool)] bool secure, IntPtr res, out IntPtr list);
        [DllImport("wlanapi.dll")] static extern uint WlanGetProfileList(IntPtr h, ref Guid g, IntPtr res, out IntPtr list);
        [DllImport("wlanapi.dll")] static extern uint WlanQueryInterface(IntPtr h, ref Guid g, int opcode, IntPtr res, out uint size, out IntPtr data, IntPtr type);
        [DllImport("wlanapi.dll")] static extern uint WlanConnect(IntPtr h, ref Guid g, ref ConnParams p, IntPtr res);
        [DllImport("wlanapi.dll")] static extern uint WlanRegisterNotification(IntPtr h, uint source, [MarshalAs(UnmanagedType.Bool)] bool ignoreDup, NotifyCallback cb, IntPtr ctx, IntPtr res, out uint prev);
        [DllImport("wlanapi.dll")] static extern void WlanFreeMemory(IntPtr p);

        [StructLayout(LayoutKind.Sequential)]
        struct ConnParams
        {
            public int Mode;             // 0 = wlan_connection_mode_profile
            public IntPtr Profile;       // LPCWSTR
            public IntPtr Ssid;          // 可空
            public IntPtr BssidList;     // 可空：PDOT11_BSSID_LIST
            public int BssType;          // 1 = infrastructure
            public uint Flags;
        }

        const uint SourceAcm = 0x8;
        const uint ErrAccessDenied = 5;
        const int IfaceInfoSize = 532;   // WLAN_INTERFACE_INFO：GUID(16) + WCHAR[256] + state(4)
        const int AvailNetSize = 628;    // WLAN_AVAILABLE_NETWORK
        const int BssEntrySize = 360;    // WLAN_BSS_ENTRY
        const int ProfileInfoSize = 516; // WLAN_PROFILE_INFO：WCHAR[256] + flags(4)

        // ---------- 状态 ----------
        IntPtr _h = IntPtr.Zero;
        readonly List<Guid> _ifaces = new List<Guid>();
        NotifyCallback _cb;            // 必须保持引用，否则会被 GC 回收
        DateTime _ifaceAt = DateTime.MinValue;
        HashSet<string> _profiles = new HashSet<string>();
        DateTime _profilesAt = DateTime.MinValue;

        /// <summary>原生读取的结构体校验失败过（之后一律走 netsh）。</summary>
        public bool Broken { get; private set; }
        public bool IsOpen => _h != IntPtr.Zero;

        /// <summary>任何 WLAN 通知（扫描完成=7、扫描失败=8、断线、连上……）。可能在任意线程触发。</summary>
        public event Action<uint> Notified;

        static WlanClient()
        {
            try { Encoding.RegisterProvider(CodePagesEncodingProvider.Instance); } catch { }
        }

        public bool Open()
        {
            try
            {
                if (WlanOpenHandle(2, IntPtr.Zero, out _, out _h) != 0) { _h = IntPtr.Zero; return false; }
                _cb = OnNotify;
                WlanRegisterNotification(_h, SourceAcm, false, _cb, IntPtr.Zero, IntPtr.Zero, out _);
                RefreshInterfaces();
                return true;
            }
            catch { _h = IntPtr.Zero; return false; }
        }

        void OnNotify(IntPtr data, IntPtr ctx)
        {
            try
            {
                if (data == IntPtr.Zero) return;
                uint src = (uint)Marshal.ReadInt32(data, 0);
                uint code = (uint)Marshal.ReadInt32(data, 4);
                if (src != SourceAcm) return;
                Notified?.Invoke(code);
            }
            catch { }
        }

        void RefreshInterfaces()
        {
            _ifaceAt = DateTime.Now;
            if (_h == IntPtr.Zero) return;
            IntPtr list;
            if (WlanEnumInterfaces(_h, IntPtr.Zero, out list) != 0) return;
            try
            {
                int n = Marshal.ReadInt32(list, 0);
                var fresh = new List<Guid>();
                for (int i = 0; i < n && i < 16; i++)
                    fresh.Add(Marshal.PtrToStructure<Guid>(IntPtr.Add(list, 8 + i * IfaceInfoSize)));
                _ifaces.Clear();
                _ifaces.AddRange(fresh);
            }
            finally { WlanFreeMemory(list); }
        }

        /// <summary>请求所有网卡立即扫描（异步，完成时会收到 Notified(7)）。</summary>
        /// <summary>返回 true = 至少有一块网卡接受了扫描请求（之后会收到通知 7/8）；false = 不会有通知，调用方不要空等。</summary>
        public bool Scan()
        {
            if (_h == IntPtr.Zero) return false;
            bool any = false;
            for (int i = 0; i < _ifaces.Count; i++)
            {
                Guid g = _ifaces[i];
                try { if (WlanScan(_h, ref g, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero) == 0) any = true; } catch { }
            }
            return any;
        }

        /// <summary>定向扫描某个 SSID（发送指向该 SSID 的探测请求）：比泛扫描更快，也能发现隐藏/弱信号网络。</summary>
        public void ScanDirected(string ssid)
        {
            if (_h == IntPtr.Zero || string.IsNullOrEmpty(ssid)) return;
            var bytes = Encoding.UTF8.GetBytes(ssid);
            if (bytes.Length == 0 || bytes.Length > 32) return;

            IntPtr p = Marshal.AllocHGlobal(36);               // DOT11_SSID：uSSIDLength(4) + ucSSID[32]
            try
            {
                for (int i = 0; i < 36; i++) Marshal.WriteByte(p, i, 0);
                Marshal.WriteInt32(p, 0, bytes.Length);
                Marshal.Copy(bytes, 0, IntPtr.Add(p, 4), bytes.Length);
                for (int i = 0; i < _ifaces.Count; i++)
                {
                    Guid g = _ifaces[i];
                    try { WlanScan(_h, ref g, p, IntPtr.Zero, IntPtr.Zero); } catch { }
                }
            }
            finally { Marshal.FreeHGlobal(p); }
        }

        // ---------- 读取 ----------
        static string DecodeSsid(byte[] b)
        {
            try { return new UTF8Encoding(false, true).GetString(b); } catch { }
            try { return Encoding.GetEncoding("GB18030").GetString(b); } catch { }
            return Encoding.Latin1.GetString(b);
        }

        static string ReadSsid(IntPtr p)
        {
            int len = Marshal.ReadInt32(p, 0);
            if (len <= 0 || len > 32) return null;
            var b = new byte[len];
            Marshal.Copy(IntPtr.Add(p, 4), b, 0, len);
            return DecodeSsid(b);
        }

        static string ReadMac(IntPtr p, int offset)
        {
            var sb = new StringBuilder(17);
            for (int i = 0; i < 6; i++)
            {
                if (i > 0) sb.Append(':');
                sb.Append(Marshal.ReadByte(p, offset + i).ToString("x2"));
            }
            return sb.ToString();
        }

        static int BandFromKhz(uint khz) => khz < 3000000 ? 2 : khz < 5925000 ? 5 : 6;

        void RefreshProfiles(Guid g)
        {
            if ((DateTime.Now - _profilesAt).TotalSeconds < 20 && _profiles.Count > 0) return;
            _profilesAt = DateTime.Now;
            IntPtr list;
            if (WlanGetProfileList(_h, ref g, IntPtr.Zero, out list) != 0) return;
            try
            {
                int n = Marshal.ReadInt32(list, 0);
                var set = new HashSet<string>();
                for (int i = 0; i < n && i < 512; i++)
                {
                    string name = Marshal.PtrToStringUni(IntPtr.Add(list, 8 + i * ProfileInfoSize));
                    if (!string.IsNullOrEmpty(name)) set.Add(name);
                }
                _profiles = set;
            }
            finally { WlanFreeMemory(list); }
        }

        /// <summary>读取完整状态（通常 &lt; 5 ms）。结构体校验失败返回 null。</summary>
        public WifiState ReadState()
        {
            if (_h == IntPtr.Zero || Broken) return null;
            try
            {
                if ((DateTime.Now - _ifaceAt).TotalSeconds > 10) RefreshInterfaces();

                var st = new WifiState { Fast = true };
                var map = new Dictionary<string, NearbyNetwork>();
                int totalBss = 0, validBss = 0;

                for (int i = 0; i < _ifaces.Count; i++)
                {
                    Guid g = _ifaces[i];

                    // 1) 当前连接
                    IntPtr data;
                    uint size;
                    uint r = WlanQueryInterface(_h, ref g, 7, IntPtr.Zero, out size, out data, IntPtr.Zero);
                    if (r == ErrAccessDenied) st.LocationDenied = true;
                    else if (r == 0 && data != IntPtr.Zero)
                    {
                        try
                        {
                            int state = Marshal.ReadInt32(data, 0);
                            if (state == 1 && !st.Connected)
                            {
                                string ssid = ReadSsid(IntPtr.Add(data, 520));
                                if (ssid != null)
                                {
                                    st.Connected = true;
                                    st.Ssid = ssid;
                                    st.CurIface = i;
                                    st.CurProfile = Marshal.PtrToStringUni(IntPtr.Add(data, 8));
                                    st.CurBssid = ReadMac(data, 560);
                                    st.Signal = (int)Math.Min(100u, (uint)Marshal.ReadInt32(data, 576));
                                }
                            }
                            else if (state == 5 || state == 6 || state == 7) st.Connecting = true;   // 关联/发现/认证中
                        }
                        finally { WlanFreeMemory(data); }
                    }

                    // 2) 已保存配置名（可见网络自带 profile 名，比 SSID 对应关系更准）
                    var profByName = new Dictionary<string, string>();
                    IntPtr alist;
                    uint r2 = WlanGetAvailableNetworkList(_h, ref g, 0, IntPtr.Zero, out alist);
                    if (r2 == ErrAccessDenied) st.LocationDenied = true;
                    else if (r2 == 0 && alist != IntPtr.Zero)
                    {
                        try
                        {
                            int n = Marshal.ReadInt32(alist, 0);
                            for (int k = 0; k < n && k < 512; k++)
                            {
                                IntPtr e = IntPtr.Add(alist, 8 + k * AvailNetSize);
                                string ssid = ReadSsid(IntPtr.Add(e, 512));
                                string prof = Marshal.PtrToStringUni(e);
                                if (ssid != null && !string.IsNullOrEmpty(prof)) profByName[ssid] = prof;
                            }
                        }
                        finally { WlanFreeMemory(alist); }
                    }

                    // 3) BSSID 列表：信号、频率（→ 频段）
                    IntPtr blist;
                    uint r3 = WlanGetNetworkBssList(_h, ref g, IntPtr.Zero, 3, false, IntPtr.Zero, out blist);
                    if (r3 == ErrAccessDenied) st.LocationDenied = true;
                    else if (r3 == 0 && blist != IntPtr.Zero)
                    {
                        try
                        {
                            int n = Marshal.ReadInt32(blist, 4);
                            for (int k = 0; k < n && k < 1024; k++)
                            {
                                IntPtr e = IntPtr.Add(blist, 8 + k * BssEntrySize);
                                totalBss++;
                                int rssi = Marshal.ReadInt32(e, 56);
                                uint lq = (uint)Marshal.ReadInt32(e, 60);
                                uint khz = (uint)Marshal.ReadInt32(e, 92);
                                // 合理性校验：不符合说明结构体偏移不对
                                if (lq > 100 || rssi > 0 || rssi < -127 || khz < 2300000 || khz > 7200000) continue;
                                validBss++;
                                string ssid = ReadSsid(e);
                                if (ssid == null) continue;      // 隐藏网络
                                NearbyNetwork net;
                                if (!map.TryGetValue(ssid, out net))
                                {
                                    net = new NearbyNetwork { Ssid = ssid };
                                    map[ssid] = net;
                                }
                                string pn;
                                if (profByName.TryGetValue(ssid, out pn)) net.Profile = pn;
                                net.Bss.Add(new BssInfo
                                {
                                    Bssid = ReadMac(e, 40),
                                    Signal = (int)lq,
                                    Rssi = rssi,
                                    Band = BandFromKhz(khz),
                                    Iface = i
                                });
                            }
                        }
                        finally { WlanFreeMemory(blist); }
                    }

                    RefreshProfiles(g);
                }

                // 有 BSS 条目但一条都通不过校验 → 偏移不对，放弃原生读取
                if (totalBss > 0 && validBss == 0) { Broken = true; return null; }

                foreach (var n in map.Values) Wifi.Summarize(n);
                st.Nearby = new List<NearbyNetwork>(map.Values);
                st.Profiles = _profiles;

                // 当前连接的频段：用 BSSID 反查
                if (st.Connected)
                {
                    foreach (var n in st.Nearby)
                    {
                        if (n.Ssid != st.Ssid) continue;
                        foreach (var b in n.Bss)
                            if (b.Bssid == st.CurBssid) { st.CurBand = b.Band; break; }
                    }
                }
                return st;
            }
            catch { return null; }
        }

        // ---------- 连接 ----------
        static byte[] ParseMac(string s)
        {
            if (string.IsNullOrEmpty(s)) return null;
            var parts = s.Split(':', '-');
            if (parts.Length != 6) return null;
            var mac = new byte[6];
            for (int i = 0; i < 6; i++)
                if (!byte.TryParse(parts[i], System.Globalization.NumberStyles.HexNumber, null, out mac[i])) return null;
            return mac;
        }

        /// <summary>发出连接请求（异步完成）。bssid 不为空时指定接入点——用来在同名的 5G / 2.4G 之间选频段。</summary>
        public bool Connect(string profile, string bssid, int iface)
        {
            if (_h == IntPtr.Zero || string.IsNullOrEmpty(profile)) return false;
            IntPtr pProfile = IntPtr.Zero, pList = IntPtr.Zero;
            try
            {
                if (_ifaces.Count == 0) return false;
                Guid g = _ifaces[iface >= 0 && iface < _ifaces.Count ? iface : 0];
                pProfile = Marshal.StringToHGlobalUni(profile);

                var mac = ParseMac(bssid);
                if (mac != null)
                {
                    // DOT11_BSSID_LIST：NDIS 头(4) + uNumOfEntries(4) + uTotalNumOfEntries(4) + MAC(6) + 对齐(2) = 20 字节
                    pList = Marshal.AllocHGlobal(20);
                    for (int i = 0; i < 20; i++) Marshal.WriteByte(pList, i, 0);
                    Marshal.WriteByte(pList, 0, 0x80);        // NDIS_OBJECT_TYPE_DEFAULT
                    Marshal.WriteByte(pList, 1, 1);           // DOT11_BSSID_LIST_REVISION_1
                    Marshal.WriteInt16(pList, 2, 20);         // Size
                    Marshal.WriteInt32(pList, 4, 1);          // uNumOfEntries
                    Marshal.WriteInt32(pList, 8, 1);          // uTotalNumOfEntries
                    for (int i = 0; i < 6; i++) Marshal.WriteByte(pList, 12 + i, mac[i]);
                }

                var prm = new ConnParams
                {
                    Mode = 0,
                    Profile = pProfile,
                    Ssid = IntPtr.Zero,
                    BssidList = pList,
                    BssType = 1,
                    Flags = 0
                };
                return WlanConnect(_h, ref g, ref prm, IntPtr.Zero) == 0;
            }
            catch { return false; }
            finally
            {
                if (pProfile != IntPtr.Zero) Marshal.FreeHGlobal(pProfile);
                if (pList != IntPtr.Zero) Marshal.FreeHGlobal(pList);
            }
        }

        public void Dispose()
        {
            if (_h != IntPtr.Zero)
            {
                try { WlanCloseHandle(_h, IntPtr.Zero); } catch { }
                _h = IntPtr.Zero;
            }
        }
    }
}
