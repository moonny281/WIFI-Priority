using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.NetworkInformation;
using System.Threading;
using System.Windows;
using Microsoft.Win32;
using WifiPriority.Models;
using WifiPriority.Services;

namespace WifiPriority
{
    public partial class App : Application
    {
        public static AppSettings Settings;
        public static SwitchEngine Engine;
        /// <summary>主窗口是否可见（引擎线程不能直接读 WPF 控件的 IsVisible，由主窗口的 IsVisibleChanged 维护）。</summary>
        public static volatile bool MainVisible;
        public static MainWindow MainWin;
        public static OverlayWindow Overlay;
        public static TrayService Tray;
        public static bool Exiting;
        public static EngineStatus LastStatus;
        public static string LastSwitchText = "尚无切换记录";

        public static event Action<EngineStatus> StatusChanged;   // 在 UI 线程触发
        public static event Action<string> LogAdded;              // 可能在任意线程触发

        static readonly List<string> LogHistory = new List<string>();
        static readonly object LogLock = new object();
        Mutex _mutex;

        public static void Log(string msg)
        {
            string line = $"{DateTime.Now:HH:mm:ss}  {msg}";
            lock (LogLock)
            {
                LogHistory.Add(line);
                if (LogHistory.Count > 500) LogHistory.RemoveAt(0);
            }
            LogAdded?.Invoke(line);
        }

        public static List<string> GetLogHistory()
        {
            lock (LogLock) return LogHistory.ToList();
        }

        public static void ClearLog()
        {
            lock (LogLock) LogHistory.Clear();
        }

        public static void SaveSettings() => SettingsStore.Save(Settings);

        protected override void OnStartup(StartupEventArgs e)
        {
            // v1.2.0 降低内存：界面很简单，用不着 D3D 硬件加速；软件渲染可以省掉 GPU 相关的显存映射和 D3D 设备（几十 MB 工作集）
            System.Windows.Media.RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly;

            base.OnStartup(e);

            bool background = e.Args.Any(a => a.Equals("--background", StringComparison.OrdinalIgnoreCase));

            // 单实例（以管理员权限运行的实例对普通权限的第二个实例不可见，会走 Unauthorized 分支）
            bool created = false;
            try { _mutex = new Mutex(true, "WifiPriority.SingleInstance", out created); }
            catch (UnauthorizedAccessException) { created = false; }
            if (!created)
            {
                if (!background)
                    MessageBox.Show("WiFi 优先级 已在后台运行。\n请在任务栏右下角的系统托盘里点击它的图标打开主窗口。",
                        "WiFi 优先级", MessageBoxButton.OK, MessageBoxImage.Information);
                Shutdown();
                return;
            }

            DispatcherUnhandledException += (s, ex) =>
            {
                Log("未处理的异常：" + ex.Exception.Message);
                ex.Handled = true;
            };

            Settings = SettingsStore.Load();
            ThemeService.Apply(Settings.Theme);

            Engine = new SwitchEngine(Settings);
            Engine.Log += m => Log(m);
            // 有人在看流量（悬浮框显示流量 / 主窗口可见）才 1 秒采样并推送；否则 3 秒采一次，只为把用量记对
            Engine.IsWatching = () => (Settings.OverlayEnabled && Settings.OverlayShowTraffic) || MainVisible;
            Engine.Traffic.Updated += snap => Dispatcher.BeginInvoke(new Action(() =>
            {
                Overlay?.SetTraffic(snap);
                MainWin?.OnTraffic(snap);
            }));
            if (SettingsStore.InternetCheckMigrated) SettingsStore.Save(Settings);   // v1.1.8：联网检测默认开启，把迁移结果落盘
            if (SettingsStore.BandPolicyMigrated)
            {
                SettingsStore.Save(Settings);
                Log("v1.1.3：频段策略已由“全局偏好频段优先”改为“仅同一网络内”（列表顺序绝对优先）——旧策略会让当前的 5G 网络排在列表更靠前的 2.4G 网络之前，导致更高优先级的网络出现时不切换。想恢复可在 设置 → 频段偏好 里改回");
            }
            Engine.Updated += st => Dispatcher.BeginInvoke(new Action(() =>
            {
                LastStatus = st;
                Tray?.Update(st);
                Overlay?.UpdateStatus(st);
                StatusChanged?.Invoke(st);
            }));
            Engine.Switched += (f, t, reason) => Dispatcher.BeginInvoke(new Action(() =>
            {
                LastSwitchText = $"{DateTime.Now:MM-dd HH:mm}   {f ?? "(未连接)"} → {t}   （{reason}）";
                if (Settings.Notify) Tray?.Balloon("已切换 WiFi", $"{f ?? "未连接"} → {t}\n{reason}");
            }));

            MainWin = new MainWindow();
            Overlay = new OverlayWindow(Settings);

            // 后台内存：主窗口不可见时每 60 秒做一次压缩式 GC 并收缩工作集；启动 6 秒后先收一次（去掉启动峰值）
            {
                Action trim = () =>
                {
                    if (MainVisible) return;
                    GC.Collect(2, GCCollectionMode.Forced, true, true);
                    Services.Native.TrimWorkingSet();
                };
                var first = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(6) };
                first.Tick += (s2, a2) => { first.Stop(); trim(); };
                first.Start();
                var every = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(60) };
                every.Tick += (s2, a2) => trim();
                every.Start();
            }

            Tray = new TrayService();
            Tray.OpenRequested = ShowMain;
            Tray.ScanRequested = () => Engine.TriggerNow();
            Tray.PauseToggled = () =>
            {
                if (Engine.Paused) Engine.ResumeNow(); else Engine.ManualPause = true;
                Engine.TriggerNow();
            };
            Tray.OverlayToggled = () =>
            {
                Settings.OverlayEnabled = !Settings.OverlayEnabled;
                SaveSettings();
                Overlay.ApplySettings();
            };
            Tray.LockToggled = () =>
            {
                Settings.OverlayLocked = !Settings.OverlayLocked;
                SaveSettings();
                Overlay.ApplySettings();
            };
            Tray.ExitRequested = ExitApp;

            // 系统唤醒 / 网络变化 / 主题变化
            SystemEvents.PowerModeChanged += (s, a) =>
            {
                if (a.Mode == PowerModes.Resume)
                {
                    Log("系统唤醒，重新扫描");
                    Engine.TriggerNow();
                }
            };
            SystemEvents.UserPreferenceChanged += (s, a) =>
            {
                if (a.Category == UserPreferenceCategory.General && Settings.Theme == "System")
                    Dispatcher.BeginInvoke(new Action(() => ThemeService.Apply("System")));
            };
            NetworkChange.NetworkAddressChanged += (s, a) => Engine?.TriggerNow();

            Overlay.ApplySettings();
            Engine.Start();
            Log("程序已启动");

            if (!background) ShowMain();
        }

        public static void ShowMain()
        {
            MainWin.Show();
            if (MainWin.WindowState == WindowState.Minimized) MainWin.WindowState = WindowState.Normal;
            MainWin.Activate();
        }

        public static void ExitApp()
        {
            Exiting = true;
            try { Engine?.Stop(); } catch { }
            try { Tray?.Dispose(); } catch { }
            try { Overlay?.Close(); } catch { }
            try { MainWin?.Close(); } catch { }
            Current.Shutdown();
        }
    }
}
