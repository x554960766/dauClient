//! 设备标识提取（设计文档 §7.2b，v1.1 P0-4）
//! GATE 1 人工环节减摩：客户端自动挖出 ANDROID_ID / UMID，
//! 用户从「自己想办法挖设备号」降为「点复制、点链接、看一眼」。

use crate::adb::{Adb, AdbError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub android_id: String,
    pub umid: String,
    /// umid 提取方式：run-as / root / none
    pub method: String,
}

/// 提取设备标识。
/// 待实测假设 #4：run-as 能读到友盟写的 SharedPreferences（依赖 debuggable=true）。
/// 兜底：adb root（google_apis 镜像支持）后直接读。
pub async fn extract(adb: &Adb, serial: &str, pkg: &str, user: Option<u32>) -> DeviceIdentity {
    let mut android_id = String::new();

    // 路径 0 (优先)：从系统的 settings_ssaid.xml 中提取针对目标应用的真实 SSAID
    // 在 Android 8.0+ 上，应用通过 Settings.Secure 获取到的 ANDROID_ID 实际上是 SSAID
    if adb.root(serial).await.is_ok() {
        let user_id = user.unwrap_or(0);
        let ssaid_cmd = format!("abx2xml /data/system/users/{}/settings_ssaid.xml - 2>/dev/null", user_id);
        if let Ok(xml) = adb.shell(serial, &["sh", "-c", &ssaid_cmd]).await {
            if let Some(val) = parse_ssaid_from_xml(&xml, pkg) {
                android_id = val;
            }
        }
        if android_id.is_empty() {
            let plain_path = format!("/data/system/users/{}/settings_ssaid.xml", user_id);
            if let Ok(xml) = adb.shell_read(serial, &plain_path).await {
                if let Some(val) = parse_ssaid_from_xml(&xml, pkg) {
                    android_id = val;
                }
            }
        }
    }

    if android_id.is_empty() {
        android_id = adb.get_android_id(serial, user).await.unwrap_or_default();
    }

    // 路径 1：run-as（debug 包免 root）
    let run_as = adb
        .run_as_read(serial, pkg, "/data/data/*/shared_prefs/*.xml")
        .await
        .ok();
    if let Some(content) = run_as {
        if let Some(umid) = parse_umid(&content) {
            return DeviceIdentity { android_id, umid, method: "run-as".into() };
        }
    }

    // 路径 2：root 兜底（google_apis 镜像）
    if adb.root(serial).await.is_ok() {
        if let Ok(content) = adb
            .shell_read(serial, &format!("/data/data/{}/shared_prefs/*.xml", pkg))
            .await
        {
            if let Some(umid) = parse_umid(&content) {
                return DeviceIdentity { android_id, umid, method: "root".into() };
            }
        }
    }

    DeviceIdentity { android_id, umid: String::new(), method: "none".into() }
}

/// 从 settings_ssaid.xml 内容中提取目标包名的 SSAID (ANDROID_ID)
pub fn parse_ssaid_from_xml(xml: &str, pkg: &str) -> Option<String> {
    let pkg_target = format!("package=\"{}\"", pkg);
    for line in xml.lines() {
        if line.contains(&pkg_target) {
            if let Some(pos) = line.find("value=\"") {
                let after = &line[pos + 7..];
                if let Some(end) = after.find('"') {
                    let s = &after[..end];
                    if s.len() >= 14 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 从 SharedPreferences XML 中提取 UMID。
/// 友盟常见 key：umeng 相关 prefs 中的 umid / umid_token 等。
fn parse_umid(xml: &str) -> Option<String> {
    // 匹配 <string name="...umid...">值</string>，值一般为 32-48 位十六进制/字母数字串
    let lower = xml.to_lowercase();
    for key in ["umid", "device_token", "umeng_uuid"] {
        if let Some(pos) = lower.find(&format!("name=\"{}", key)) {
            // 从该位置向后找 >...< 之间的值
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
    // 宽松兜底：任何 name 含 umid 的 string 节点
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find("umid") {
        let abs = search_from + pos;
        let rest = &xml[abs..];
        if let Some(gt) = rest.find('>') {
            let after = &rest[gt + 1..];
            if let Some(lt) = after.find('<') {
                let val = after[..lt].trim();
                if val.len() >= 16 && val.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Some(val.to_string());
                }
            }
        }
        search_from = abs + 4;
    }
    None
}

/// 对比两个标识是否不同（重置是否生效的客户端侧即时信号）
pub fn identity_changed(before: &DeviceIdentity, after: &DeviceIdentity) -> bool {
    if !before.umid.is_empty() && !after.umid.is_empty() {
        return before.umid != after.umid;
    }
    !before.android_id.is_empty() && before.android_id != after.android_id
}

impl From<AdbError> for String {
    fn from(e: AdbError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ssaid_from_xml() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<settings version="-1">
  <setting id="0" name="userkey" value="3F8F76D314476453CA261832BC55067C9D861F61662319039ADA9C3E4E28E990" package="android" defaultValue="3F8F76D314476453CA261832BC55067C9D861F61662319039ADA9C3E4E28E990" defaultSysSet="true" tag="null" />
  <setting id="3" name="10127" value="46e8bbd9b5051b81" package="com.google.android.gms" defaultValue="46e8bbd9b5051b81" defaultSysSet="false" tag="null" />
  <setting id="4" name="10205" value="17abd65dec46238b" package="com.xxcb.chenshipin" defaultValue="17abd65dec46238b" defaultSysSet="false" tag="null" />
</settings>"#;

        assert_eq!(parse_ssaid_from_xml(xml, "com.xxcb.chenshipin"), Some("17abd65dec46238b".to_string()));
        assert_eq!(parse_ssaid_from_xml(xml, "com.google.android.gms"), Some("46e8bbd9b5051b81".to_string()));
        assert_eq!(parse_ssaid_from_xml(xml, "com.unknown.app"), None);
    }
}
