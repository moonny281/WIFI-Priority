using System;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using Microsoft.Win32;

namespace WifiPriority.Services
{
    public static class ThemeService
    {
        public static bool IsDark { get; private set; }
        static bool _applied;

        public static bool SystemIsDark()
        {
            try
            {
                var v = Registry.GetValue(
                    @"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                    "AppsUseLightTheme", 1);
                return v is int i && i == 0;
            }
            catch { return false; }
        }

        /// <summary>mode: System / Light / Dark</summary>
        public static void Apply(string mode)
        {
            bool dark = mode == "Dark" || (mode != "Light" && SystemIsDark());
            if (_applied && dark == IsDark) return;
            IsDark = dark;
            _applied = true;

            var app = Application.Current;
            var uri = new Uri(dark ? "/WifiPriority;component/Themes/Dark.xaml"
                                   : "/WifiPriority;component/Themes/Light.xaml", UriKind.Relative);
            app.Resources.MergedDictionaries[0] = new ResourceDictionary { Source = uri };

            // Windows 10 没有 Mica：改用不透明的纯色背景
            if (!Native.IsMicaSupported) app.Resources["WindowTint"] = app.Resources["WindowSolid"];
            else app.Resources.Remove("WindowTint");

            if (App.Main != null) ApplyMica(App.Main);
        }

        public static void ApplyMica(Window w)
        {
            var h = new WindowInteropHelper(w).EnsureHandle();
            var src = HwndSource.FromHwnd(h);
            if (src != null && src.CompositionTarget != null)
                src.CompositionTarget.BackgroundColor = Colors.Transparent;
            Native.ApplyMica(h, IsDark);
        }
    }
}
