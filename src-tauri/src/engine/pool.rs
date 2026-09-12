//! 设备池持久化（留存方案核心）
//! 将 AVD 设备信息持久化到 JSON 文件，跨天复用同一设备身份，
//! 让友盟识别为老用户回访，从而产生留存数据。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 池中单台设备的持久化记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolDevice {
    /// AVD 名称（如 pool-0、pool-1）
    pub avd_name: String,
    /// 设备品牌/型号/指纹（跨天复用时必须一致）
    pub brand: String,
    pub model: String,
    pub device: String,
    pub board: String,
    pub hardware: String,
    pub fingerprint: String,
    pub width: u32,
    pub height: u32,
    pub density: u32,
    /// 首次运行后提取的 ANDROID_ID（友盟用它识别老用户）
    pub android_id: String,
    pub created_at: String,
    pub last_used_at: String,
}

impl PoolDevice {
    /// 从随机设备配置 + AVD 名称构建新的池设备记录
    pub fn new(index: u32, info: &crate::avd::DeviceProfileInfo) -> Self {
        Self {
            avd_name: format!("pool-{}", index),
            brand: info.brand.to_string(),
            model: info.model.to_string(),
            device: info.device.to_string(),
            board: info.board.to_string(),
            hardware: info.hardware.to_string(),
            fingerprint: info.fingerprint.to_string(),
            width: info.width,
            height: info.height,
            density: info.density,
            android_id: String::new(),
            created_at: crate::engine::beijing_now(),
            last_used_at: String::new(),
        }
    }

    /// 将池设备信息转为 BootOpts 所需的 props 列表（与 random_device_props 同构，但使用固定值）
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
}

/// 设备池（JSON 持久化）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DevicePool {
    pub devices: Vec<PoolDevice>,
}

impl DevicePool {
    /// 从 JSON 文件加载设备池（不存在则返回空池）
    pub async fn load(file: &Path) -> Self {
        if let Ok(content) = tokio::fs::read_to_string(file).await {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// 持久化到 JSON 文件
    pub async fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(p) = file.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(file, json).await
    }

    /// 池中设备数量
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// 取指定数量的设备（按索引顺序，不超过池大小）
    pub fn acquire(&self, count: u32) -> Vec<PoolDevice> {
        self.devices.iter().take(count as usize).cloned().collect()
    }

    /// 计算下一台新设备的索引号
    fn next_index(&self) -> u32 {
        self.devices
            .iter()
            .filter_map(|d| d.avd_name.strip_prefix("pool-"))
            .filter_map(|s| s.parse::<u32>().ok())
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }

    /// 添加一台新设备到池中（返回新设备的引用）
    pub fn add_device(&mut self, info: &crate::avd::DeviceProfileInfo) -> PoolDevice {
        let idx = self.next_index();
        let dev = PoolDevice::new(idx, info);
        self.devices.push(dev.clone());
        dev
    }

    /// 更新指定设备的 android_id 和 last_used_at
    pub fn update_device(&mut self, avd_name: &str, android_id: &str) {
        if let Some(dev) = self.devices.iter_mut().find(|d| d.avd_name == avd_name) {
            if !android_id.is_empty() {
                dev.android_id = android_id.to_string();
            }
            dev.last_used_at = crate::engine::beijing_now();
        }
    }

    /// 从池中移除指定设备
    pub fn remove_device(&mut self, avd_name: &str) {
        self.devices.retain(|d| d.avd_name != avd_name);
    }
}

/// 设备池文件的默认路径（与 runs_dir 同级）
pub fn default_pool_file(base: &Path) -> PathBuf {
    base.parent().unwrap_or(base).join("device_pool.json")
}
