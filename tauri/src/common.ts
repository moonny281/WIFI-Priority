import { invoke } from '@tauri-apps/api/core';

// ---------- 与 Rust 端对应的类型 ----------
// Settings 的字段名与 WPF 版的 settings.json 完全一致（PascalCase）
export interface Settings {
  Priorities: string[];
  ScanIntervalSec: number;
  UpThreshold: number;
  MinConnectSignal: number;
  StaleFilter: boolean;
  DownThreshold: number;
  ConfirmMs: number;
  CooldownSec: number;
  ManualPauseMinutes: number;
  TakeoverUnlisted: boolean;
  InternetCheck: boolean;
  Notify: boolean;
  BandPolicy: 'Off' | 'Same' | 'Global';
  PreferBand: '5G' | '2.4G';
  Theme: 'System' | 'Light' | 'Dark';
  SchemaVersion: number;
  OverlayEnabled: boolean;
  OverlayLocked: boolean;
  OverlayShowSignal: boolean;
  OverlayOpacity: number;
  OverlayScale: number;
  OverlayAnchor: string;
  OverlayX: number;
  OverlayY: number;
  OverlayLayer: 'Top' | 'Desktop';
}

export interface Bss {
  bssid: string;
  signal: number;
  band: number;
  chan: number;
  rssi: number;
  iface: number;
}

export interface Nearby {
  ssid: string;
  signal: number;
  bss: Bss[];
  bands: string;
  bandTags: string;
  profile: string | null;
}

export interface Status {
  connected: boolean;
  ssid: string | null;
  signal: number;
  curBand: number;
  curBssid: string | null;
  locationDenied: boolean;
  paused: boolean;
  pausedUntil: number | null;
  connecting: boolean;
  target: string | null;
  message: string | null;
  native: boolean;
  nearby: Nearby[];
  profiles: string[];
}

export interface Info {
  version: string;
  mica: boolean;
  native: boolean;
  lastSwitch: string;
}

export const api = {
  getInfo: () => invoke<Info>('get_info'),
  getSettings: () => invoke<Settings>('get_settings'),
  updateSettings: (patch: Partial<Settings>) => invoke<Settings>('update_settings', { patch }),
  getStatus: () => invoke<Status | null>('get_status'),
  getLogs: () => invoke<string[]>('get_logs'),
  clearLogs: () => invoke<void>('clear_logs'),
  scanNow: () => invoke<void>('scan_now'),
  setPaused: (paused: boolean) => invoke<void>('set_paused', { paused }),
  addProfile: (ssid: string, password: string) => invoke<void>('add_profile', { ssid, password }),
  autostartGet: () => invoke<boolean>('autostart_get'),
  autostartSet: (enable: boolean) => invoke<{ ok: boolean; enabled: boolean }>('autostart_set', { enable }),
  openLocationSettings: () => invoke<void>('open_location_settings'),
  overlayFit: (w: number, h: number) => invoke<void>('overlay_fit', { w, h }),
  exitApp: () => invoke<void>('exit_app'),
};

// ---------- 小工具 ----------
export function bandName(band: number): string {
  return band === 6 ? '6G' : band === 5 ? '5G' : band === 2 ? '2.4G' : '';
}

/** 信号强度（0-100）→ 四格图标的档位 0-4 */
export function level(signal: number): number {
  return signal >= 75 ? 4 : signal >= 55 ? 3 : signal >= 35 ? 2 : signal > 0 ? 1 : 0;
}

export function barsHtml(signal: number): string {
  return `<span class="bars lv${level(signal)}"><i></i><i></i><i></i><i></i></span>`;
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] as string);
}

// ---------- 主题：云母白 / 墨夜 / 跟随系统 ----------
let mq: MediaQueryList | null = null;
let currentMode = 'System';

export function applyTheme(mode: string): void {
  currentMode = mode;
  if (!mq) {
    mq = window.matchMedia('(prefers-color-scheme: dark)');
    mq.addEventListener('change', () => applyTheme(currentMode));
  }
  const dark = mode === 'Dark' || (mode !== 'Light' && mq.matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
}
