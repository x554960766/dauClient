//! 身份快照档案库（Profile Archive System）
//! 支持 300~10,000+ 台留存设备，仅占用几兆磁盘空间。
//!
//! 原理：
//! 1. 建库阶段：新运行设备后，将系统的 ANDROID_ID (SSAID) 与 App 私有 shared_prefs XML
//!    及硬件品牌属性打包备份为 profiles/profile_XXXX.json 档案文件（~2 KB/台）。
//! 2. 刷留存阶段：复用通用槽位模拟器，通过 adb root 将档案中的 ANDROID_ID、shared_prefs
//!    和硬件指纹精准还原回模拟器，友盟 SDK 读取到的标识与 Day 1 严格一致，触发“老用户回访”。

use crate::adb::{Adb, AdbError};
use crate::avd::DeviceProfileInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 身份快照档案（单台留存设备的完整标识凭证）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityProfile {
    pub profile_id: String,
    pub pkg: String,
    pub android_id: String,
    pub umid: String,
    pub brand: String,
    pub model: String,
    pub device: String,
    pub board: String,
    pub hardware: String,
    pub fingerprint: String,
    pub width: u32,
    pub height: u32,
    pub density: u32,
    /// shared_prefs 文件内容表：文件名 (如 umeng_general.xml) -> XML 文本
    pub shared_prefs: HashMap<String, String>,
    pub created_at: String,
    pub last_used_at: String,
}

impl IdentityProfile {
    /// 将档案的硬件属性转换为 BootOpts 能够识别的 props 键值对
    pub fn to_boot_props(&self) -> Vec<(String, String)> {
        let partitions = ["", ".system", ".vendor", ".product", ".odm", ".system_ext", ".bootimage"];
        let mut props = Vec::new();

        for p in partitions {
            props.push((format!("ro.product{}.brand", p), self.brand.clone()));
            props.push((format!("ro.product{}.manufacturer", p), self.brand.clone()));
            props.push((format!("ro.product{}.model", p), self.model.clone()));
            props.push((format!("ro.product{}.device", p), self.device.clone()));
            props.push((format!("ro.product{}.name", p), self.device.clone()));
        }

        props.push(("ro.product.board".to_string(), self.board.clone()));
        props.push(("ro.build.product".to_string(), self.device.clone()));
        props.push(("ro.hardware".to_string(), self.hardware.clone()));
        props.push(("ro.build.flavor".to_string(), format!("{}-user", self.device)));
        props.push(("ro.build.description".to_string(), format!("{}-user 14 release-keys", self.device)));
        props.push(("ro.build.fingerprint".to_string(), self.fingerprint.clone()));
        props.push(("ro.system.build.fingerprint".to_string(), self.fingerprint.clone()));
        props.push(("ro.vendor.build.fingerprint".to_string(), self.fingerprint.clone()));
        props.push(("ro.product.build.fingerprint".to_string(), self.fingerprint.clone()));
        props.push(("ro.odm.build.fingerprint".to_string(), self.fingerprint.clone()));
        props.push(("ro.system_ext.build.fingerprint".to_string(), self.fingerprint.clone()));
        props.push(("ro.bootimage.build.fingerprint".to_string(), self.fingerprint.clone()));

        props
    }

    /// 转为 DeviceProfileInfo 方便注入 config.ini
    pub fn to_profile_info(&self) -> DeviceProfileInfo {
        DeviceProfileInfo {
            brand: Box::leak(self.brand.clone().into_boxed_str()),
            model: Box::leak(self.model.clone().into_boxed_str()),
            device: Box::leak(self.device.clone().into_boxed_str()),
            board: Box::leak(self.board.clone().into_boxed_str()),
            hardware: Box::leak(self.hardware.clone().into_boxed_str()),
            fingerprint: Box::leak(self.fingerprint.clone().into_boxed_str()),
            width: self.width,
            height: self.height,
            density: self.density,
        }
    }
}

/// 从运行中的设备导出身份快照档案
pub async fn backup_from_device(
    adb: &Adb,
    serial: &str,
    pkg: &str,
    profile_id: &str,
    info: &DeviceProfileInfo,
) -> Result<IdentityProfile, AdbError> {
    let android_id = adb.get_android_id(serial, None).await.unwrap_or_default();
    let mut shared_prefs = HashMap::new();
    let mut umid = String::new();

    // 请求 root 权限以读取私有 Shared Preferences
    if adb.root(serial).await.is_ok() {
        let prefs_dir = format!("/data/data/{}/shared_prefs", pkg);
        if let Ok(ls_out) = adb.shell(serial, &["ls", &prefs_dir]).await {
            for line in ls_out.lines() {
                let filename = line.trim();
                if filename.ends_with(".xml") {
                    let file_path = format!("{}/{}", prefs_dir, filename);
                    if let Ok(xml) = adb.shell_read(serial, &file_path).await {
                        if umid.is_empty() {
                            if let Some(parsed) = parse_umid(&xml) {
                                umid = parsed;
                            }
                        }
                        shared_prefs.insert(filename.to_string(), xml);
                    }
                }
            }
        }
    }

    let now = crate::engine::beijing_now();
    Ok(IdentityProfile {
        profile_id: profile_id.to_string(),
        pkg: pkg.to_string(),
        android_id,
        umid,
        brand: info.brand.to_string(),
        model: info.model.to_string(),
        device: info.device.to_string(),
        board: info.board.to_string(),
        hardware: info.hardware.to_string(),
        fingerprint: info.fingerprint.to_string(),
        width: info.width,
        height: info.height,
        density: info.density,
        shared_prefs,
        created_at: now.clone(),
        last_used_at: now,
    })
}

/// 将身份快照档案注入到运行中的模拟器（还原 ANDROID_ID + shared_prefs 凭证）
pub async fn restore_to_device(
    adb: &Adb,
    serial: &str,
    pkg: &str,
    profile: &IdentityProfile,
) -> Result<(), AdbError> {
    if adb.root(serial).await.is_err() {
        return Err(AdbError::CommandFailed {
            cmd: "adb root".into(),
            code: -1,
            stderr: "无法获取 root 权限以还原身份档案".into(),
        });
    }

    // 1. 还原系统的 ANDROID_ID (SSAID) 文件
    if !profile.android_id.is_empty() {
        inject_ssaid(adb, serial, pkg, &profile.android_id).await?;
    }

    // 2. 还原 App 私有 shared_prefs
    let prefs_dir = format!("/data/data/{}/shared_prefs", pkg);
    let _ = adb.shell(serial, &["mkdir", "-p", &prefs_dir]).await;

    for (filename, xml_content) in &profile.shared_prefs {
        let file_path = format!("{}/{}", prefs_dir, filename);
        // 使用 sh -c 配合 base64 -d 将 XML 内容可靠安全地写回 Android 文件系统
        let encoded = base64_encode(xml_content.as_bytes());
        let _ = adb
            .shell(
                serial,
                &[
                    "sh",
                    "-c",
                    &format!("echo '{}' | base64 -d > {}", encoded, file_path),
                ],
            )
            .await;
    }

    // 3. 修复文件所有权 (chown/chmod)
    if let Ok(uid_str) = adb.shell(serial, &["stat", "-c", "%u", &format!("/data/data/{}", pkg)]).await {
        let uid = uid_str.trim();
        if !uid.is_empty() && uid.chars().all(|c| c.is_ascii_digit()) {
            let _ = adb.shell(serial, &["chown", "-R", &format!("{}:{}", uid, uid), &prefs_dir]).await;
            let _ = adb.shell(serial, &["chmod", "-R", "660", &prefs_dir]).await;
            let _ = adb.shell(serial, &["chmod", "771", &prefs_dir]).await;
        }
    }

    Ok(())
}

/// 注入系统的 settings_ssaid.xml 文件中的包名条目
pub async fn inject_ssaid(adb: &Adb, serial: &str, pkg: &str, android_id: &str) -> Result<(), AdbError> {
    const SSAID_PATH: &str = "/data/system/users/0/settings_ssaid.xml";
    let ssaid_xml = adb.shell_read(serial, SSAID_PATH).await.unwrap_or_default();

    let new_xml = if ssaid_xml.contains(&format!("package=\"{}\"", pkg)) {
        // 存在则替换该包名的 value 属性
        let mut result = Vec::new();
        for line in ssaid_xml.lines() {
            if line.contains(&format!("package=\"{}\"", pkg)) {
                // 替换 value="xxx"
                let updated = replace_xml_attr(line, "value", android_id);
                let updated = replace_xml_attr(&updated, "defaultValue", android_id);
                result.push(updated);
            } else {
                result.push(line.to_string());
            }
        }
        result.join("\n")
    } else {
        // 不存在则在 </ssaidSettings> 前插入新条目
        let new_entry = format!(
            "  <setting id=\"99\" name=\"10000\" value=\"{}\" package=\"{}\" defaultValue=\"{}\" defaultSysSet=\"false\" tag=\"null\" />",
            android_id, pkg, android_id
        );
        if ssaid_xml.contains("</ssaidSettings>") {
            ssaid_xml.replace("</ssaidSettings>", &format!("{}\n</ssaidSettings>", new_entry))
        } else {
            format!(
                "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<ssaidSettings version=\"1\">\n{}\n</ssaidSettings>",
                new_entry
            )
        }
    };

    let encoded = base64_encode(new_xml.as_bytes());
    let _ = adb
        .shell(
            serial,
            &["sh", "-c", &format!("echo '{}' | base64 -d > {}", encoded, SSAID_PATH)],
        )
        .await;

    // 辅助注入 secure 命名空间（兼容旧 Android）
    let _ = adb
        .shell(serial, &["settings", "put", "secure", "android_id", android_id])
        .await;

    Ok(())
}

fn replace_xml_attr(line: &str, attr: &str, new_val: &str) -> String {
    let target = format!("{}=\"", attr);
    if let Some(start) = line.find(&target) {
        let val_start = start + target.len();
        if let Some(end) = line[val_start..].find('"') {
            let mut s = String::new();
            s.push_str(&line[..val_start]);
            s.push_str(new_val);
            s.push_str(&line[val_start + end..]);
            return s;
        }
    }
    line.to_string()
}

fn parse_umid(xml: &str) -> Option<String> {
    let lower = xml.to_lowercase();
    for key in ["umid", "device_token", "umeng_uuid"] {
        if let Some(pos) = lower.find(&format!("name=\"{}", key)) {
            let rest = &xml[pos..];
            if let Some(gt) = rest.find('>') {
                let after = &rest[gt + 1..];
                if let Some(lt) = after.find('<') {
                    let val = after[..lt].trim();
                    if val.len() >= 16 && val.chars().all(|c| c.is_ascii_alphanumeric()) {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    None
}

fn base64_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let _ = write!(out, "{}", CHARS[((triple >> 18) & 0x3F) as usize] as char);
        let _ = write!(out, "{}", CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            let _ = write!(out, "{}", CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            let _ = write!(out, "{}", CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 档案库存储管理器
pub struct ProfileArchiveManager;

impl ProfileArchiveManager {
    /// 默认档案存储目录（`<runs_dir>/../profiles/`）
    pub fn default_profiles_dir(base: &Path) -> PathBuf {
        base.parent().unwrap_or(base).join("profiles")
    }

    /// 加载档案库中的所有身份档案
    pub async fn load_all(dir: &Path) -> Vec<IdentityProfile> {
        let mut profiles = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(content) = tokio::fs::read_to_string(&path).await {
                        if let Ok(prof) = serde_json::from_str::<IdentityProfile>(&content) {
                            profiles.push(prof);
                        }
                    }
                }
            }
        }
        profiles.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
        profiles
    }

    /// 保存单份身份档案到 JSON 文件
    pub async fn save(dir: &Path, profile: &IdentityProfile) -> std::io::Result<()> {
        tokio::fs::create_dir_all(dir).await?;
        let file_path = dir.join(format!("{}.json", profile.profile_id));
        let json = serde_json::to_string_pretty(profile)?;
        tokio::fs::write(file_path, json).await
    }

    /// 清空所有档案
    pub async fn clear(dir: &Path) -> std::io::Result<usize> {
        let mut count = 0;
        if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}
