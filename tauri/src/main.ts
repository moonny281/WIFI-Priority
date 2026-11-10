import './styles.css';
import logoUrl from './assets/logo.png';
import { listen } from '@tauri-apps/api/event';
import { api, applyTheme, barsHtml, bandName, level, type Info, type Nearby, type Settings, type Status } from './common';

// ====================== 页面骨架 ======================
const TEMPLATE = `
<div class="app">
  <aside class="nav">
    <div class="brand"><img src="${logoUrl}" alt="" /><span>WiFi 优先级</span></div>
    <div class="subtitle">自动切换到最优网络</div>
    <button class="nav-item active" data-page="overview">概览</button>
    <button class="nav-item" data-page="priority">优先级</button>
    <button class="nav-item" data-page="nearby">附近网络</button>
    <button class="nav-item" data-page="overlay">桌面悬浮框</button>
    <button class="nav-item" data-page="settings">设置</button>
    <button class="nav-item" data-page="log">日志</button>
    <div class="nav-foot"><div class="ver" id="version"></div><button id="btn-exit">退出程序</button></div>
  </aside>

  <main class="pages">
    <!-- 概览 -->
    <section class="page active" id="page-overview">
      <h1>概览</h1>
      <div class="card row-card">
        <div class="grow">
          <div class="sub">当前连接</div>
          <div class="big" id="cur-ssid">检测中…</div>
          <div class="sub" id="cur-detail"></div>
          <button class="mt12 hidden" id="btn-location">打开“位置服务”设置</button>
        </div>
        <div id="cur-bars"></div>
      </div>
      <div class="card">
        <h2>自动切换</h2>
        <div class="sub mt8" id="state-text"></div>
        <div class="btns mt12">
          <button class="accent" id="btn-pause">暂停自动切换</button>
          <button id="btn-scan">立即扫描</button>
        </div>
      </div>
      <div class="card">
        <h2>最近一次切换</h2>
        <div class="sub mt8" id="last-switch">尚无切换记录</div>
      </div>
    </section>

    <!-- 优先级 -->
    <section class="page" id="page-priority">
      <h1 style="margin-bottom:4px">优先级</h1>
      <div class="sub" style="margin-bottom:14px">越靠上优先级越高：可拖动条目排序，也可点 ▲ ▼。网络名称需与 Windows 里保存的配置文件名一致。</div>
      <div class="card">
        <div class="form-grid">
          <div><label class="cap">网络名称 (SSID)</label><input type="text" id="new-ssid" spellcheck="false" /></div>
          <div><label class="cap">密码（仅该网络尚未保存时填写）</label><input type="password" id="new-pwd" /></div>
          <button class="accent" id="btn-add" style="padding:8px 16px">添加到列表</button>
        </div>
        <div class="sub hint" id="add-hint"></div>
      </div>
      <div class="list" id="pri-list"></div>
      <div class="sub empty" id="pri-empty">还没有网络，请在上方添加，或到“附近网络”里一键加入。</div>
    </section>

    <!-- 附近网络 -->
    <section class="page" id="page-nearby">
      <h1 style="margin-bottom:4px">附近网络</h1>
      <div class="sub" id="near-count" style="margin-bottom:14px"></div>
      <div class="list" id="near-list"></div>
    </section>

    <!-- 桌面悬浮框 -->
    <section class="page" id="page-overlay">
      <h1 style="margin-bottom:4px">桌面悬浮框</h1>
      <div class="sub" style="margin-bottom:14px">在桌面上显示当前连接的 WiFi。直接用鼠标拖动即可移动位置；托盘图标的右键菜单里也能开关、锁定、吸附。</div>
      <div class="card">
        <label class="switch"><input type="checkbox" data-setting="OverlayEnabled" /><span>显示桌面悬浮框</span></label>
        <label class="switch"><input type="checkbox" data-setting="OverlayShowSignal" /><span>显示信号强度</span></label>
        <label class="switch"><input type="checkbox" data-setting="OverlayLocked" /><span>锁定位置并鼠标穿透（悬浮框不再响应点击，需在此处或托盘菜单解锁）</span></label>
      </div>
      <div class="card">
        <div class="slider-head"><span>透明度</span><span class="sub" id="ov-opacity-text"></span></div>
        <input type="range" min="0.3" max="1" step="0.01" data-setting="OverlayOpacity" />
        <div class="slider-head mt16"><span>大小</span><span class="sub" id="ov-scale-text"></span></div>
        <input type="range" min="0.8" max="1.6" step="0.01" data-setting="OverlayScale" />
      </div>
      <div class="card">
        <h2>位置</h2>
        <div class="sub mt8" style="margin-bottom:12px">拖动悬浮框可自由摆放；也可以一键吸附到屏幕角落。</div>
        <div class="btns">
          <button data-anchor="TopLeft">左上角</button><button data-anchor="TopRight">右上角</button>
          <button data-anchor="BottomLeft">左下角</button><button data-anchor="BottomRight">右下角</button>
          <button id="btn-ov-reset">恢复默认外观</button>
        </div>
      </div>
    </section>

    <!-- 设置 -->
    <section class="page" id="page-settings">
      <h1>设置</h1>
      <div class="card">
        <h2>启动与外观</h2>
        <label class="switch mt12"><input type="checkbox" id="sw-autostart" /><span>开机自启（任务计划，最高权限，登录后后台常驻）</span></label>
        <div class="sub" id="autostart-hint" style="margin:4px 0 0 52px">首次开启需要确认一次管理员授权（UAC）。若移动了 exe 的位置，请关闭后重新开启。</div>
        <div class="mt16" style="margin-bottom:8px">主题</div>
        <div class="seg" data-setting="Theme">
          <button data-value="System">跟随系统</button><button data-value="Light">云母白</button><button data-value="Dark">墨夜</button>
        </div>
      </div>

      <div class="card">
        <h2>频段偏好（5G / 2.4G）</h2>
        <div class="sub mt8">同一个 WiFi 常同时开放 2.4G 和 5G：5G 更快但穿墙差。这里决定遇到多个频段时怎么选。</div>
        <div class="mt14" style="margin-bottom:8px">频段策略</div>
        <div class="seg" data-setting="BandPolicy">
          <button data-value="Global">全局偏好频段优先</button><button data-value="Same">仅同一网络内</button><button data-value="Off">不区分频段</button>
        </div>
        <div class="mt14" style="margin-bottom:8px">优先频段</div>
        <div class="seg" data-setting="PreferBand">
          <button data-value="5G">5G 优先（6G &gt; 5G &gt; 2.4G）</button><button data-value="2.4G">2.4G 优先</button>
        </div>
        <div class="sub hint" id="band-hint"></div>
      </div>

      <div class="card">
        <h2>切换策略</h2>
        <div class="sub mt8" style="margin-bottom:10px">信号强度是 Windows 报告的百分比（约等于 2 × (dBm + 100)，60% ≈ −70 dBm，40% ≈ −80 dBm）。</div>
        <div class="setting"><span>主动扫描间隔（秒，0 = 连续扫描，最快）</span><input type="number" min="0" max="600" data-setting="ScanIntervalSec" /></div>
        <div class="setting"><span>切换到更高优先级所需的信号 ≥ (%)</span><input type="number" min="10" max="100" data-setting="UpThreshold" /></div>
        <div class="setting"><span>当前网络信号低于 (%) 视为过弱/消失，回落到次优先级</span><input type="number" min="0" max="95" data-setting="DownThreshold" /></div>
        <div class="setting"><span>确认时间（毫秒，候选保持最优这么久才切换，0 = 立即）</span><input type="number" min="0" max="10000" data-setting="ConfirmMs" /></div>
        <div class="setting"><span>两次“升级”切换的最短间隔（秒；信号丢失时的回落不受限制）</span><input type="number" min="0" max="3600" data-setting="CooldownSec" /></div>
        <div class="setting"><span>检测到手动换网后暂停自动切换（分钟，0 = 不暂停）</span><input type="number" min="0" max="1440" data-setting="ManualPauseMinutes" /></div>
        <label class="switch mt16"><input type="checkbox" data-setting="TakeoverUnlisted" /><span>当前连接的网络不在优先级列表里时，也自动切到列表中的网络</span></label>
        <label class="switch"><input type="checkbox" data-setting="InternetCheck" /><span>联网检测：连上但无法上网时，视为不可用并回落</span></label>
        <label class="switch"><input type="checkbox" data-setting="Notify" /><span>切换时弹出系统通知</span></label>
      </div>
    </section>

    <!-- 日志 -->
    <section class="page flex" id="page-log">
      <div class="page-head"><h1>日志</h1><button id="btn-clear-log">清空</button></div>
      <div class="card log" id="log-box"></div>
    </section>
  </main>
</div>`;

// ====================== 状态 ======================
const $ = <T extends HTMLElement>(sel: string): T => document.querySelector(sel) as T;

let settings: Settings;
let info: Info;
let status: Status | null = null;
let lastSwitch = '尚无切换记录';
let dragging = false;

// ====================== 列表的“原地更新” ======================
// 不用整块重绘：重绘会让正在按下的按钮失效（点击丢失）、拖拽被打断。
function syncList<T>(
  container: HTMLElement,
  items: T[],
  key: (t: T) => string,
  create: (t: T) => HTMLElement,
  update: (el: HTMLElement, t: T) => void,
): void {
  const existing = new Map<string, HTMLElement>();
  Array.from(container.children).forEach((c) => existing.set((c as HTMLElement).dataset.key ?? '', c as HTMLElement));
  let prev: Element | null = null;
  for (const it of items) {
    const k = key(it);
    let el = existing.get(k);
    if (!el) {
      el = create(it);
      el.dataset.key = k;
    } else {
      existing.delete(k);
    }
    update(el, it);
    const next: Element | null = prev ? prev.nextElementSibling : container.firstElementChild;
    if (el !== next) container.insertBefore(el, next);
    prev = el;
  }
  existing.forEach((el) => el.remove());
}

function setText(el: Element | null, text: string): void {
  if (el && el.textContent !== text) el.textContent = text;
}

function setBars(el: Element | null, signal: number): void {
  if (!el) return;
  const html = barsHtml(signal);
  if (el.innerHTML !== html) el.innerHTML = html;
}

// ====================== 渲染 ======================
function hasProfile(ssid: string): boolean {
  if (!status) return false;
  return status.profiles.includes(ssid) || status.nearby.some((n) => n.ssid === ssid && !!n.profile);
}

function renderOverview(): void {
  const st = status;
  const ssidEl = $('#cur-ssid');
  if (!st) {
    setText(ssidEl, '检测中…');
    return;
  }
  setText(ssidEl, st.locationDenied ? '需要开启位置服务' : st.connected ? (st.ssid ?? '') : '未连接');
  const band = st.curBand > 0 ? ` · ${bandName(st.curBand)} 频段` : '';
  setText(
    $('#cur-detail'),
    st.locationDenied
      ? 'Windows 需要你授权“位置服务”才允许程序读取周围的 WiFi。'
      : st.connected
        ? `信号强度 ${st.signal}%${band}`
        : (st.message ?? '当前没有连接到任何 WiFi'),
  );
  $('#btn-location').classList.toggle('hidden', !st.locationDenied);
  const bars = $('#cur-bars');
  const html = `<span class="bars xl lv${st.connected && !st.locationDenied ? level(st.signal) : 0}"><i></i><i></i><i></i><i></i></span>`;
  if (bars.innerHTML !== html) bars.innerHTML = html;

  const scan = settings.ScanIntervalSec <= 0 ? '连续扫描' : `每 ${settings.ScanIntervalSec} 秒扫描一次`;
  const mode = st.native ? '原生 WLAN 事件驱动' : 'netsh 兼容模式（较慢）';
  let text: string;
  if (st.locationDenied) text = '位置服务未开启，暂时无法扫描周围的 WiFi。';
  else if (st.paused) {
    text = st.pausedUntil
      ? `自动切换已暂停，将在 ${new Date(st.pausedUntil * 1000).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })} 自动恢复。`
      : '自动切换已暂停（手动）。';
  } else {
    text = `自动切换运行中（${mode}，${scan}）。` + (settings.Priorities.length === 0 ? ' 还没有设置优先级列表，请先到“优先级”页添加网络。' : '');
  }
  setText($('#state-text'), text);
  setText($('#btn-pause'), st.paused ? '恢复自动切换' : '暂停自动切换');
  setText($('#last-switch'), lastSwitch);
}

function priorityStatus(ssid: string): { text: string; signal: number } {
  const st = status;
  if (!st) return { text: '', signal: 0 };
  const n = st.nearby.find((x) => x.ssid === ssid);
  const isCur = st.connected && st.ssid === ssid;
  let text = isCur
    ? '当前连接' + (st.curBand > 0 ? ' · ' + bandName(st.curBand) : '')
    : n
      ? '可见' + (n.bandTags ? ' · ' + n.bandTags : '')
      : '不可见';
  if (!isCur && !hasProfile(ssid)) text += ' · 未保存配置';
  return { text, signal: n ? n.signal : 0 };
}

function renderPriority(): void {
  if (dragging) return;
  const list = $('#pri-list');
  syncList(
    list,
    settings.Priorities,
    (s) => s,
    (s) => {
      const el = document.createElement('div');
      el.className = 'row';
      el.draggable = true;
      el.innerHTML = `<span class="rank"></span><span class="name"></span><span class="meta st"></span>
        <span class="bs"></span>
        <span><button class="ghost" data-act="up">▲</button><button class="ghost" data-act="down">▼</button><button class="ghost" data-act="del">✕</button></span>`;
      (el.querySelector('.name') as HTMLElement).textContent = s;
      return el;
    },
    (el, s) => {
      const i = settings.Priorities.indexOf(s);
      const p = priorityStatus(s);
      setText(el.querySelector('.rank'), String(i + 1));
      setText(el.querySelector('.st'), p.text);
      setBars(el.querySelector('.bs'), p.signal);
    },
  );
  $('#pri-empty').classList.toggle('hidden', settings.Priorities.length > 0);
}

function renderNearby(): void {
  const list = $('#near-list');
  const items: Nearby[] = status ? status.nearby : [];
  syncList(
    list,
    items,
    (n) => n.ssid,
    (n) => {
      const el = document.createElement('div');
      el.className = 'row';
      el.innerHTML = `<span class="name"></span><span class="meta bands"></span><span class="meta saved"></span>
        <span class="bs"></span><span class="pct"></span><button data-act="join"></button>`;
      (el.querySelector('.name') as HTMLElement).textContent = n.ssid;
      (el.querySelector('button') as HTMLElement).dataset.ssid = n.ssid;
      return el;
    },
    (el, n) => {
      const st = status as Status;
      const inList = settings.Priorities.includes(n.ssid);
      setText(el.querySelector('.bands'), n.bands);
      setText(el.querySelector('.saved'), st.connected && st.ssid === n.ssid ? '已连接' : hasProfile(n.ssid) ? '已保存' : '未保存');
      setBars(el.querySelector('.bs'), n.signal);
      setText(el.querySelector('.pct'), `${n.signal}%`);
      const btn = el.querySelector('button') as HTMLButtonElement;
      setText(btn, inList ? '已在列表' : '加入优先级');
      btn.disabled = inList;
    },
  );
  setText($('#near-count'), `共发现 ${items.length} 个网络（实时刷新）`);
}

function bandHint(): string {
  const pref = settings.PreferBand === '2.4G' ? '2.4G' : '5G（含 6G）';
  const other = settings.PreferBand === '2.4G' ? '其它频段' : '2.4G ';
  if (settings.BandPolicy === 'Off') return '不区分频段：只按优先级列表顺序，同名的双频网络由 Windows 自己选频段。';
  if (settings.BandPolicy === 'Same') return `仅同一网络内：先按列表顺序；同一个 WiFi 同时有多个频段时，优先连 ${pref}。`;
  return `全局：只要有可用的 ${pref} 网络，就优先于所有${other}网络，再按列表顺序。例如列表里靠后的 5G 网络会排在靠前的 2.4G 网络前面。`;
}

/** 把 settings 同步到所有带 data-setting 的控件上。 */
function syncSettingsUi(): void {
  document.querySelectorAll<HTMLElement>('[data-setting]').forEach((el) => {
    const key = el.dataset.setting as keyof Settings;
    const val = settings[key];
    if (el instanceof HTMLInputElement) {
      if (el.type === 'checkbox') el.checked = Boolean(val);
      else if (document.activeElement !== el) el.value = String(val);
      if (el.type === 'range') {
        const min = Number(el.min), max = Number(el.max);
        el.style.setProperty('--p', `${((Number(val) - min) / (max - min)) * 100}%`);
      }
    } else if (el.classList.contains('seg')) {
      el.querySelectorAll<HTMLButtonElement>('button').forEach((b) => b.classList.toggle('on', b.dataset.value === String(val)));
    }
  });
  setText($('#ov-opacity-text'), `${Math.round(settings.OverlayOpacity * 100)}%`);
  setText($('#ov-scale-text'), `${Math.round(settings.OverlayScale * 100)}%`);
  setText($('#band-hint'), bandHint());
  applyTheme(settings.Theme);
}

let renderTimer: number | null = null;
function scheduleRender(): void {
  if (renderTimer !== null) return;
  renderTimer = window.setTimeout(() => {
    renderTimer = null;
    renderAll();
  }, 120);
}

function renderAll(): void {
  renderOverview();
  renderPriority();
  renderNearby();
}

async function patch(p: Partial<Settings>): Promise<void> {
  settings = await api.updateSettings(p);
  syncSettingsUi();
  renderAll();
}

// ====================== 事件 ======================
function wire(): void {
  // 导航
  document.querySelectorAll<HTMLButtonElement>('.nav-item').forEach((b) =>
    b.addEventListener('click', () => {
      document.querySelectorAll('.nav-item').forEach((x) => x.classList.toggle('active', x === b));
      document.querySelectorAll('.page').forEach((p) => p.classList.toggle('active', p.id === `page-${b.dataset.page}`));
    }),
  );
  $('#btn-exit').addEventListener('click', () => void api.exitApp());

  // 概览
  $('#btn-location').addEventListener('click', () => void api.openLocationSettings());
  $('#btn-scan').addEventListener('click', () => void api.scanNow());
  $('#btn-pause').addEventListener('click', () => void api.setPaused(!(status?.paused ?? false)));

  // 带 data-setting 的通用控件
  document.addEventListener('change', (e) => {
    const t = e.target;
    if (!(t instanceof HTMLInputElement)) return;
    const key = t.dataset.setting;
    if (!key || t.type === 'range') return;
    const value: unknown = t.type === 'checkbox' ? t.checked : t.type === 'number' ? Number(t.value) : t.value;
    void patch({ [key]: value } as Partial<Settings>);
  });
  let rangeTimer: number | null = null;
  document.addEventListener('input', (e) => {
    const t = e.target;
    if (!(t instanceof HTMLInputElement) || t.type !== 'range' || !t.dataset.setting) return;
    const key = t.dataset.setting;
    const value = Number(t.value);
    t.style.setProperty('--p', `${((value - Number(t.min)) / (Number(t.max) - Number(t.min))) * 100}%`);
    if (rangeTimer !== null) window.clearTimeout(rangeTimer);
    rangeTimer = window.setTimeout(() => void patch({ [key]: value } as Partial<Settings>), 60);
  });
  document.addEventListener('click', (e) => {
    const t = e.target as HTMLElement;
    const seg = t.closest<HTMLElement>('.seg[data-setting]');
    const btn = t.closest<HTMLButtonElement>('.seg button[data-value]');
    if (seg && btn) void patch({ [seg.dataset.setting as string]: btn.dataset.value } as Partial<Settings>);
    const anchor = t.closest<HTMLButtonElement>('button[data-anchor]');
    if (anchor) void patch({ OverlayEnabled: true, OverlayAnchor: anchor.dataset.anchor });
  });
  $('#btn-ov-reset').addEventListener('click', () =>
    void patch({ OverlayOpacity: 0.95, OverlayScale: 1.0, OverlayAnchor: 'TopRight', OverlayLocked: false }),
  );

  // 开机自启
  const sw = $<HTMLInputElement>('#sw-autostart');
  sw.addEventListener('change', async () => {
    const want = sw.checked;
    sw.disabled = true;
    $('#autostart-hint').textContent = '正在请求管理员授权…';
    try {
      const r = await api.autostartSet(want);
      sw.checked = r.enabled;
      $('#autostart-hint').textContent = r.ok
        ? want ? '已创建开机自启任务：登录后自动以最高权限在后台运行。' : '已移除开机自启任务。'
        : '操作未完成（可能取消了管理员授权）。';
    } finally {
      sw.disabled = false;
    }
  });

  // 优先级：添加
  const hint = (t: string) => setText($('#add-hint'), t);
  $('#btn-add').addEventListener('click', async () => {
    const ssid = $<HTMLInputElement>('#new-ssid').value.trim();
    const pwd = $<HTMLInputElement>('#new-pwd').value;
    if (!ssid) return hint('请先输入网络名称。');
    if (settings.Priorities.includes(ssid)) return hint('该网络已在列表中。');
    if (pwd) {
      hint('正在保存网络配置…');
      try {
        await api.addProfile(ssid, pwd);
      } catch (err) {
        return hint(String(err));
      }
    }
    await patch({ Priorities: [...settings.Priorities, ssid] });
    $<HTMLInputElement>('#new-ssid').value = '';
    $<HTMLInputElement>('#new-pwd').value = '';
    hint(pwd ? '已保存并加入优先级列表。' : '已加入优先级列表。');
  });

  // 优先级：上移 / 下移 / 删除
  $('#pri-list').addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-act]');
    const row = (e.target as HTMLElement).closest<HTMLElement>('.row');
    if (!btn || !row) return;
    const ssid = row.dataset.key ?? '';
    const arr = [...settings.Priorities];
    const i = arr.indexOf(ssid);
    if (i < 0) return;
    if (btn.dataset.act === 'up' && i > 0) [arr[i - 1], arr[i]] = [arr[i], arr[i - 1]];
    else if (btn.dataset.act === 'down' && i < arr.length - 1) [arr[i + 1], arr[i]] = [arr[i], arr[i + 1]];
    else if (btn.dataset.act === 'del') arr.splice(i, 1);
    else return;
    void patch({ Priorities: arr });
  });

  // 优先级：拖拽排序
  const pri = $('#pri-list');
  let dragKey = '';
  pri.addEventListener('dragstart', (e) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>('.row');
    if (!row) return;
    dragging = true;
    dragKey = row.dataset.key ?? '';
    row.classList.add('dragging');
    e.dataTransfer?.setData('text/plain', dragKey);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  });
  pri.addEventListener('dragover', (e) => {
    e.preventDefault();
    pri.querySelectorAll('.row.drop').forEach((r) => r.classList.remove('drop'));
    (e.target as HTMLElement).closest<HTMLElement>('.row')?.classList.add('drop');
  });
  pri.addEventListener('drop', (e) => {
    e.preventDefault();
    const target = (e.target as HTMLElement).closest<HTMLElement>('.row');
    const arr = [...settings.Priorities];
    const from = arr.indexOf(dragKey);
    const to = target ? arr.indexOf(target.dataset.key ?? '') : arr.length - 1;
    if (from >= 0 && to >= 0 && from !== to) {
      const [moved] = arr.splice(from, 1);
      arr.splice(to, 0, moved);
      void patch({ Priorities: arr });
    }
  });
  pri.addEventListener('dragend', () => {
    dragging = false;
    pri.querySelectorAll('.row').forEach((r) => r.classList.remove('drop', 'dragging'));
    renderPriority();
  });

  // 附近网络：加入
  $('#near-list').addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-act="join"]');
    const ssid = btn?.dataset.ssid;
    if (!ssid || btn?.disabled) return;
    if (hasProfile(ssid)) {
      void patch({ Priorities: [...settings.Priorities, ssid] });
    } else {
      // 没有保存过密码：跳到“优先级”页，让用户输入密码
      document.querySelector<HTMLButtonElement>('.nav-item[data-page="priority"]')?.click();
      $<HTMLInputElement>('#new-ssid').value = ssid;
      $<HTMLInputElement>('#new-pwd').value = '';
      hint('这个网络还没有保存过配置，请输入密码后点“添加到列表”（仅支持 WPA2 个人版）。');
      $<HTMLInputElement>('#new-pwd').focus();
    }
  });

  // 日志
  $('#btn-clear-log').addEventListener('click', async () => {
    await api.clearLogs();
    $('#log-box').innerHTML = '';
  });
}

function addLog(line: string): void {
  const box = $('#log-box');
  const d = document.createElement('div');
  d.textContent = line;
  box.appendChild(d);
  while (box.children.length > 500) box.firstElementChild?.remove();
  box.scrollTop = box.scrollHeight;
}

// ====================== 启动 ======================
async function init(): Promise<void> {
  $('#app').innerHTML = TEMPLATE;
  [settings, info, status] = await Promise.all([api.getSettings(), api.getInfo(), api.getStatus()]);
  document.documentElement.classList.toggle('mica', info.mica);
  lastSwitch = info.lastSwitch;
  setText($('#version'), `v${info.version}`);
  (await api.getLogs()).forEach(addLog);
  $<HTMLInputElement>('#sw-autostart').checked = await api.autostartGet();

  wire();
  syncSettingsUi();
  renderAll();

  await listen<Status>('status', (e) => {
    status = e.payload;
    scheduleRender();
  });
  await listen<Settings>('settings', (e) => {
    settings = e.payload;
    syncSettingsUi();
    scheduleRender();
  });
  await listen<string>('log', (e) => addLog(e.payload));
  await listen<string>('switched', (e) => {
    lastSwitch = e.payload;
    scheduleRender();
  });
  // 日志页显示时滚到底部
  new MutationObserver(() => {
    const box = $('#log-box');
    box.scrollTop = box.scrollHeight;
  }).observe($('#page-log'), { attributes: true, attributeFilter: ['class'] });
}

void init();

// 屏蔽默认右键菜单（保持原生应用的观感）；输入框里仍可用
document.addEventListener('contextmenu', (e) => {
  if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
});
