using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace WifiPriority.Models
{
    public abstract class NotifyBase : INotifyPropertyChanged
    {
        public event PropertyChangedEventHandler PropertyChanged;

        protected void Set<T>(ref T field, T value, [CallerMemberName] string name = null)
        {
            if (Equals(field, value)) return;
            field = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
        }
    }

    public class PriorityItem : NotifyBase
    {
        string _ssid, _status = "";
        int _rank, _signal;

        public string Ssid { get => _ssid; set => Set(ref _ssid, value); }
        public int Rank { get => _rank; set => Set(ref _rank, value); }
        public int Signal { get => _signal; set => Set(ref _signal, value); }
        public string Status { get => _status; set => Set(ref _status, value); }
    }

    public class NearbyItem : NotifyBase
    {
        string _ssid, _bands = "", _savedText = "", _actionText = "加入优先级";
        int _signal;
        bool _canAdd = true;

        public string Ssid { get => _ssid; set => Set(ref _ssid, value); }
        public int Signal { get => _signal; set => Set(ref _signal, value); }
        public string Bands { get => _bands; set => Set(ref _bands, value); }
        public string SavedText { get => _savedText; set => Set(ref _savedText, value); }
        public string ActionText { get => _actionText; set => Set(ref _actionText, value); }
        public bool CanAdd { get => _canAdd; set => Set(ref _canAdd, value); }
    }
}
