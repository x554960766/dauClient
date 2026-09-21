//! 手机 + USB + ADB 飞行模式换 IP 控制器
//! 用于在放量执行波次间隙控制 USB 手机重拨分配新 IP，并校验出口公网 IP

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbPhoneInfo {
    pub serial: String,
    pub model: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateIpResult {
    pub success: bool,
    pub old_ip: String,
    pub new_ip: String,
    pub message: String,
}

/// 查找 adb 可执行文件路径
pub fn resolve_adb_bin(sdk_dir: &Path) -> PathBuf {
    let bundled = sdk_dir.join("platform-tools").join(crate::sdkmgr::adb_bin_name());
    if bundled.exists() {
        return bundled;
    }
    // PATH 兜底
    PathBuf::from(crate::sdkmgr::adb_bin_name())
}

/// 检测当前连接的真实安卓手机列表（过滤掉 emulator-* 模拟器）
pub async fn detect_usb_phones(sdk_dir: &Path) -> Vec<UsbPhoneInfo> {
    let bin = resolve_adb_bin(sdk_dir);
    let mut cmd = Command::new(&bin);
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let output = match cmd.args(["devices", "-l"]).output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("执行 adb devices -l 失败: {}", e);
            return Vec::new();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut phones = Vec::new();

    for line in stdout.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let serial = parts[0].to_string();
        // 过滤掉模拟器
        if serial.starts_with("emulator-") {
            continue;
        }

        let status = parts[1].to_string();

        // 尝试提取 model 字段（如 model:Pixel_5）
        let model = parts
            .iter()
            .find_map(|p| p.strip_prefix("model:"))
            .map(|s| s.replace('_', " "))
            .unwrap_or_else(|| serial.clone());

        phones.push(UsbPhoneInfo {
            serial,
            model,
            status,
        });
    }

    phones
}

/// 确保 Mac 已经连上指定的手机热点，若未连接则强制自动连接（仅在 macOS 上生效，Windows 自动跳过）
#[cfg(target_os = "macos")]
async fn ensure_macos_hotspot_connected(ssid_opt: Option<&str>, pwd_opt: Option<&str>) {
    let route_out = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .await;
    if let Ok(o) = route_out {
        let s = String::from_utf8_lossy(&o.stdout);
        // 如果默认网关已经是常见热点网段 (172.20.10.1)，说明连线正常
        if s.contains("172.20.10.1") {
            return;
        }
    }

    if let Some(ssid) = ssid_opt {
        let trimmed_ssid = ssid.trim();
        if !trimmed_ssid.is_empty() {
            let pwd = pwd_opt.map(|p| p.trim()).unwrap_or("");
            tracing::info!(ssid = %trimmed_ssid, "检测到尚未连接到配置的手机热点，尝试自动强连...");
            let mut cmd = Command::new("networksetup");
            cmd.args(["-setairportnetwork", "en0", trimmed_ssid]);
            if !pwd.is_empty() {
                cmd.arg(pwd);
            }
            let _ = cmd.output().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn ensure_macos_hotspot_connected(_ssid_opt: Option<&str>, _pwd_opt: Option<&str>) {
    // Windows 环境下完全不执行任何 Wi-Fi 强连指令，保持原生 USB RNDIS 行为
}

/// 获取当前电脑出口的公网 IP
pub async fn get_current_public_ip() -> Option<String> {
    let endpoints = [
        "http://cip.cc",
        "https://api.ipify.org",
        "http://ifconfig.me/ip",
        "https://icanhazip.com",
    ];

    // 禁用环境代理 (no_proxy)，确保探测的是电脑真实的物理网关/蜂窝出口 IP
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    for ep in endpoints {
        if let Ok(resp) = client.get(ep).send().await {
            if let Ok(text) = resp.text().await {
                let trimmed = text.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('<') && trimmed.len() < 120 {
                    // cip.cc 格式为 "IP : x.x.x.x"
                    for line in trimmed.lines() {
                        let line_str = line.trim();
                        if line_str.starts_with("IP") && line_str.contains(':') {
                            if let Some(ip_part) = line_str.split(':').nth(1) {
                                let ip = ip_part.trim().to_string();
                                if ip.contains('.') || ip.contains(':') {
                                    return Some(ip);
                                }
                            }
                        } else if !line_str.is_empty() && (line_str.contains('.') || line_str.contains(':')) && line_str.len() < 50 {
                            return Some(line_str.to_string());
                        }
                    }
                }
            }
        }
    }

    // 兜底：使用系统 curl（增加 --noproxy * 避免被本地代理拦截）
    let mut curl_cmd = Command::new("curl");
    #[cfg(target_os = "windows")]
    {
        curl_cmd.creation_flags(0x08000000);
    }
    if let Ok(out) = curl_cmd.args(["-s", "--noproxy", "*", "--max-time", "5", "http://cip.cc"]).output().await {
        let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
        for line in raw.lines() {
            let line_str = line.trim();
            if line_str.starts_with("IP") && line_str.contains(':') {
                if let Some(ip_part) = line_str.split(':').nth(1) {
                    let ip = ip_part.trim().to_string();
                    if ip.contains('.') || ip.contains(':') {
                        return Some(ip);
                    }
                }
            } else if !line_str.is_empty() && (line_str.contains('.') || line_str.contains(':')) && line_str.len() < 50 {
                return Some(line_str.to_string());
            }
        }
    }

    None
}

/// 优先通过 USB 手机直接向公网探测手机自身的蜂窝公网 IP（彻底绕过宿主机代理/VPN），无真机时探测电脑出口
pub async fn get_phone_or_public_ip(sdk_dir: &Path, serial_opt: Option<&str>) -> Option<String> {
    let bin = resolve_adb_bin(sdk_dir);

    let target_serial = if let Some(s) = serial_opt {
        Some(s.to_string())
    } else {
        let phones = detect_usb_phones(sdk_dir).await;
        phones.into_iter().find(|p| p.status == "device").map(|p| p.serial)
    };

    if let Some(serial) = target_serial {
        let shell_script = "curl -s -4 --max-time 3 http://ip.sb || curl -s -4 --max-time 3 http://ifconfig.me/ip || curl -s -4 --max-time 3 https://api.ipify.org || wget -qO- -T 3 http://ip.sb";
        let mut cmd = Command::new(&bin);
        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(0x08000000);
        }
        if let Ok(out) = cmd.args(["-s", &serial, "shell", shell_script]).output().await {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            for line in text.lines() {
                let s = line.trim();
                if s.split('.').count() == 4 && s.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    return Some(s.to_string());
                }
            }
        }
    }

    get_current_public_ip().await
}

/// 执行单次 ADB 飞行模式换 IP 操作
pub async fn rotate_ip_via_adb(
    sdk_dir: &Path,
    serial_opt: Option<&str>,
    disconnect_wait_s: u32,
    reconnect_wait_s: u32,
    hotspot_ssid: Option<&str>,
    hotspot_password: Option<&str>,
    cancel: Option<&CancellationToken>,
) -> Result<RotateIpResult, String> {
    // 0. Mac 环境先确保 Wi-Fi 处于热点连接状态（Windows 自动空操作）
    ensure_macos_hotspot_connected(hotspot_ssid, hotspot_password).await;

    let bin = resolve_adb_bin(sdk_dir);

    // 1. 查找真机
    let phones = detect_usb_phones(sdk_dir).await;
    if phones.is_empty() {
        return Err("未检测到 USB 连接的安卓真机，请确认已开启 USB 调试并连线。".into());
    }

    let target_serial = match serial_opt {
        Some(s) if phones.iter().any(|p| p.serial == s) => s.to_string(),
        _ => phones[0].serial.clone(),
    };

    tracing::info!(target_serial = %target_serial, "开始执行 USB 手机飞行模式换 IP");

    // 2. 获取旧 IP（优先探测真机自身蜂窝公网 IP，彻底防止电脑端 VPN/代理干扰）
    let old_ip = get_phone_or_public_ip(sdk_dir, Some(&target_serial)).await.unwrap_or_else(|| "未知".into());
    tracing::info!(old_ip = %old_ip, "换 IP 前手机公网 IP");

    let run_adb = |args: &[&str]| {
        let mut cmd = Command::new(&bin);
        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(0x08000000);
        }
        cmd.args(["-s", &target_serial]);
        cmd.args(args);
        cmd
    };

    // 2.5 核心保障：确保飞行模式只断开蜂窝数据 (cell)，绝对不关闭 Wi-Fi 和热点
    let _ = run_adb(&["shell", "settings", "put", "global", "airplane_mode_radios", "cell"]).output().await;

    // 3. 开启飞行模式（断开连接）
    // 方案 A：Android 11+ connectivity cmd
    let res = run_adb(&["shell", "cmd", "connectivity", "airplane-mode", "enable"])
        .output()
        .await;

    let success_a = res.map(|o| o.status.success()).unwrap_or(false);
    if !success_a {
        // 方案 B / C 兜底
        let _ = run_adb(&["shell", "su", "-c", "svc data disable"]).output().await;
        let _ = run_adb(&["shell", "settings", "put", "global", "airplane_mode_on", "1"]).output().await;
        let _ = run_adb(&["shell", "am", "broadcast", "-a", "android.intent.action.AIRPLANE_MODE", "--ez", "state", "true"]).output().await;
    }

    // 等待基站释放旧 session
    for _ in 0..disconnect_wait_s {
        if let Some(c) = cancel {
            if c.is_cancelled() {
                return Err("换 IP 过程被用户取消".into());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // 4. 关闭飞行模式（重新搜网并注册基站）
    let res = run_adb(&["shell", "cmd", "connectivity", "airplane-mode", "disable"])
        .output()
        .await;

    let success_b = res.map(|o| o.status.success()).unwrap_or(false);
    if !success_b {
        let _ = run_adb(&["shell", "su", "-c", "svc data enable"]).output().await;
        let _ = run_adb(&["shell", "settings", "put", "global", "airplane_mode_on", "0"]).output().await;
        let _ = run_adb(&["shell", "am", "broadcast", "-a", "android.intent.action.AIRPLANE_MODE", "--ez", "state", "false"]).output().await;
    }

    // 4.5 守护热点：若系统异常关停了热点，立刻指令拉起
    let _ = run_adb(&["shell", "cmd", "connectivity", "start-tethering", "wifi"]).output().await;

    // 等待网络重新握手
    for _ in 0..reconnect_wait_s {
        if let Some(c) = cancel {
            if c.is_cancelled() {
                return Err("换 IP 过程被用户取消".into());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // 5. 守护 USB 网络共享（防止手机断网后 USB 共享被系统自动关闭）
    let _ = run_adb(&["shell", "svc", "usb", "setFunctions", "rndis"]).output().await;

    // 6. 验证新 IP（重试最多 4 次，间隔 2s）
    let mut new_ip = None;
    for _ in 0..4 {
        if let Some(c) = cancel {
            if c.is_cancelled() {
                return Err("换 IP 过程被用户取消".into());
            }
        }
        if let Some(ip) = get_phone_or_public_ip(sdk_dir, Some(&target_serial)).await {
            new_ip = Some(ip);
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let final_new_ip = match new_ip {
        Some(ip) => ip,
        None => {
            return Err("切换后未检测到有效公网 IP，请检查手机信号与 USB 网络共享状态。".into());
        }
    };

    if final_new_ip != old_ip {
        tracing::info!(old = %old_ip, new = %final_new_ip, "换 IP 成功");
        Ok(RotateIpResult {
            success: true,
            old_ip: old_ip.clone(),
            new_ip: final_new_ip.clone(),
            message: format!("换 IP 成功：{} -> {}", old_ip, final_new_ip),
        })
    } else {
        tracing::warn!(ip = %final_new_ip, "网络已恢复但 IP 未发生改变");
        Ok(RotateIpResult {
            success: false,
            old_ip: old_ip.clone(),
            new_ip: final_new_ip.clone(),
            message: format!("网络已恢复但 IP 未发生变化（仍为 {}），建议适当增加断网等待秒数", final_new_ip),
        })
    }
}
