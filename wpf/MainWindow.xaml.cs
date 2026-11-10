using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.Linq;
using System.Reflection;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Input;
using System.Windows.Media;
using WifiPriority.Models;
using WifiPriority.Services;

namespace WifiPriority
{
    public partial class MainWindow : Window
    {
        readonly AppSettings S = App.Settings;
        readonly ObservableCollection<PriorityItem> _pri = new ObservableCollection<PriorityItem>();
        readonly ObservableCollection<NearbyItem> _near = new ObservableCollection<NearbyItem>();
        readonly ObservableCollection<string> _logs = new ObservableCollection<string>();

        bool _ready, _loading;
        EngineStatus _last;
        Point _dragStart;
        PriorityItem _dragItem;

        public MainWindow()
        {
            InitializeComponent();

            // 版本号（来自构建时注入的版本，去掉 +commit 后缀）
            string ver = typeof(App).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion ?? "";
            int plus = ver.IndexOf('+');
            if (plus > 0) ver = ver.Substring(0, plus);
            VersionText.Text = ver.Length > 0 ? "v" + ver : "";

            PriorityList.ItemsSource = _pri;
            NearbyList.ItemsSource = _near;
            LogList.ItemsSource = _logs;

            // 附近网络按信号强度实时排序
            var view = CollectionViewSource.GetDefaultView(_near);
            view.SortDescriptions.Add(new SortDescription(nameof(NearbyItem.Signal), ListSortDirection.Descending));
            if (view is ICollectionViewLiveShaping ls && ls.CanChangeLiveSorting)
            {
                ls.LiveSortingProperties.Add(nameof(NearbyItem.Signal));
                ls.IsLiveSorting = true;
            }

            foreach (var ssid in S.Priorities) _pri.Add(new PriorityItem { Ssid = ssid });
            Renumber();
            _pri.CollectionChanged += (a, b) => UpdateEmptyHint();
            UpdateEmptyHint();

            foreach (var l in App.GetLogHistory()) _logs.Add(l);
            App.LogAdded += l => Dispatcher.BeginInvoke(new Action(() => AddLog(l)));
            App.StatusChanged += OnStatus;

            LoadSettingsToUi();

            SourceInitialized += (a, b) => ThemeService.ApplyMica(this);
            Closing += (a, e) =>
            {
                if (!App.Exiting) { e.Cancel = true; Hide(); }   // 关闭窗口 = 最小化到托盘
            };
            Activated += (a, b) => { LoadSettingsToUi(); RefreshPauseUi(); };
            Loaded += async (a, b) =>
            {
                bool en = await Task.Run(() => AutoStart.IsEnabled());
                _loading = true;
                AutoStartSwitch.IsChecked = en;
                _loading = false;
            };

            _ready = true;
        }

        // ================= 导航 =================
        void Nav_Checked(object sender, RoutedEventArgs e)
        {
            if (!_ready) return;
            string tag = (string)((RadioButton)sender).Tag;
            PageOverview.Visibility = tag == "Overview" ? Visibility.Visible : Visibility.Collapsed;
            PagePriority.Visibility = tag == "Priority" ? Visibility.Visible : Visibility.Collapsed;
            PageNearby.Visibility = tag == "Nearby" ? Visibility.Visible : Visibility.Collapsed;
            PageOverlay.Visibility = tag == "Overlay" ? Visibility.Visible : Visibility.Collapsed;
            PageSettings.Visibility = tag == "Settings" ? Visibility.Visible : Visibility.Collapsed;
            PageLog.Visibility = tag == "Log" ? Visibility.Visible : Visibility.Collapsed;
        }

        void ExitApp_Click(object sender, RoutedEventArgs e) => App.ExitApp();

        // ================= 引擎状态刷新 =================
        void OnStatus(EngineStatus st)
        {
            _last = st;

            // 概览
            CurSsid.Text = st.LocationDenied ? "需要开启位置服务" : st.Connected ? st.Ssid : "未连接";
            CurBars.Value = st.Connected && !st.LocationDenied ? st.Signal : 0;
            CurDetail.Text = st.LocationDenied
                ? "Windows 需要你授权“位置服务”才允许程序读取周围的 WiFi。"
                : st.Connected ? $"信号强度 {st.Signal}%" + (st.CurBand > 0 ? $" · {Wifi.BandName(st.CurBand)} 频段" : "")
                : (st.Message ?? "当前没有连接到任何 WiFi");
            LocationBtn.Visibility = st.LocationDenied ? Visibility.Visible : Visibility.Collapsed;
            LastSwitchText.Text = App.LastSwitchText;
            RefreshPauseUi();

            // 优先级列表状态
            foreach (var it in _pri)
            {
                var n = st.Nearby.FirstOrDefault(x => x.Ssid == it.Ssid);
                it.Signal = n != null ? n.Signal : 0;
                bool isCur = st.Connected && it.Ssid == st.Ssid;
                string s = isCur ? "当前连接" + (st.CurBand > 0 ? " · " + Wifi.BandName(st.CurBand) : "")
                    : n != null ? "可见" + (n.BandTags.Length > 0 ? " · " + n.BandTags : "")
                    : "不可见";
                if (!isCur && !st.Profiles.Contains(it.Ssid)) s += " · 未保存配置";
                it.Status = s;
            }

            // 附近网络（原地更新，避免列表跳动）
            var seen = new System.Collections.Generic.HashSet<string>();
            foreach (var n in st.Nearby)
            {
                seen.Add(n.Ssid);
                var it = _near.FirstOrDefault(x => x.Ssid == n.Ssid);
                if (it == null) { it = new NearbyItem { Ssid = n.Ssid }; _near.Add(it); }
                bool inList = S.Priorities.Contains(n.Ssid);
                it.Signal = n.Signal;
                it.Bands = n.Bands;
                it.SavedText = st.Connected && n.Ssid == st.Ssid ? "已连接" : st.Profiles.Contains(n.Ssid) ? "已保存" : "未保存";
                it.CanAdd = !inList;
                it.ActionText = inList ? "已在列表" : "加入优先级";
            }
            for (int i = _near.Count - 1; i >= 0; i--)
                if (!seen.Contains(_near[i].Ssid)) _near.RemoveAt(i);
            NearbyCount.Text = $"共发现 {_near.Count} 个网络（实时刷新）";
        }

        void RefreshPauseUi()
        {
            var eng = App.Engine;
            if (eng == null) return;
            bool paused = eng.Paused;
            PauseBtn.Content = paused ? "恢复自动切换" : "暂停自动切换";

            if (_last != null && _last.LocationDenied)
                StateText.Text = "位置服务未开启，暂时无法扫描周围的 WiFi。";
            else if (paused)
            {
                var until = eng.PausedUntil;
                StateText.Text = until.HasValue ? $"自动切换已暂停，将在 {until.Value:HH:mm} 自动恢复。" : "自动切换已暂停（手动）。";
            }
            else
            {
                string scanText = S.ScanIntervalSec <= 0 ? "连续扫描" : $"每 {S.ScanIntervalSec} 秒扫描一次";
                string mode = App.Engine.NativeMode ? "原生 WLAN 事件驱动" : "netsh 兼容模式（较慢）";
                StateText.Text = $"自动切换运行中（{mode}，{scanText}）。"
                    + (S.Priorities.Count == 0 ? " 还没有设置优先级列表，请先到“优先级”页添加网络。" : "");
            }
        }

        void OpenLocation_Click(object sender, RoutedEventArgs e)
        {
            try { Process.Start(new ProcessStartInfo("ms-settings:privacy-location") { UseShellExecute = true }); } catch { }
        }

        void ScanNow_Click(object sender, RoutedEventArgs e) => App.Engine.TriggerNow();

        void Pause_Click(object sender, RoutedEventArgs e)
        {
            if (App.Engine.Paused) App.Engine.ResumeNow(); else App.Engine.ManualPause = true;
            RefreshPauseUi();
            App.Engine.TriggerNow();
        }

        // ================= 优先级 =================
        void UpdateEmptyHint() =>
            PriorityEmpty.Visibility = _pri.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        void Renumber()
        {
            for (int i = 0; i < _pri.Count; i++) _pri[i].Rank = i + 1;
        }

        void SavePriorities()
        {
            Renumber();
            S.Priorities = _pri.Select(p => p.Ssid).ToList();
            App.SaveSettings();
            App.Engine.TriggerNow();
        }

        void AddPriority(string ssid)
        {
            ssid = (ssid ?? "").Trim();
            if (ssid.Length == 0 || _pri.Any(p => p.Ssid == ssid)) return;
            _pri.Add(new PriorityItem { Ssid = ssid });
            SavePriorities();
            if (_last != null) OnStatus(_last);
        }

        async void AddNetwork_Click(object sender, RoutedEventArgs e)
        {
            string ssid = NewSsid.Text.Trim();
            string pwd = NewPwd.Password;
            if (ssid.Length == 0) { AddHint.Text = "请先输入网络名称。"; return; }
            if (_pri.Any(p => p.Ssid == ssid)) { AddHint.Text = "该网络已在列表中。"; return; }

            if (pwd.Length > 0)
            {
                if (pwd.Length < 8 || pwd.Length > 64)
                {
                    AddHint.Text = "WPA2 密码长度需为 8–63 个字符（或 64 位十六进制）。";
                    return;
                }
                AddHint.Text = "正在保存网络配置…";
                bool ok = await Wifi.AddProfileAsync(ssid, pwd);
                if (!ok) { AddHint.Text = "保存网络配置失败，请检查名称/密码；WPA3、企业网络请先在 Windows 里手动连接一次。"; return; }
                App.Log($"已保存 {ssid} 的网络配置");
            }

            AddPriority(ssid);
            NewSsid.Clear();
            NewPwd.Clear();
            AddHint.Text = pwd.Length > 0 ? "已保存并加入优先级列表。" : "已加入优先级列表。";
        }

        void PriUp_Click(object sender, RoutedEventArgs e)
        {
            var it = (PriorityItem)((FrameworkElement)sender).DataContext;
            int i = _pri.IndexOf(it);
            if (i > 0) { _pri.Move(i, i - 1); SavePriorities(); }
        }

        void PriDown_Click(object sender, RoutedEventArgs e)
        {
            var it = (PriorityItem)((FrameworkElement)sender).DataContext;
            int i = _pri.IndexOf(it);
            if (i >= 0 && i < _pri.Count - 1) { _pri.Move(i, i + 1); SavePriorities(); }
        }

        void PriDel_Click(object sender, RoutedEventArgs e)
        {
            var it = (PriorityItem)((FrameworkElement)sender).DataContext;
            _pri.Remove(it);
            SavePriorities();
            if (_last != null) OnStatus(_last);
        }

        // ---- 拖拽排序 ----
        static PriorityItem ItemUnder(DependencyObject o)
        {
            while (o != null)
            {
                if (o is ListBoxItem lbi) return lbi.DataContext as PriorityItem;
                o = (o is Visual || o is System.Windows.Media.Media3D.Visual3D)
                    ? VisualTreeHelper.GetParent(o)
                    : LogicalTreeHelper.GetParent(o);
            }
            return null;
        }

        void PriorityList_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
        {
            _dragStart = e.GetPosition(null);
            _dragItem = ItemUnder(e.OriginalSource as DependencyObject);
        }

        void PriorityList_PreviewMouseMove(object sender, MouseEventArgs e)
        {
            if (e.LeftButton != MouseButtonState.Pressed || _dragItem == null) return;
            Vector d = e.GetPosition(null) - _dragStart;
            if (Math.Abs(d.X) < SystemParameters.MinimumHorizontalDragDistance &&
                Math.Abs(d.Y) < SystemParameters.MinimumVerticalDragDistance) return;
            var item = _dragItem;
            _dragItem = null;
            DragDrop.DoDragDrop(PriorityList, item, DragDropEffects.Move);
        }

        void PriorityList_Drop(object sender, DragEventArgs e)
        {
            var src = e.Data.GetData(typeof(PriorityItem)) as PriorityItem;
            if (src == null) return;
            var target = ItemUnder(e.OriginalSource as DependencyObject);
            int from = _pri.IndexOf(src);
            int to = target == null ? _pri.Count - 1 : _pri.IndexOf(target);
            if (from < 0 || to < 0 || from == to) return;
            _pri.Move(from, to);
            SavePriorities();
        }

        // ================= 附近网络 =================
        void AddNearby_Click(object sender, RoutedEventArgs e)
        {
            string ssid = (sender as Button)?.Tag as string;
            if (string.IsNullOrEmpty(ssid)) return;

            bool saved = _last != null && _last.Profiles.Contains(ssid);
            if (saved)
            {
                AddPriority(ssid);
            }
            else
            {
                // 没有保存过密码：跳到“优先级”页，让用户输入密码
                NavPriority.IsChecked = true;
                NewSsid.Text = ssid;
                NewPwd.Clear();
                AddHint.Text = "这个网络还没有保存过配置，请输入密码后点“添加到列表”（仅支持 WPA2 个人版）。";
                NewPwd.Focus();
            }
        }

        // ================= 桌面悬浮框 =================
        void OverlayLayer_Checked(object sender, RoutedEventArgs e)
        {
            if (!_ready || _loading) return;
            S.OverlayLayer = (string)((RadioButton)sender).Tag == "Desktop" ? "Desktop" : "Top";
            App.SaveSettings();
            App.Overlay.ApplySettings();
        }

        void Overlay_Changed(object sender, RoutedEventArgs e)
        {
            if (!_ready || _loading) return;
            S.OverlayEnabled = OvEnabled.IsChecked == true;
            S.OverlayShowSignal = OvShowSignal.IsChecked == true;
            S.OverlayLocked = OvLocked.IsChecked == true;
            App.SaveSettings();
            App.Overlay.ApplySettings();
            if (_last != null) App.Overlay.UpdateStatus(_last);
        }

        void OvSlider_Changed(object sender, RoutedPropertyChangedEventArgs<double> e)
        {
            if (!_ready || _loading) return;
            S.OverlayOpacity = Math.Round(OvOpacity.Value, 2);
            S.OverlayScale = Math.Round(OvScale.Value, 2);
            OvOpacityText.Text = $"{S.OverlayOpacity * 100:0}%";
            OvScaleText.Text = $"{S.OverlayScale * 100:0}%";
            App.SaveSettings();
            App.Overlay.ApplySettings();
        }

        void Anchor_Click(object sender, RoutedEventArgs e)
        {
            string tag = (sender as Button)?.Tag as string;
            if (string.IsNullOrEmpty(tag)) return;
            if (!S.OverlayEnabled)
            {
                S.OverlayEnabled = true;
                App.SaveSettings();
                LoadSettingsToUi();
                App.Overlay.ApplySettings();
            }
            App.Overlay.SetAnchor(tag);
        }

        void OvReset_Click(object sender, RoutedEventArgs e)
        {
            S.OverlayOpacity = 0.95;
            S.OverlayScale = 1.0;
            S.OverlayAnchor = "TopRight";
            S.OverlayLocked = false;
            App.SaveSettings();
            LoadSettingsToUi();
            App.Overlay.ApplySettings();
        }

        // ================= 设置 =================
        void LoadSettingsToUi()
        {
            bool old = _loading;
            _loading = true;

            OvEnabled.IsChecked = S.OverlayEnabled;
            OvShowSignal.IsChecked = S.OverlayShowSignal;
            OvLocked.IsChecked = S.OverlayLocked;
            OvLayerTop.IsChecked = S.OverlayLayer != "Desktop";
            OvLayerDesktop.IsChecked = S.OverlayLayer == "Desktop";
            OvOpacity.Value = S.OverlayOpacity;
            OvScale.Value = S.OverlayScale;
            OvOpacityText.Text = $"{S.OverlayOpacity * 100:0}%";
            OvScaleText.Text = $"{S.OverlayScale * 100:0}%";

            TbInterval.Text = S.ScanIntervalSec.ToString();
            TbUp.Text = S.UpThreshold.ToString();
            TbDown.Text = S.DownThreshold.ToString();
            TbConfirmMs.Text = S.ConfirmMs.ToString();
            TbCooldown.Text = S.CooldownSec.ToString();
            TbManualPause.Text = S.ManualPauseMinutes.ToString();
            SwTakeover.IsChecked = S.TakeoverUnlisted;
            SwInternet.IsChecked = S.InternetCheck;
            SwNotify.IsChecked = S.Notify;

            ThemeSystem.IsChecked = S.Theme == "System";
            ThemeLight.IsChecked = S.Theme == "Light";
            ThemeDark.IsChecked = S.Theme == "Dark";

            PolicyOff.IsChecked = S.BandPolicy == "Off";
            PolicyGlobal.IsChecked = S.BandPolicy == "Global";
            PolicySame.IsChecked = S.BandPolicy != "Off" && S.BandPolicy != "Global";
            Prefer24.IsChecked = S.PreferBand == "2.4G";
            Prefer5.IsChecked = S.PreferBand != "2.4G";
            UpdateBandHint();

            _loading = old;
        }

        static int ParseInt(TextBox t, int def, int min, int max)
        {
            if (!int.TryParse(t.Text.Trim(), out int v)) v = def;
            v = Math.Max(min, Math.Min(max, v));
            t.Text = v.ToString();
            return v;
        }

        void Settings_Changed(object sender, RoutedEventArgs e)
        {
            if (!_ready || _loading) return;
            S.ScanIntervalSec = ParseInt(TbInterval, S.ScanIntervalSec, 0, 600);
            S.UpThreshold = ParseInt(TbUp, S.UpThreshold, 10, 100);
            S.DownThreshold = ParseInt(TbDown, S.DownThreshold, 0, 95);
            if (S.DownThreshold > S.UpThreshold) { S.DownThreshold = S.UpThreshold; TbDown.Text = S.DownThreshold.ToString(); }
            S.ConfirmMs = ParseInt(TbConfirmMs, S.ConfirmMs, 0, 10000);
            S.CooldownSec = ParseInt(TbCooldown, S.CooldownSec, 0, 3600);
            S.ManualPauseMinutes = ParseInt(TbManualPause, S.ManualPauseMinutes, 0, 1440);
            S.TakeoverUnlisted = SwTakeover.IsChecked == true;
            S.InternetCheck = SwInternet.IsChecked == true;
            S.Notify = SwNotify.IsChecked == true;
            App.SaveSettings();
            RefreshPauseUi();
        }

        void Band_Checked(object sender, RoutedEventArgs e)
        {
            if (!_ready || _loading) return;
            string tag = (string)((RadioButton)sender).Tag;
            if (tag == "Off" || tag == "Same" || tag == "Global") S.BandPolicy = tag;
            else S.PreferBand = tag;
            App.SaveSettings();
            UpdateBandHint();
            App.Engine.TriggerNow();
        }

        void UpdateBandHint()
        {
            string pref = S.PreferBand == "2.4G" ? "2.4G" : "5G（含 6G）";
            string other = S.PreferBand == "2.4G" ? "其它频段" : "2.4G ";
            BandHint.Text = S.BandPolicy == "Off"
                ? "不区分频段：只按优先级列表顺序，同名的双频网络由 Windows 自己选频段。"
                : S.BandPolicy == "Global"
                    ? $"全局：只要有可用的 {pref} 网络，就优先于所有{other}网络，再按列表顺序。例如列表里靠后的 5G 网络会排在靠前的 2.4G 网络前面——所以当前连着靠后的 5G 网络时，列表靠前的 2.4G 网络出现也不会切换过去。"
                    : $"仅同一网络内（推荐）：列表顺序绝对优先，更高优先级的网络出现就切换；同一个 WiFi 同时有多个频段时，优先连 {pref}。";
        }

        void Theme_Checked(object sender, RoutedEventArgs e)
        {
            if (!_ready || _loading) return;
            string mode = (string)((RadioButton)sender).Tag;
            S.Theme = mode;
            App.SaveSettings();
            ThemeService.Apply(mode);
        }

        async void AutoStart_Click(object sender, RoutedEventArgs e)
        {
            if (!_ready || _loading) return;
            bool want = AutoStartSwitch.IsChecked == true;
            AutoStartSwitch.IsEnabled = false;
            AutoStartHint.Text = "正在请求管理员授权…";

            bool ok = await Task.Run(() => want ? AutoStart.Enable() : AutoStart.Disable());
            bool actual = await Task.Run(() => AutoStart.IsEnabled());

            _loading = true;
            AutoStartSwitch.IsChecked = actual;
            _loading = false;
            AutoStartSwitch.IsEnabled = true;

            if (ok) AutoStartHint.Text = want ? "已创建开机自启任务：登录后自动以最高权限在后台运行。" : "已移除开机自启任务。";
            else AutoStartHint.Text = "操作未完成（可能取消了管理员授权）。";
            App.Log(AutoStartHint.Text);
        }

        // ================= 日志 =================
        void AddLog(string line)
        {
            _logs.Add(line);
            if (_logs.Count > 500) _logs.RemoveAt(0);
            if (PageLog.Visibility == Visibility.Visible) LogList.ScrollIntoView(_logs[_logs.Count - 1]);
        }

        void ClearLog_Click(object sender, RoutedEventArgs e)
        {
            App.ClearLog();
            _logs.Clear();
        }
    }
}
