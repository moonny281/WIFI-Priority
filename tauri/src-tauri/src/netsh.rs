//! netsh 兼容层：原生 WLAN 接口不可用或读取校验失败时的后备方案；另外负责保存新网络的配置文件和联网检测。

use crate::model::WifiState;
use crate::parse;
use crate::util::hide_window;
use std::process::{Command, Stdio};

fn run_netsh(args: &str) -> String {
    // 先把代码页切到 UTF-8，这样中文 SSID 不会乱码
    let line = format!("chcp 65001 >nul & netsh {}", args);
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/c", &line]).stdin(Stdio::null());
    hide_window(&mut cmd);
    match cmd.output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// 并行跑三条 netsh 命令并解析。
pub fn read_state() -> WifiState {
    let (a, b, c) = std::thread::scope(|s| {
        let h1 = s.spawn(|| run_netsh("wlan show interfaces"));
        let h2 = s.spawn(|| run_netsh("wlan show networks mode=bssid"));
        let h3 = s.spawn(|| run_netsh("wlan show profiles"));
        (
            h1.join().unwrap_or_default(),
            h2.join().unwrap_or_default(),
            h3.join().unwrap_or_default(),
        )
    });
    parse::parse_state(&a, &b, &c)
}

/// 连接到已保存的配置文件（netsh 不能指定 BSSID）。
pub fn connect(profile: &str, ssid: &str) -> bool {
    let mut cmd = Command::new("netsh.exe");
    cmd.args(["wlan", "connect"])
        .arg(format!("name={}", profile))
        .arg(format!("ssid={}", ssid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 为 WPA2-个人 网络生成并导入配置文件（密码只在临时文件里短暂存在）。
pub fn add_profile(ssid: &str, password: &str) -> bool {
    let s = xml_escape(ssid);
    let k = xml_escape(password);
    let xml = format!(
        r#"<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1">
  <name>{s}</name>
  <SSIDConfig><SSID><name>{s}</name></SSID></SSIDConfig>
  <connectionType>ESS</connectionType>
  <connectionMode>auto</connectionMode>
  <MSM><security>
    <authEncryption><authentication>WPA2PSK</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption>
    <sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{k}</keyMaterial></sharedKey>
  </security></MSM>
</WLANProfile>"#
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("wp_{}.xml", nanos));
    if std::fs::write(&tmp, xml).is_err() {
        return false;
    }
    let mut cmd = Command::new("netsh.exe");
    cmd.args(["wlan", "add", "profile"])
        .arg(format!("filename={}", tmp.display()))
        .arg("user=all")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut cmd);
    let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
    let _ = std::fs::remove_file(&tmp);
    ok
}

/// 联网检测：ping 两个公共 DNS，任一通即认为能上网。
pub fn has_internet() -> bool {
    for host in ["223.5.5.5", "1.1.1.1"] {
        let mut cmd = Command::new("ping.exe");
        cmd.args(["-n", "1", "-w", "1500", host])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut cmd);
        if cmd.status().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}
