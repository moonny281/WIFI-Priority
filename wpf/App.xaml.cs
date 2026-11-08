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
        public static MainWindow Main;
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

            Main = new MainWindow();
            Overlay = new OverlayWindow(Settings);

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
            Main.Show();
            if (Main.WindowState == WindowState.Minimized) Main.WindowState = WindowState.Normal;
            Main.Activate();
        }

        public static void ExitApp()
        {
            Exiting = true;
            try { Engine?.Stop(); } catch { }
            try { Tray?.Dispose(); } catch { }
            try { Overlay?.Close(); } catch { }
            try { Main?.Close(); } catch { }
            Current.Shutdown();
        }
    }
}
