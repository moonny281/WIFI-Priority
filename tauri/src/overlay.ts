import './overlay.css';
import { listen } from '@tauri-apps/api/event';
import { api, applyTheme, bandName, fmtBytes, fmtRate, level, type Settings, type Status, type Traffic } from './common';

const root = document.getElementById('root') as HTMLElement;
const barsEl = document.getElementById('bars') as HTMLElement;
const ssidEl = document.getElementById('ssid') as HTMLElement;
const subEl = document.getElementById('sub') as HTMLElement;
const pill = document.getElementById('pill') as HTMLElement;
const trafEl = document.getElementById('traf') as HTMLElement;
const traf2El = document.getElementById('traf2') as HTMLElement;

let settings: Settings | null = null;
let status: Status | null = null;
let traffic: Traffic | null = null;
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
  renderTraffic();
  fit();
}

/**
 * 实时流量：第一行是实时速度（↓下行 ↑上行）；第二行按“连接 / 网络”区分用量：
 * 本次 = 这一次连接用了多少，今日 / 累计 = 该网络（SSID）今天 / 累计用了多少。
 */
function renderTraffic(): void {
  const t = traffic;
  const on = !!settings?.OverlayShowTraffic && !!status?.connected && !!t && t.connected && t.ssid === status.ssid;
  trafEl.style.display = on ? '' : 'none';
  traf2El.style.display = on ? '' : 'none';
  if (!on || !t) return;
  trafEl.innerHTML = `<span class="dn">↓ ${fmtRate(t.rxBps)}</span><span class="up">↑ ${fmtRate(t.txBps)}</span>`;
  traf2El.textContent = `本次 ${fmtBytes(t.sessRx + t.sessTx)} · 今日 ${fmtBytes(t.todayRx + t.todayTx)} · 累计 ${fmtBytes(t.totalRx + t.totalTx)}`;
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
  [settings, status, traffic] = await Promise.all([api.getSettings(), api.getStatus(), api.getTraffic()]);
  render();
  new ResizeObserver(() => fit()).observe(root);
  await listen<Status>('status', (e) => {
    status = e.payload;
    render();
  });
  await listen<Traffic>('traffic', (e) => {
    traffic = e.payload;
    renderTraffic();
    fit();
  });
  await listen<Settings>('settings', (e) => {
    settings = e.payload;
    render();
  });
}

void init();
document.addEventListener('contextmenu', (e) => e.preventDefault());
