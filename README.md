# WiFi 优先级（WifiPriority）

Windows 小工具：按你排好的优先级自动切换 WiFi，并且**同一个 WiFi 有 5G / 2.4G 时优先 5G**。

- 周围出现**更高优先级**的 WiFi（信号够强）→ 自动切过去
- 当前 WiFi **信号消失/过弱** → 自动回落到次优先级（或同一网络的另一个频段）
- **5G 优先于 2.4G**（可改成 2.4G 优先，或不区分频段）
- 桌面**悬浮框**显示当前 WiFi 与频段（可拖动、吸附四角、调透明度/大小、锁定穿透、随时开关）
- 托盘常驻、开机自启（任务计划 + 最高权限，登录后无需再点 UAC）
- 界面：云母白（Windows 11 Mica）/ 墨夜（深色）/ 跟随系统

## 仓库结构（多应用）

```
VERSION                    ← 版本号唯一来源（如 1.1.0），流水线和 WPF 项目都读它
wpf/                       ← 当前的 WPF 版（C# / .NET 8）
tauri/                     ← 以后放 Tauri 重构版（目录里有 src-tauri/tauri.conf.json 就会被流水线自动识别）
.github/workflows/release.yml
```

## 发布流程（全自动）

1. 改 `VERSION`（例如 `1.1.0` → `1.2.0`），提交并推送到 `main`。
2. 流水线自动：读版本号 → 构建 WPF 版（和已存在的 Tauri 版）→ 生成 `SHA256SUMS.txt` → 创建 GitHub Release `v1.2.0` 并上传所有产物，发布说明自动生成。
3. 不改版本号直接推送：只构建，不发布（产物在该次运行的 Artifacts 里下载）。`v1.2.0` 已经存在时同理。
4. 其它触发方式：推送 `v*` 标签（以标签版本号为准）；Actions 页手动运行（可选择是否发布）；Pull Request 只构建。
5. 版本号带 `-` 的（如 `1.2.0-beta.1`）会自动标记为预发布。

发布产物命名：

| 文件 | 说明 |
|---|---|
| `WifiPriority-wpf-v<版本>-win-x64.exe` | WPF 版，单文件、自包含，无需安装 .NET |
| `WifiPriority-tauri-v<版本>-x64-setup.exe` / `.msi` / `-portable.exe` | Tauri 版（加了 `tauri/` 之后才会出现） |
| `SHA256SUMS.txt` | 所有文件的校验和 |

未做代码签名，首次运行 SmartScreen 可能提示“未知发布者”，点“更多信息 → 仍要运行”即可。

### 以后加入 Tauri 版

把 Tauri 项目放进 `tauri/`（即 `tauri/package.json` 和 `tauri/src-tauri/tauri.conf.json`），不用改流水线。流水线会：

- 自动用统一的版本号覆盖 `tauri.conf.json` 的 version
- 按锁文件识别 npm / pnpm / yarn 并安装依赖，然后执行 `npx tauri build`（需要 `@tauri-apps/cli` 在 devDependencies 里）
- 收集 NSIS 安装包、MSI 和免安装 exe，与 WPF 版一起发布

### 本地编译 WPF 版

安装 .NET 8 SDK 后：

```
dotnet publish wpf/WifiPriority.csproj -c Release -r win-x64 --self-contained true -p:PublishSingleFile=true -o out
```

## 使用

1. 首次运行：到 **优先级** 页添加网络。已经在 Windows 里连过的网络直接填名称（或在 **附近网络** 页一键加入）；没连过的填名称 + 密码（仅支持 WPA2 个人版，WPA3/企业网络请先在 Windows 里手动连一次）。
2. 拖动条目（或点 ▲▼）排序，越靠上优先级越高。
3. **设置 → 频段偏好** 选择 5G/2.4G 的策略（见下）。
4. **设置 → 开机自启**：开启时会弹一次管理员授权（UAC），之后每次登录自动在后台运行。
5. 关闭主窗口只会最小化到托盘；托盘右键可暂停/恢复、显示/隐藏悬浮框、退出。

## 5G 优先怎么工作

程序会读取每个接入点（BSSID）的频段和信号，把“列表 × 频段”展开成候选项再排序。**频段策略**有三档：

| 策略 | 规则 |
|---|---|
| 全局偏好频段优先（默认） | 只要有可用的 5G 网络，就优先于所有 2.4G 网络，其次才按列表顺序。适合 2.4G/5G 用**不同名字**的路由器（如 `Home` 和 `Home_5G`） |
| 仅同一网络内 | 先按列表顺序；同一个 WiFi 同时有 2.4G 和 5G 时连 5G。你的列表顺序永远优先 |
| 不区分频段 | 只看列表顺序，同名双频由 Windows 自己选 |

> 默认是“全局”：它会让列表里靠后的 5G 网络排到靠前的 2.4G 网络前面。如果你希望列表顺序绝对优先，请改成“仅同一网络内”。

- 6G（Wi‑Fi 6E）排在 5G 之前；优先频段可改为“2.4G 优先”。
- 防抖：切到 5G 要求信号 ≥ 升级阈值（默认 60%）；5G 低于回落阈值（默认 40%）才退回 2.4G，中间 40–60% 不动，避免来回跳。
- 同一个 WiFi 内换频段是通过“指定接入点（BSSID）重新连接”实现的。如果路由器或网卡驱动不接受，程序会在日志里提示，并在 10 分钟内不再尝试该频段，不会反复断线重连。

## 切换策略（均可在设置里调整）

| 项 | 默认 | 说明 |
|---|---|---|
| 扫描间隔 | 10 秒 | 另外在系统唤醒、网络变化时会立即扫描 |
| 升级阈值 | ≥ 60%（≈ −70 dBm） | 更优候选信号达到才会切过去 |
| 回落阈值 | < 40%（≈ −80 dBm） | 当前网络低于此值或消失，视为丢失；此时回落目标只要求 ≥ 此值 |
| 连续确认 | 2 次 | 防止偶尔一次扫描抖动造成误切 |
| 冷却时间 | 60 秒 | 升级切换之间的最短间隔，防止来回跳 |
| 手动换网后暂停 | 10 分钟 | 你在 Windows 里手动换网时，不和你对着干 |

## 注意事项

- **位置服务**：新版 Windows 11 要求开启“设置 → 隐私和安全性 → 位置”，程序才能读取周围 WiFi。没开时概览页会提示并提供一键跳转按钮。
- 程序只能连接**已保存配置文件**的网络，且默认假设配置文件名 = WiFi 名称（Windows 默认如此）。
- 权限：程序平时以普通权限运行；开机自启由任务计划以“最高权限”启动，因此不用每次 UAC。如果移动了 exe 位置，请在设置里关闭再重新开启开机自启。
- 配置保存在 `%AppData%\WifiPriority\settings.json`。

## WPF 项目结构

```
wpf/WifiPriority.csproj      项目文件（net8.0-windows，WPF + WinForms 托盘，单文件发布；版本读自 ../VERSION）
wpf/App.xaml(.cs)            入口：单实例、托盘、系统事件、引擎启动
wpf/MainWindow.xaml(.cs)     主窗口：概览 / 优先级 / 附近网络 / 悬浮框 / 设置 / 日志
wpf/OverlayWindow.xaml(.cs)  桌面悬浮框
wpf/TrayService.cs           托盘图标（运行时绘制）与菜单
wpf/Services/SwitchEngine.cs 核心切换逻辑（候选排序、防抖、冷却、回落、手动暂停、频段验证）
wpf/Services/Wifi.cs         netsh 解析（SSID → BSSID → 频段/信号）与连接
wpf/Services/Native.cs       DWM 云母、悬浮框样式、WlanScan 主动扫描、按 BSSID 连接
wpf/Services/AutoStart.cs    任务计划开机自启
wpf/Services/ThemeService.cs 浅色/深色主题与 Mica
wpf/Themes/*.xaml            两套配色 + 自绘控件样式
```
