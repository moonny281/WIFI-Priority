using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Threading;
using WifiPriority.Models;
using WifiPriority.Services;

namespace WifiPriority
{
    /// <summary>桌面悬浮框：显示当前连接的 WiFi。可拖动、吸附四角、调透明度/大小、锁定穿透、随时开关。</summary>
    public partial class OverlayWindow : Window
    {
        readonly AppSettings S;

        // 层级：置顶模式下，别的置顶窗口出现后悬浮框不会自己回到最上面，所以要“重申”
        readonly DispatcherTimer _topTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
        Native.WinEventProc _fgProc;          // 必须持有引用，否则会被 GC 回收
        IntPtr _fgHook = IntPtr.Zero;

        bool IsTopLayer => S.OverlayLayer != "Desktop";

        public OverlayWindow(AppSettings s)
        {
            S = s;
            InitializeComponent();
            SourceInitialized += (a, b) => { ApplyNativeStyles(); ApplyLayer(); };
            SizeChanged += (a, b) => Reposition();
            Root.MouseLeftButtonDown += Root_MouseLeftButtonDown;

            _topTimer.Tick += (a, b) => { if (IsVisible && IsTopLayer) ApplyLayer(); };
            _fgProc = (h, ev, hw, o, c, t, tm) =>
            {
                // 别的窗口刚被激活：等它自己的置顶动作做完再重申，所以放到较低优先级执行
                Dispatcher.BeginInvoke(new Action(() => { if (IsVisible && IsTopLayer) ApplyLayer(); }), DispatcherPriority.Background);
            };
            Closed += (a, b) => { _topTimer.Stop(); Native.Unhook(_fgHook); _fgHook = IntPtr.Zero; };
        }

        /// <summary>应用/重申层级：Top = 悬浮在所有窗口之上；Desktop = 只在桌面（被普通窗口盖住）。</summary>
        void ApplyLayer()
        {
            var h = new WindowInteropHelper(this).EnsureHandle();
            Topmost = IsTopLayer;                 // 让 WPF 的属性和实际状态一致
            Native.SetOverlayLayer(h, IsTopLayer);
        }

        void UpdateLayerWatchers()
        {
            if (IsTopLayer && S.OverlayEnabled)
            {
                if (!_topTimer.IsEnabled) _topTimer.Start();
                if (_fgHook == IntPtr.Zero) _fgHook = Native.HookForeground(_fgProc);
            }
            else
            {
                _topTimer.Stop();
                Native.Unhook(_fgHook);
                _fgHook = IntPtr.Zero;
            }
        }

        void ApplyNativeStyles()
        {
            var h = new WindowInteropHelper(this).EnsureHandle();
            Native.SetOverlayStyles(h, S.OverlayLocked);
        }

        public void ApplySettings()
        {
            Opacity = Math.Max(0.2, Math.Min(1.0, S.OverlayOpacity));
            double sc = Math.Max(0.7, Math.Min(2.0, S.OverlayScale));
            ScaleT.ScaleX = sc;
            ScaleT.ScaleY = sc;
            Bars.Visibility = S.OverlayShowSignal ? Visibility.Visible : Visibility.Collapsed;
            ApplyNativeStyles();

            if (S.OverlayEnabled)
            {
                if (!IsVisible) Show();
                UpdateLayout();
                Reposition();
                ApplyLayer();                     // 每次显示 / 设置变化都重申层级
            }
            else if (IsVisible)
            {
                Hide();
            }

            UpdateLayerWatchers();
        }

        public void SetAnchor(string anchor)
        {
            S.OverlayAnchor = anchor;
            App.SaveSettings();
            Reposition();
        }

        void Reposition()
        {
            if (!IsVisible) return;
            double w = ActualWidth, h = ActualHeight;
            if (w < 1 || h < 1) return;

            var wa = SystemParameters.WorkArea;
            const double m = 6;
            switch (S.OverlayAnchor)
            {
                case "TopLeft": Left = wa.Left + m; Top = wa.Top + m; break;
                case "TopRight": Left = wa.Right - w - m; Top = wa.Top + m; break;
                case "BottomLeft": Left = wa.Left + m; Top = wa.Bottom - h - m; break;
                case "BottomRight": Left = wa.Right - w - m; Top = wa.Bottom - h - m; break;
                default:
                    double vl = SystemParameters.VirtualScreenLeft, vt = SystemParameters.VirtualScreenTop;
                    double vr = vl + SystemParameters.VirtualScreenWidth, vb = vt + SystemParameters.VirtualScreenHeight;
                    Left = Math.Min(Math.Max(S.OverlayX, vl), Math.Max(vl, vr - w));
                    Top = Math.Min(Math.Max(S.OverlayY, vt), Math.Max(vt, vb - h));
                    break;
            }
        }

        void Root_MouseLeftButtonDown(object sender, MouseButtonEventArgs e)
        {
            if (S.OverlayLocked) return;
            double l0 = Left, t0 = Top;
            try { DragMove(); } catch { }
            if (Math.Abs(Left - l0) > 1 || Math.Abs(Top - t0) > 1)
            {
                S.OverlayAnchor = "Free";
                S.OverlayX = Left;
                S.OverlayY = Top;
                App.SaveSettings();
            }
        }

        public void UpdateStatus(EngineStatus st)
        {
            if (st.Connecting && st.Target != null)
            {
                SsidText.Text = "正在切换到 " + st.Target + "…";
                SubText.Text = st.Connected ? "当前 " + st.Ssid : "正在连接";
                Bars.Value = 0;
            }
            else if (st.LocationDenied)
            {
                SsidText.Text = "需要开启位置服务";
                SubText.Text = "Windows 需授权才能读取 WiFi";
                Bars.Value = 0;
            }
            else if (st.Connected)
            {
                SsidText.Text = st.Ssid;
                string band = st.CurBand > 0 ? " · " + Wifi.BandName(st.CurBand) : "";
                SubText.Text = (S.OverlayShowSignal ? $"信号 {st.Signal}%" : "已连接") + band + (st.Paused ? " · 已暂停" : "");
                Bars.Value = st.Signal;
            }
            else
            {
                SsidText.Text = "未连接";
                SubText.Text = st.Paused ? "已暂停自动切换" : "正在寻找网络…";
                Bars.Value = 0;
            }
        }

        void OpenMain_Click(object sender, RoutedEventArgs e) => App.ShowMain();

        void Anchor_Click(object sender, RoutedEventArgs e)
        {
            if (sender is MenuItem mi && mi.Tag is string tag) SetAnchor(tag);
        }

        void Lock_Click(object sender, RoutedEventArgs e)
        {
            S.OverlayLocked = true;
            App.SaveSettings();
            ApplySettings();
        }

        void Hide_Click(object sender, RoutedEventArgs e)
        {
            S.OverlayEnabled = false;
            App.SaveSettings();
            ApplySettings();
        }
    }
}
