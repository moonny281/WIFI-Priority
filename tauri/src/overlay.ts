import './overlay.css';
import { listen } from '@tauri-apps/api/event';
import { api, applyTheme, bandName, level, type Settings, type Status } from './common';

const root = document.getElementById('root') as HTMLElement;
const barsEl = document.getElementById('bars') as HTMLElement;
const ssidEl = document.getElementById('ssid') as HTMLElement;
const subEl = document.getElementById('sub') as HTMLElement;
const pill = document.getElementById('pill') as HTMLElement;

let settings: Settings | null = null;
let status: Status | null = null;
let lastW = 0;
let lastH = 0;

function render(): void {
  if (!settings) return;
  const s = settings;
  applyTheme(s.Theme);
  root.style.fontSize = `${14 * s.OverlayScale}px`;
  pill.style.opacity = String(s.OverlayOpacity);
  barsEl.style.display = s.OverlayShowSignal ? '' : 'none';

  const st = status;
  let ssid = 'WiFi 检测中…';
  let sub = '';
  let signal = 0;
  if (st) {
    if (st.connecting && st.target) {
      ssid = `正在切换到 ${st.target}…`;
      sub = st.connected ? `当前 ${st.ssid ?? ''}` : '正在连接';
    } else if (st.locationDenied) {
      ssid = '需要开启位置服务';
      sub = 'Windows 需授权才能读取 WiFi';
    } else if (st.connected) {
      ssid = st.ssid ?? '';
      const band = st.curBand > 0 ? ` · ${bandName(st.curBand)}` : '';
      sub = (s.OverlayShowSignal ? `信号 ${st.signal}%` : '已连接') + band + (st.paused ? ' · 已暂停' : '');
      signal = st.signal;
    } else {
      ssid = '未连接';
      sub = st.paused ? '已暂停自动切换' : '正在寻找网络…';
    }
  }
  ssidEl.textContent = ssid;
  subEl.textContent = sub;
  barsEl.className = `bars lv${level(signal)}`;
  fit();
}

/** 量好内容的逻辑像素尺寸，让窗口贴合内容。 */
function fit(): void {
  const r = root.getBoundingClientRect();
  const w = Math.ceil(r.width);
  const h = Math.ceil(r.height);
  if (w === lastW && h === lastH) return;
  lastW = w;
  lastH = h;
  void api.overlayFit(w, h);
}

async function init(): Promise<void> {
  [settings, status] = await Promise.all([api.getSettings(), api.getStatus()]);
  render();
  new ResizeObserver(() => fit()).observe(root);
  await listen<Status>('status', (e) => {
    status = e.payload;
    render();
  });
  await listen<Settings>('settings', (e) => {
    settings = e.payload;
    render();
  });
}

void init();
document.addEventListener('contextmenu', (e) => e.preventDefault());
