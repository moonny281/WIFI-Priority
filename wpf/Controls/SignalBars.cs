using System.Windows;
using System.Windows.Media;

namespace WifiPriority.Controls
{
    /// <summary>四格信号强度图标，颜色随主题（通过隐式样式绑定 DynamicResource）。</summary>
    public class SignalBars : FrameworkElement
    {
        static FrameworkPropertyMetadata Meta(object def) =>
            new FrameworkPropertyMetadata(def, FrameworkPropertyMetadataOptions.AffectsRender);

        public static readonly DependencyProperty ValueProperty =
            DependencyProperty.Register(nameof(Value), typeof(int), typeof(SignalBars), Meta(0));
        public static readonly DependencyProperty InactiveProperty =
            DependencyProperty.Register(nameof(Inactive), typeof(Brush), typeof(SignalBars), Meta(Brushes.Gray));
        public static readonly DependencyProperty GoodProperty =
            DependencyProperty.Register(nameof(Good), typeof(Brush), typeof(SignalBars), Meta(Brushes.LimeGreen));
        public static readonly DependencyProperty WarnProperty =
            DependencyProperty.Register(nameof(Warn), typeof(Brush), typeof(SignalBars), Meta(Brushes.Orange));
        public static readonly DependencyProperty BadProperty =
            DependencyProperty.Register(nameof(Bad), typeof(Brush), typeof(SignalBars), Meta(Brushes.Red));

        public int Value { get => (int)GetValue(ValueProperty); set => SetValue(ValueProperty, value); }
        public Brush Inactive { get => (Brush)GetValue(InactiveProperty); set => SetValue(InactiveProperty, value); }
        public Brush Good { get => (Brush)GetValue(GoodProperty); set => SetValue(GoodProperty, value); }
        public Brush Warn { get => (Brush)GetValue(WarnProperty); set => SetValue(WarnProperty, value); }
        public Brush Bad { get => (Brush)GetValue(BadProperty); set => SetValue(BadProperty, value); }

        protected override Size MeasureOverride(Size availableSize) => new Size(22, 16);

        protected override void OnRender(DrawingContext dc)
        {
            int v = Value;
            int level = v >= 75 ? 4 : v >= 55 ? 3 : v >= 35 ? 2 : v > 0 ? 1 : 0;
            Brush active = level >= 3 ? Good : level == 2 ? Warn : Bad;
            const double bw = 4, gap = 2;
            for (int i = 0; i < 4; i++)
            {
                double h = 5 + i * 3.6;
                var r = new Rect(i * (bw + gap), 16 - h, bw, h);
                dc.DrawRoundedRectangle(i < level ? active : Inactive, null, r, 1, 1);
            }
        }
    }
}
