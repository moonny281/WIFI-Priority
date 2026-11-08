using System;
using System.Runtime.InteropServices;
using System.Threading.Tasks;

namespace WifiPriority.Services
{
    internal static class Native
    {
        // ---------- DWM（云母 / 深色标题栏） ----------
        [StructLayout(LayoutKind.Sequential)]
        struct MARGINS { public int Left, Right, Top, Bottom; }

        [DllImport("dwmapi.dll")]
        static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

        [DllImport("dwmapi.dll")]
        static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref MARGINS margins);

        /// <summary>Windows 11 22H2（build 22621）起支持 Mica 背景。</summary>
        public static bool IsMicaSupported => Environment.OSVersion.Version.Build >= 22621;

        public static void ApplyMica(IntPtr hwnd, bool dark)
        {
            try
            {
                int d = dark ? 1 : 0;
                DwmSetWindowAttribute(hwnd, 20, ref d, 4);           // DWMWA_USE_IMMERSIVE_DARK_MODE
                if (IsMicaSupported)
                {
                    var m = new MARGINS { Left = -1, Right = -1, Top = -1, Bottom = -1 };
                    DwmExtendFrameIntoClientArea(hwnd, ref m);
                    int t = 2;                                        // DWMWA_SYSTEMBACKDROP_TYPE = Mica
                    DwmSetWindowAttribute(hwnd, 38, ref t, 4);
                }
            }
            catch { }
        }

        // ---------- 悬浮框窗口样式 ----------
        const int GWL_EXSTYLE = -20;
        const int WS_EX_TRANSPARENT = 0x20;
        const int WS_EX_TOOLWINDOW = 0x80;
        const int WS_EX_NOACTIVATE = 0x08000000;

        [DllImport("user32.dll")]
        static extern int GetWindowLong(IntPtr hWnd, int nIndex);

        [DllImport("user32.dll")]
        static extern int SetWindowLong(IntPtr hWnd, int nIndex, int dwNewLong);

        public static void SetOverlayStyles(IntPtr hwnd, bool clickThrough)
        {
            try
            {
                int s = GetWindowLong(hwnd, GWL_EXSTYLE);
                s |= WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
                if (clickThrough) s |= WS_EX_TRANSPARENT; else s &= ~WS_EX_TRANSPARENT;
                SetWindowLong(hwnd, GWL_EXSTYLE, s);
            }
            catch { }
        }

        // ---------- WLAN 主动扫描 ----------
        [DllImport("wlanapi.dll")]
        static extern uint WlanOpenHandle(uint dwClientVersion, IntPtr pReserved, out uint pdwNegotiatedVersion, out IntPtr phClientHandle);

        [DllImport("wlanapi.dll")]
        static extern uint WlanCloseHandle(IntPtr hClientHandle, IntPtr pReserved);

        [DllImport("wlanapi.dll")]
        static extern uint WlanEnumInterfaces(IntPtr hClientHandle, IntPtr pReserved, out IntPtr ppInterfaceList);

        [DllImport("wlanapi.dll")]
        static extern uint WlanScan(IntPtr hClientHandle, ref Guid pInterfaceGuid, IntPtr pDot11Ssid, IntPtr pIeData, IntPtr pReserved);

        [DllImport("wlanapi.dll")]
        static extern void WlanFreeMemory(IntPtr pMemory);

        // WLAN_INTERFACE_INFO = GUID(16) + WCHAR[256](512) + state(4) = 532 字节；列表头为两个 DWORD（8 字节）
        const int InterfaceInfoSize = 532;

        // ---------- 指定接入点（BSSID）连接：用来在同名的 5G / 2.4G 之间选频段 ----------
        [StructLayout(LayoutKind.Sequential)]
        struct WLAN_CONNECTION_PARAMETERS
        {
            public int wlanConnectionMode;      // 0 = wlan_connection_mode_profile
            public IntPtr strProfile;           // LPCWSTR
            public IntPtr pDot11Ssid;           // 可为空：用配置文件里的 SSID
            public IntPtr pDesiredBssidList;    // PDOT11_BSSID_LIST
            public int dot11BssType;            // 1 = dot11_BSS_type_infrastructure
            public uint dwFlags;
        }

        [DllImport("wlanapi.dll")]
        static extern uint WlanConnect(IntPtr hClientHandle, ref Guid pInterfaceGuid, ref WLAN_CONNECTION_PARAMETERS pConnectionParameters, IntPtr pReserved);

        static byte[] ParseMac(string s)
        {
            var parts = (s ?? "").Split(':', '-');
            if (parts.Length != 6) return null;
            var mac = new byte[6];
            for (int i = 0; i < 6; i++)
                if (!byte.TryParse(parts[i], System.Globalization.NumberStyles.HexNumber, null, out mac[i])) return null;
            return mac;
        }

        /// <summary>请求连接到指定 BSSID（配置文件名 profile）。返回 true 仅表示系统接受了连接请求。</summary>
        public static bool ConnectToBssid(string profile, string bssid)
        {
            IntPtr h = IntPtr.Zero, pProfile = IntPtr.Zero, pList = IntPtr.Zero;
            try
            {
                var mac = ParseMac(bssid);
                if (mac == null) return false;
                if (WlanOpenHandle(2, IntPtr.Zero, out _, out h) != 0) return false;

                Guid g;
                if (WlanEnumInterfaces(h, IntPtr.Zero, out IntPtr list) != 0) return false;
                try
                {
                    if (Marshal.ReadInt32(list) < 1) return false;
                    g = Marshal.PtrToStructure<Guid>(IntPtr.Add(list, 8));   // 第一块无线网卡
                }
                finally { WlanFreeMemory(list); }

                // DOT11_BSSID_LIST：NDIS 头(4) + uNumOfEntries(4) + uTotalNumOfEntries(4) + MAC(6) + 对齐(2) = 20 字节
                pProfile = Marshal.StringToHGlobalUni(profile);
                pList = Marshal.AllocHGlobal(20);
                for (int i = 0; i < 20; i++) Marshal.WriteByte(pList, i, 0);
                Marshal.WriteByte(pList, 0, 0x80);              // NDIS_OBJECT_TYPE_DEFAULT
                Marshal.WriteByte(pList, 1, 1);                 // DOT11_BSSID_LIST_REVISION_1
                Marshal.WriteInt16(pList, 2, 20);               // Size
                Marshal.WriteInt32(pList, 4, 1);                // uNumOfEntries
                Marshal.WriteInt32(pList, 8, 1);                // uTotalNumOfEntries
                for (int i = 0; i < 6; i++) Marshal.WriteByte(pList, 12 + i, mac[i]);

                var prm = new WLAN_CONNECTION_PARAMETERS
                {
                    wlanConnectionMode = 0,
                    strProfile = pProfile,
                    pDot11Ssid = IntPtr.Zero,
                    pDesiredBssidList = pList,
                    dot11BssType = 1,
                    dwFlags = 0
                };
                return WlanConnect(h, ref g, ref prm, IntPtr.Zero) == 0;
            }
            catch { return false; }
            finally
            {
                if (pProfile != IntPtr.Zero) Marshal.FreeHGlobal(pProfile);
                if (pList != IntPtr.Zero) Marshal.FreeHGlobal(pList);
                if (h != IntPtr.Zero) WlanCloseHandle(h, IntPtr.Zero);
            }
        }

        /// <summary>请求所有无线网卡立即重新扫描，并等待扫描完成。</summary>
        public static async Task TriggerScanAsync()
        {
            IntPtr h = IntPtr.Zero;
            bool scanned = false;
            try
            {
                if (WlanOpenHandle(2, IntPtr.Zero, out _, out h) != 0) return;
                if (WlanEnumInterfaces(h, IntPtr.Zero, out IntPtr list) == 0)
                {
                    try
                    {
                        int n = Marshal.ReadInt32(list);
                        for (int i = 0; i < n; i++)
                        {
                            IntPtr p = IntPtr.Add(list, 8 + i * InterfaceInfoSize);
                            Guid g = Marshal.PtrToStructure<Guid>(p);
                            if (WlanScan(h, ref g, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero) == 0) scanned = true;
                        }
                    }
                    finally { WlanFreeMemory(list); }
                }
                if (scanned) await Task.Delay(2500);
            }
            catch { }
            finally
            {
                if (h != IntPtr.Zero) WlanCloseHandle(h, IntPtr.Zero);
            }
        }
    }
}
