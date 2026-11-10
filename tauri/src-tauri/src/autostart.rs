//! 开机自启：用“任务计划程序”创建登录触发、最高权限的任务（登录后自动以管理员权限启动且不弹 UAC，
//! 也不受“电池供电不启动”等默认限制）。创建/删除任务需要管理员权限，会弹一次 UAC。
//! 任务名与 WPF 版不同，两个版本可以分别管理；请只开其中一个，避免两个程序同时切换。

use crate::util::hide_window;
use std::process::{Command, Stdio};

const TASK: &str = "WifiPriorityTauri";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

pub fn is_enabled() -> bool {
    let mut cmd = Command::new("schtasks.exe");
    cmd.args(["/Query", "/TN", TASK]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn build_xml() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let (exe, dir, user) = (xml_escape(&exe.display().to_string()), xml_escape(&dir), xml_escape(&user));
    Some(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>WiFi Priority (Tauri) auto start</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId><Delay>PT10S</Delay></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{exe}</Command><Arguments>--background</Arguments><WorkingDirectory>{dir}</WorkingDirectory></Exec>
  </Actions>
</Task>"#
    ))
}

/// 以管理员身份运行 schtasks（弹 UAC）；用户取消 UAC 时返回 false。
fn run_elevated(args: &str) -> bool {
    let script = "$p = Start-Process -FilePath schtasks.exe -ArgumentList $env:WP_ARGS -Verb RunAs -Wait -PassThru; exit $p.ExitCode";
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
        .env("WP_ARGS", args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

pub fn enable() -> bool {
    let xml = match build_xml() {
        Some(x) => x,
        None => return false,
    };
    // schtasks /XML 需要 UTF-16 LE（带 BOM）
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let tmp = std::env::temp_dir().join("wifipriority_tauri_task.xml");
    if std::fs::write(&tmp, bytes).is_err() {
        return false;
    }
    let ok = run_elevated(&format!("/Create /TN \"{}\" /XML \"{}\" /F", TASK, tmp.display()));
    let _ = std::fs::remove_file(&tmp);
    ok
}

pub fn disable() -> bool {
    run_elevated(&format!("/Delete /TN \"{}\" /F", TASK))
}
