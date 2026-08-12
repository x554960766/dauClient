//! license 直写 hash 文件（v1.1 P0-2）
//! CI 标准做法：写完 license 文件后 sdkmanager 全程无交互。
//! 不喂 stdin——条数随版本变化，失败时表现为静默挂起，最难排查。

use std::fs;
use std::path::Path;

/// (文件名, hash 列表)。hash 为 Google 公开的全量 SDK license SHA-1。
const LICENSE_FILES: &[(&str, &[&str])] = &[
    (
        "android-sdk-license",
        &[
            "8933bad161af4178b1185d1a37fbf41ea5269c55",
            "d56f5187479451eabf01fb78af6dfcb131a6481e",
            "24333f8a63b6825ea9c5514f83c2829b004d1fee",
        ],
    ),
    ("android-sdk-preview-license", &["84831b9409646a918e30573bab4c9c91346d8abd"]),
    ("android-sdk-arm-dbt-license", &["859f317696f67ef3d7f30a50a5560e7834b43903"]),
    ("android-googletv-license", &["601085b94e77fbb98d06e26c2ef1c47a2b9b76e5"]),
    ("android-sdk-preview-license-old", &["79120722343a6f314e0719f863036c702b0e6b2a"]),
    ("google-gdk-license", &["33b6a2b64607f11b759f320ef9dff4ae5c47d97a"]),
    ("mips-android-sysimage-license", &["e9acab5b5fbb560a72cfaecce8946896ff6aab9d"]),
    ("intel-android-extra-license", &["d975f751698a77b662f1254ddbeed3901e976f5a"]),
];

/// 初始化时直写全部 license 文件
pub fn write_license_files(sdk_dir: &Path) -> std::io::Result<()> {
    let dir = sdk_dir.join("licenses");
    fs::create_dir_all(&dir)?;
    for (name, hashes) in LICENSE_FILES {
        let content = hashes.join("\n") + "\n";
        fs::write(dir.join(name), content)?;
    }
    Ok(())
}

/// 预检：license 文件存在且内容匹配
pub fn licenses_ok(sdk_dir: &Path) -> bool {
    let dir = sdk_dir.join("licenses");
    LICENSE_FILES.iter().take(3).all(|(name, hashes)| {
        fs::read_to_string(dir.join(name))
            .map(|c| hashes.iter().all(|h| c.contains(h)))
            .unwrap_or(false)
    })
}
