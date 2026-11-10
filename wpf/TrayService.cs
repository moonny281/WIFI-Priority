using System;
using System.Drawing;
using System.Windows.Forms;
using WifiPriority.Services;

namespace WifiPriority
{
    /// <summary>系统托盘图标（已连接 / 未连接 / 已暂停三种状态）与右键菜单。</summary>
    public class TrayService : IDisposable
    {
        readonly NotifyIcon _ni;
        readonly ToolStripMenuItem _pause, _overlay, _lock;
        Icon _icon;
        int _state = -1;

        public Action OpenRequested, ScanRequested, PauseToggled, OverlayToggled, LockToggled, ExitRequested;

        public TrayService()
        {
            var menu = new ContextMenuStrip();
            var open = new ToolStripMenuItem("打开主窗口");
            open.Click += (s, e) => OpenRequested?.Invoke();
            var scan = new ToolStripMenuItem("立即扫描");
            scan.Click += (s, e) => ScanRequested?.Invoke();
            _pause = new ToolStripMenuItem("暂停自动切换");
            _pause.Click += (s, e) => PauseToggled?.Invoke();
            _overlay = new ToolStripMenuItem("显示桌面悬浮框");
            _overlay.Click += (s, e) => OverlayToggled?.Invoke();
            _lock = new ToolStripMenuItem("锁定悬浮框（鼠标穿透）");
            _lock.Click += (s, e) => LockToggled?.Invoke();
            var exit = new ToolStripMenuItem("退出");
            exit.Click += (s, e) => ExitRequested?.Invoke();

            menu.Items.Add(open);
            menu.Items.Add(scan);
            menu.Items.Add(new ToolStripSeparator());
            menu.Items.Add(_pause);
            menu.Items.Add(_overlay);
            menu.Items.Add(_lock);
            menu.Items.Add(new ToolStripSeparator());
            menu.Items.Add(exit);
            menu.Opening += (s, e) =>
            {
                _pause.Text = App.Engine != null && App.Engine.Paused ? "恢复自动切换" : "暂停自动切换";
                _overlay.Checked = App.Settings.OverlayEnabled;
                _lock.Checked = App.Settings.OverlayLocked;
            };

            _icon = LoadIcon("tray-idle.ico");
            _ni = new NotifyIcon
            {
                Visible = true,
                Text = "WiFi 优先级",
                ContextMenuStrip = menu,
                Icon = _icon
            };
            _ni.MouseClick += (s, e) =>
            {
                if (e.Button == MouseButtons.Left) OpenRequested?.Invoke();
            };
        }

        public void Update(EngineStatus st)
        {
            int state = st.Paused ? 2 : st.Connected ? 1 : 0;
            if (state != _state)
            {
                _state = state;
                var old = _icon;
                _icon = LoadIcon(state == 2 ? "tray-paused.ico" : state == 1 ? "tray-connected.ico" : "tray-idle.ico");
                _ni.Icon = _icon;
                old?.Dispose();
            }
            string t = st.Connected ? "WiFi：" + st.Ssid : "WiFi：未连接";
            if (st.Paused) t += "（已暂停）";
            _ni.Text = t.Length > 60 ? t.Substring(0, 60) : t;   // NotifyIcon.Text 最长 63 字符
        }

        public void Balloon(string title, string text)
        {
            try { _ni.ShowBalloonTip(4000, title, text, ToolTipIcon.Info); } catch { }
        }

        static Icon LoadIcon(string name)
        {
            try
            {
                var info = System.Windows.Application.GetResourceStream(new Uri("pack://application:,,,/Assets/" + name));
                using (var stream = info.Stream) return new Icon(stream, new Size(32, 32));
            }
            catch { return SystemIcons.Application; }
        }

        public void Dispose()
        {
            _ni.Visible = false;
            _ni.Dispose();
            _icon?.Dispose();
        }
    }
}
