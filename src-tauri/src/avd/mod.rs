//! AVD 管理与 emulator 进程启停（设计文档 §5）
//! v1.1 P1-5：Emulator::boot 必须 current_dir(emulator 目录)，
//! 否则 PANIC: Missing emulator engine program。

use crate::adb::AdbEnv;
use crate::sdkmgr::{avdmanager_bin_name, emulator_bin_name};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};

#[derive(Debug, thiserror::Error, Clone, Serialize, Deserialize)]
pub enum AvdError {
    #[error("avdmanager 失败: {0}")]
    Manager(String),
    #[error("emulator 启动失败: {0}")]
    Boot(String),
    #[error("IO 错误: {0}")]
    Io(String),
}

#[derive(Debug, Clone)]
pub struct AvdManager {
    bin: PathBuf,
    env: AdbEnv,
}

impl AvdManager {
    pub fn new(sdk_dir: &Path, env: AdbEnv) -> Self {
        Self {
            bin: sdk_dir.join("cmdline-tools/latest/bin").join(avdmanager_bin_name()),
            env,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        self.env.apply(&mut c);
        c
    }

    /// 创建 AVD（stdin 自动喂 "no"，回答 "Do you wish to create a custom hardware profile?"）
    pub async fn create(&self, name: &str, system_image: &str, device_profile: &str) -> Result<(), AvdError> {
        let mut child = self
            .cmd()
            .args([
                "create", "avd",
                "-n", name,
                "-k", system_image,
                "-d", device_profile,
                "--force",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| AvdError::Manager(e.to_string()))?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(b"no\r\n").await;
            let _ = stdin.flush().await;
            drop(stdin);
        }
        let out = child.wait_with_output().await.map_err(|e| AvdError::Io(e.to_string()))?;
        if !out.status.success() {
            return Err(AvdError::Manager(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        inject_system_properties_to_config_ini(name).await;
        Ok(())
    }

    pub async fn delete(&self, name: &str) -> Result<(), AvdError> {
        let out = self
            .cmd()
            .args(["delete", "avd", "-n", name])
            .output()
            .await
            .map_err(|e| AvdError::Manager(e.to_string()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // 已不存在的 AVD 不算错误（清理幂等）
            if !err.contains("There is no Android Virtual Device") {
                return Err(AvdError::Manager(err));
            }
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<String>, AvdError> {
        let out = self
            .cmd()
            .args(["list", "avd", "-c"])
            .output()
            .await
            .map_err(|e| AvdError::Manager(e.to_string()))?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

#[derive(Debug, Clone, Default)]
pub struct BootOpts {
    pub wipe: bool,
    pub mem_mb: Option<u32>,          // 默认 2048
    pub http_proxy: Option<SocketAddr>, // v1.1：-http-proxy http://127.0.0.1:8899
    pub max_users: Option<u32>,        // v1.1：多用户方案需 fw.max_users
    pub props: Vec<(String, String)>,  // v1.6 方案 A：-prop 自定义系统属性（如品牌 ro.product.brand / 型号 ro.product.model）
}

#[derive(Debug, Clone)]
pub struct DeviceProfileInfo {
    pub brand: &'static str,
    pub model: &'static str,
    pub device: &'static str,
    pub board: &'static str,
    pub hardware: &'static str,
    pub fingerprint: &'static str,
    pub width: u32,
    pub height: u32,
    pub density: u32,
}

pub static DEVICE_PROFILES: &[DeviceProfileInfo] = &[
    // Xiaomi & Redmi（真实工信部入网型号代码）
    DeviceProfileInfo {
        brand: "Xiaomi", model: "24030PN60C", device: "aurora", board: "aurora", hardware: "qcom",
        fingerprint: "Xiaomi/aurora/aurora:14/UKQ1.230804.001/V816.0.4.0.UNACNXM:user/release-keys",
        width: 1440, height: 3200, density: 520,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "23116PN5BC", device: "shennong", board: "shennong", hardware: "qcom",
        fingerprint: "Xiaomi/shennong/shennong:14/UKQ1.230804.001/V816.0.18.0.UNCCNXM:user/release-keys",
        width: 1200, height: 2670, density: 480,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "23127PN0CC", device: "houji", board: "houji", hardware: "qcom",
        fingerprint: "Xiaomi/houji/houji:14/UKQ1.230804.001/V816.0.22.0.UNCCNXM:user/release-keys",
        width: 1200, height: 2670, density: 460,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "2211133C", device: "fuxi", board: "fuxi", hardware: "qcom",
        fingerprint: "Xiaomi/fuxi/fuxi:14/UKQ1.230804.001/V816.0.4.0.UMCCNXM:user/release-keys",
        width: 1080, height: 2400, density: 440,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "23117RK66C", device: "manet", board: "manet", hardware: "qcom",
        fingerprint: "Xiaomi/manet/manet:14/UKQ1.230804.001/V816.0.12.0.UNCCNXM:user/release-keys",
        width: 1440, height: 3200, density: 520,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "2311DRK48C", device: "vermeer", board: "vermeer", hardware: "qcom",
        fingerprint: "Xiaomi/vermeer/vermeer:14/UKQ1.230804.001/V816.0.10.0.UNCCNXM:user/release-keys",
        width: 1440, height: 3200, density: 520,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "22101317C", device: "mondrian", board: "mondrian", hardware: "qcom",
        fingerprint: "Xiaomi/mondrian/mondrian:13/TKQ1.220905.001/V14.0.23.0.TMNCNXM:user/release-keys",
        width: 1440, height: 3200, density: 520,
    },
    DeviceProfileInfo {
        brand: "Xiaomi", model: "23090RA98C", device: "zircon", board: "zircon", hardware: "mtk",
        fingerprint: "Xiaomi/zircon/zircon:13/TP1A.220624.014/V14.0.5.0.TNOCCNXM:user/release-keys",
        width: 1220, height: 2712, density: 450,
    },
    // OPPO & OnePlus
    DeviceProfileInfo {
        brand: "OPPO", model: "PHY110", device: "PHY110", board: "PHY110", hardware: "qcom",
        fingerprint: "OPPO/PHY110/PHY110:14/UP1A.231005.007/14.0.0.501:user/release-keys",
        width: 1440, height: 3168, density: 510,
    },
    DeviceProfileInfo {
        brand: "OPPO", model: "PHZ110", device: "PHZ110", board: "PHZ110", hardware: "mtk",
        fingerprint: "OPPO/PHZ110/PHZ110:14/UP1A.231005.007/14.0.0.401:user/release-keys",
        width: 1264, height: 2780, density: 450,
    },
    DeviceProfileInfo {
        brand: "OPPO", model: "PJJ110", device: "PJJ110", board: "PJJ110", hardware: "qcom",
        fingerprint: "OPPO/PJJ110/PJJ110:14/UP1A.231005.007/14.0.0.210:user/release-keys",
        width: 1240, height: 2772, density: 450,
    },
    DeviceProfileInfo {
        brand: "OPPO", model: "PJH110", device: "PJH110", board: "PJH110", hardware: "mtk",
        fingerprint: "OPPO/PJH110/PJH110:14/UP1A.231005.007/14.0.0.301:user/release-keys",
        width: 1080, height: 2412, density: 440,
    },
    DeviceProfileInfo {
        brand: "OPPO", model: "PHY120", device: "PHY120", board: "PHY120", hardware: "qcom",
        fingerprint: "OPPO/PHY120/PHY120:13/TP1A.220905.001/13.1.1.300:user/release-keys",
        width: 1080, height: 2412, density: 400,
    },
    DeviceProfileInfo {
        brand: "OnePlus", model: "PJD110", device: "PJD110", board: "PJD110", hardware: "qcom",
        fingerprint: "OnePlus/PJD110/PJD110:14/UKQ1.230917.001/14.0.0.304:user/release-keys",
        width: 1440, height: 3168, density: 510,
    },
    DeviceProfileInfo {
        brand: "OnePlus", model: "PJD130", device: "PJD130", board: "PJD130", hardware: "qcom",
        fingerprint: "OnePlus/PJD130/PJD130:14/UKQ1.230917.001/14.0.0.201:user/release-keys",
        width: 1240, height: 2772, density: 450,
    },
    // vivo & iQOO
    DeviceProfileInfo {
        brand: "vivo", model: "V2324A", device: "V2324A", board: "V2324A", hardware: "mtk",
        fingerprint: "vivo/V2324A/V2324A:14/UP1A.231005.007/compiler11181700:user/release-keys",
        width: 1260, height: 2800, density: 450,
    },
    DeviceProfileInfo {
        brand: "vivo", model: "V2309A", device: "V2309A", board: "V2309A", hardware: "mtk",
        fingerprint: "vivo/V2309A/V2309A:14/UP1A.231005.007/compiler11151608:user/release-keys",
        width: 1260, height: 2800, density: 450,
    },
    DeviceProfileInfo {
        brand: "vivo", model: "V2323A", device: "V2323A", board: "V2323A", hardware: "mtk",
        fingerprint: "vivo/V2323A/V2323A:14/UP1A.231005.007/compiler12011100:user/release-keys",
        width: 1080, height: 2400, density: 440,
    },
    DeviceProfileInfo {
        brand: "vivo", model: "V2505A", device: "V2505A", board: "V2505A", hardware: "mtk",
        fingerprint: "vivo/V2505A/V2505A:15/AP3A.240905.015/compiler12101800:user/release-keys",
        width: 1260, height: 2800, density: 450,
    },
    DeviceProfileInfo {
        brand: "vivo", model: "V2307A", device: "V2307A", board: "V2307A", hardware: "qcom",
        fingerprint: "vivo/V2307A/V2307A:14/UP1A.231005.007/compiler11201530:user/release-keys",
        width: 1440, height: 3200, density: 510,
    },
    DeviceProfileInfo {
        brand: "vivo", model: "V2339A", device: "V2339A", board: "V2339A", hardware: "qcom",
        fingerprint: "vivo/V2339A/V2339A:14/UP1A.231005.007/compiler12150900:user/release-keys",
        width: 1260, height: 2800, density: 450,
    },
    // HUAWEI
    DeviceProfileInfo {
        brand: "HUAWEI", model: "ALN-AL00", device: "ALN-AL00", board: "ALN-AL00", hardware: "kirin",
        fingerprint: "HUAWEI/ALN-AL00/ALN-AL00:12/HUAWEIALN-AL00/4.0.0.138:user/release-keys",
        width: 1260, height: 2720, density: 440,
    },
    DeviceProfileInfo {
        brand: "HUAWEI", model: "HBM-AL00", device: "HBM-AL00", board: "HBM-AL00", hardware: "kirin",
        fingerprint: "HUAWEI/HBM-AL00/HBM-AL00:12/HUAWEI4.2.0.115/HBM-AL00:user/release-keys",
        width: 1260, height: 2844, density: 460,
    },
    DeviceProfileInfo {
        brand: "HUAWEI", model: "MNA-AL00", device: "MNA-AL00", board: "MNA-AL00", hardware: "qcom",
        fingerprint: "HUAWEI/MNA-AL00/MNA-AL00:12/HUAWEIMNA-AL00/3.1.0.170:user/release-keys",
        width: 1220, height: 2700, density: 440,
    },
    DeviceProfileInfo {
        brand: "HUAWEI", model: "ADA-AL00U", device: "ADA-AL00U", board: "ADA-AL00U", hardware: "kirin",
        fingerprint: "HUAWEI/ADA-AL00U/ADA-AL00U:12/HUAWEI4.0.0.120/ADA-AL00U:user/release-keys",
        width: 1224, height: 2776, density: 440,
    },
    DeviceProfileInfo {
        brand: "HUAWEI", model: "ALT-AL10", device: "ALT-AL10", board: "ALT-AL10", hardware: "kirin",
        fingerprint: "HUAWEI/ALT-AL10/ALT-AL10:12/HUAWEIALT-AL10/4.0.0.150:user/release-keys",
        width: 1080, height: 2504, density: 420,
    },
    // HONOR
    DeviceProfileInfo {
        brand: "HONOR", model: "BTP-AN20", device: "BTP-AN20", board: "BTP-AN20", hardware: "qcom",
        fingerprint: "HONOR/BTP-AN20/BTP-AN20:14/HONORBTP-AN20/8.0.0.130:user/release-keys",
        width: 1280, height: 2800, density: 450,
    },
    DeviceProfileInfo {
        brand: "HONOR", model: "BTP-AN10", device: "BTP-AN10", board: "BTP-AN10", hardware: "qcom",
        fingerprint: "HONOR/BTP-AN10/BTP-AN10:14/HONORBTP-AN10/8.0.0.120:user/release-keys",
        width: 1280, height: 2800, density: 450,
    },
    DeviceProfileInfo {
        brand: "HONOR", model: "MAA-AN00", device: "MAA-AN00", board: "MAA-AN00", hardware: "qcom",
        fingerprint: "HONOR/MAA-AN00/MAA-AN00:14/HONORMAA-AN00/8.0.0.110:user/release-keys",
        width: 1224, height: 2700, density: 440,
    },
];

/// 根据机型或设备代号查找匹配的真机 Profile
pub fn find_profile_by_model(model: &str) -> Option<DeviceProfileInfo> {
    let clean = model.trim().to_lowercase();
    if clean.is_empty() {
        return None;
    }
    DEVICE_PROFILES.iter().find(|p| {
        let p_model = p.model.to_lowercase();
        let p_dev = p.device.to_lowercase();
        clean == p_model || clean == p_dev || clean.contains(&p_model) || p_model.contains(&clean)
    }).cloned()
}

pub fn random_device_info() -> DeviceProfileInfo {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let idx = (nanos as usize) % DEVICE_PROFILES.len();
    DEVICE_PROFILES[idx].clone()
}

/// 根据指定 Profile 构建真机属性键值对（包含完整的真机 Fingerprint、Hardware、Board、Flavor，防 SDK 识别模拟器）
pub fn device_props_from_info(info: &DeviceProfileInfo) -> Vec<(String, String)> {
    let partitions = ["", ".system", ".vendor", ".product", ".odm", ".system_ext", ".bootimage"];
    let mut props = Vec::new();

    for p in partitions {
        props.push((format!("ro.product{}.brand", p), info.brand.to_string()));
        props.push((format!("ro.product{}.manufacturer", p), info.brand.to_string()));
        props.push((format!("ro.product{}.model", p), info.model.to_string()));
        props.push((format!("ro.product{}.device", p), info.device.to_string()));
        props.push((format!("ro.product{}.name", p), info.device.to_string()));
    }

    props.push(("ro.product.board".to_string(), info.board.to_string()));
    props.push(("ro.build.product".to_string(), info.device.to_string()));
    props.push(("ro.hardware".to_string(), info.hardware.to_string()));
    props.push(("ro.build.flavor".to_string(), format!("{}-user", info.device)));
    props.push(("ro.build.description".to_string(), format!("{}-user 14 release-keys", info.device)));
    props.push(("ro.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.system.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.vendor.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.product.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.odm.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.system_ext.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.bootimage.build.fingerprint".to_string(), info.fingerprint.to_string()));

    // 针对不同国产厂商补充对应 ROM 属性，消除「机型是小米系统却是 AOSP」的风控破绽
    props.extend(vendor_os_props(&info.brand, &info.device));

    props
}

/// 针对国产主流厂商生成特定 ROM 系统版本属性（MIUI / OriginOS / ColorOS / HarmonyOS）
pub fn vendor_os_props(brand: &str, device: &str) -> Vec<(String, String)> {
    let lower = brand.trim().to_lowercase();
    let mut props = vec![
        ("ro.kernel.qemu".to_string(), "0".to_string()),
        ("ro.boot.qemu".to_string(), "0".to_string()),
        ("ro.hardware.virtual".to_string(), "0".to_string()),
    ];
    if lower == "xiaomi" || lower == "redmi" {
        props.push(("ro.miui.ui.version.name".to_string(), "V14".to_string()));
        props.push(("ro.miui.ui.version.code".to_string(), "14".to_string()));
        props.push(("ro.miui.version.code_time".to_string(), "1700000000".to_string()));
        props.push(("ro.build.display.id".to_string(), format!("{}-user 14 HyperOS-1.0.8.0 release-keys", device)));
    } else if lower == "vivo" || lower == "iqoo" {
        props.push(("ro.vivo.os.name".to_string(), "OriginOS".to_string()));
        props.push(("ro.vivo.os.version".to_string(), "4.0".to_string()));
        props.push(("ro.vivo.os.build.version".to_string(), "4.0".to_string()));
        props.push(("ro.vivo.product.version".to_string(), "4.0".to_string()));
        props.push(("ro.build.display.id".to_string(), format!("OriginOS 4.0.1.{}", device)));
    } else if lower == "oppo" || lower == "oneplus" || lower == "realme" {
        props.push(("ro.build.version.opporom".to_string(), "ColorOS 14.0".to_string()));
        props.push(("ro.rom.different.version".to_string(), "ColorOS 14.0".to_string()));
        props.push(("ro.build.display.id".to_string(), format!("{}_14.0.0.300(CN01)", device)));
    } else if lower == "huawei" {
        props.push(("ro.build.version.emui".to_string(), "HarmonyOS 4.0.0".to_string()));
        props.push(("ro.build.hw_emui_api_level".to_string(), "29".to_string()));
        props.push(("ro.build.display.id".to_string(), format!("HarmonyOS 4.0.0.138({})", device)));
    } else if lower == "honor" {
        props.push(("ro.build.version.magic".to_string(), "MagicOS 8.0".to_string()));
        props.push(("ro.honor.build.version.incremental".to_string(), "8.0.0.130".to_string()));
        props.push(("ro.build.display.id".to_string(), format!("MagicOS 8.0.0.130({})", device)));
    } else {
        props.push(("ro.build.display.id".to_string(), format!("{}-user 14 release-keys", device)));
    }
    props
}

/// 随机常见 Android 设备品牌与型号属性
pub fn random_device_props() -> Vec<(String, String)> {
    let info = random_device_info();
    device_props_from_info(&info)
}

/// 按 AVD 名「确定性」选择真机档案：同一 AVD 永远返回同一机型。
///
/// 这是 L3 批量提速的关键。`apply_device_spoofing` 会把机型指纹写入 system 分区的
/// build.prop（`-wipe-data` 只清 userdata，改动持久）。若机型固定，同一 AVD 首次 L3
/// 完整伪装（含 build.prop 重启）之后，后续 L3  wipe 重启时 `getprop` 读到的仍是该机型，
/// 命中 apply_device_spoofing 开头的「型号已匹配」早退分支，从而跳过 root/remount/重启，
/// 实现「同一 AVD 只有第一次 L3 双启动，后续 L3 单启动」。
/// 不同 AVD（不同 slot）仍映射到不同机型，跨设备多样性不受影响。
pub fn device_info_for_avd(avd_name: &str) -> DeviceProfileInfo {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    avd_name.hash(&mut h);
    let idx = (h.finish() as usize) % DEVICE_PROFILES.len();
    DEVICE_PROFILES[idx].clone()
}

/// 与 `device_info_for_avd` 配套的确定性 props（供 BootOpts.props / config.ini 注入）
pub fn device_props_for_avd(avd_name: &str) -> Vec<(String, String)> {
    device_props_from_info(&device_info_for_avd(avd_name))
}

/// 将真机品牌/型号以及物理分辨率/DPI 注入 AVD 的 config.ini（Android 原生 system.property.* 及 hw.lcd 覆盖机制）
pub async fn inject_system_properties_to_config_ini_with_profile(avd_name: &str, custom_info: Option<&DeviceProfileInfo>) {
    let avd_dir = if let Ok(custom) = std::env::var("ANDROID_AVD_HOME") {
        PathBuf::from(custom).join(format!("{}.avd", avd_name))
    } else if let Some(home) = dirs::home_dir() {
        home.join(".android").join("avd").join(format!("{}.avd", avd_name))
    } else {
        return;
    };

    let config_path = avd_dir.join("config.ini");
    if !config_path.exists() {
        return;
    }

    let default_info;
    let info = match custom_info {
        Some(i) => i,
        None => {
            default_info = random_device_info();
            &default_info
        }
    };

    let partitions = ["", ".system", ".vendor", ".product", ".odm", ".system_ext", ".bootimage"];
    let mut props = Vec::new();

    for p in partitions {
        props.push((format!("ro.product{}.brand", p), info.brand.to_string()));
        props.push((format!("ro.product{}.manufacturer", p), info.brand.to_string()));
        props.push((format!("ro.product{}.model", p), info.model.to_string()));
        props.push((format!("ro.product{}.device", p), info.device.to_string()));
        props.push((format!("ro.product{}.name", p), info.device.to_string()));
    }

    props.push(("ro.product.board".to_string(), info.board.to_string()));
    props.push(("ro.build.product".to_string(), info.device.to_string()));
    props.push(("ro.hardware".to_string(), info.hardware.to_string()));
    props.push(("ro.build.flavor".to_string(), format!("{}-user", info.device)));
    props.push(("ro.build.description".to_string(), format!("{}-user 14 release-keys", info.device)));
    props.push(("ro.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.system.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.vendor.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.product.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.odm.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.system_ext.build.fingerprint".to_string(), info.fingerprint.to_string()));
    props.push(("ro.bootimage.build.fingerprint".to_string(), info.fingerprint.to_string()));

    // 注入对应国产厂商特有 ROM 系统属性
    props.extend(vendor_os_props(&info.brand, &info.device));

    let mut extra = String::new();
    extra.push_str("\n# Custom System Properties & Display Hardware\n");
    extra.push_str(&format!("hw.lcd.width={}\n", info.width));
    extra.push_str(&format!("hw.lcd.height={}\n", info.height));
    extra.push_str(&format!("hw.lcd.density={}\n", info.density));
    // 多开并发轻量化硬件配置（跨平台生效，降低系统级内存与虚拟设备开销）
    extra.push_str("hw.ramSize=1280\n");
    extra.push_str("vm.heapSize=256\n");
    extra.push_str("hw.cpu.ncore=2\n");
    extra.push_str("hw.camera.back=none\n");
    extra.push_str("hw.camera.front=none\n");
    extra.push_str("hw.audioInput=no\n");
    extra.push_str("hw.audioOutput=no\n");

    for (k, v) in props {
        extra.push_str(&format!("system.property.{}={}\n", k, v));
    }

    if let Ok(existing) = tokio::fs::read_to_string(&config_path).await {
        let cleaned: Vec<&str> = existing
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.starts_with("system.property.")
                    && !t.starts_with("hw.lcd.width")
                    && !t.starts_with("hw.lcd.height")
                    && !t.starts_with("hw.lcd.density")
                    && !t.starts_with("hw.ramSize")
                    && !t.starts_with("vm.heapSize")
                    && !t.starts_with("hw.cpu.ncore")
                    && !t.starts_with("hw.camera.")
                    && !t.starts_with("hw.audio")
            })
            .collect();
        let mut new_content = cleaned.join("\n");
        new_content.push_str(&extra);
        let _ = tokio::fs::write(&config_path, new_content).await;
        tracing::info!(
            brand = %info.brand,
            model = %info.model,
            width = info.width,
            height = info.height,
            density = info.density,
            "L3/AVD 屏幕分辨率与真机属性已注入 config.ini"
        );
    }
}

pub async fn inject_system_properties_to_config_ini(avd_name: &str) {
    inject_system_properties_to_config_ini_with_profile(avd_name, None).await;
}

/// 强行清理指定 AVD 的残留文件锁（防 QEMU 报 FATAL: Another emulator instance is running）
pub fn clean_avd_lock_files(avd_name: &str) {
    let avd_dir = if let Ok(custom) = std::env::var("ANDROID_AVD_HOME") {
        PathBuf::from(custom).join(format!("{}.avd", avd_name))
    } else if let Some(home) = dirs::home_dir() {
        home.join(".android").join("avd").join(format!("{}.avd", avd_name))
    } else {
        return;
    };

    if !avd_dir.exists() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(&avd_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".lock") {
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// 检查指定 TCP 端口是否被彻底释放（用来确定 QEMU 模拟器进程已真正关闭）
pub async fn wait_port_free(port: u16, max_wait_s: u64) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < max_wait_s {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// 强行杀死占用指定端口（以及 port+1 对应 adb 端口）的旧 QEMU/emulator 进程
pub async fn kill_emulator_on_port(port: u16) {
    #[cfg(unix)]
    {
        for p in [port, port + 1] {
            let port_s = p.to_string();
            if let Ok(out) = std::process::Command::new("lsof")
                .args(["-ti", &format!(":{}", port_s)])
                .output()
            {
                let pids = String::from_utf8_lossy(&out.stdout);
                for pid in pids.lines() {
                    if let Ok(pid_num) = pid.trim().parse::<i32>() {
                        unsafe {
                            libc::kill(pid_num, libc::SIGKILL);
                        }
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for p in [port, port + 1] {
            let mut cmd = std::process::Command::new("powershell");
            cmd.args([
                "-NoProfile",
                "-Command",
                &format!("Get-NetTCPConnection -LocalPort {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess | ForEach-Object {{ Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }}", p),
            ]);
            cmd.creation_flags(0x08000000);
            let _ = cmd.output();
        }
    }
}

#[derive(Debug, Clone)]
pub struct Emulator {
    bin: PathBuf,
    dir: PathBuf,
    env: AdbEnv,
}

impl Emulator {
    pub fn new(sdk_dir: &Path, env: AdbEnv) -> Self {
        let dir = sdk_dir.join("emulator");
        Self {
            bin: dir.join(emulator_bin_name()),
            dir,
            env,
        }
    }

    /// 等价脚本 boot_emu。
    /// 关键（v1.1 P1-5）：current_dir 必须是 emulator 目录，
    /// 二进制靠自身所在目录定位 lib64/、qemu/ 等资源。
    pub async fn boot(&self, avd: &str, port: u16, opts: &BootOpts) -> Result<Child, AvdError> {
        if !wait_port_free(port, 2).await {
            tracing::warn!("[AVD] 端口 {} 未被释放，尝试清理残留 QEMU 进程...", port);
            kill_emulator_on_port(port).await;
            let _ = wait_port_free(port, 3).await;
        }
        clean_avd_lock_files(avd);

        let mut cmd = Command::new(&self.bin);
        self.env.apply(&mut cmd);
        cmd.current_dir(&self.dir);
        let gpu_mode = if cfg!(target_os = "macos") {
            "host"
        } else if cfg!(target_os = "windows") {
            "swiftshader_indirect"
        } else {
            "swiftshader_indirect"
        };

        cmd.args([
            "-avd", avd,
            "-port", &port.to_string(),
            "-accel", "auto",
            "-cores", "2",
            "-no-window",
            "-no-audio",
            "-no-snapshot",
            "-no-boot-anim",
            "-no-metrics",
            "-writable-system",
            "-camera-back", "none",
            "-camera-front", "none",
            "-no-passive-gps",
            "-dns-server", "114.114.114.114,8.8.8.8",
            "-timezone", "Asia/Shanghai",
            "-gpu", gpu_mode,
            "-memory", &opts.mem_mb.unwrap_or(1280).to_string(),
        ]);
        if opts.wipe {
            cmd.arg("-wipe-data");
        }
        if let Some(proxy) = &opts.http_proxy {
            cmd.args(["-http-proxy", &format!("http://{}", proxy)]);
        }
        if let Some(max_users) = opts.max_users {
            cmd.args(["-prop", &format!("fw.max_users={}", max_users)]);
        }
        // 方案 A：通过 -prop 命令注入系统级 ro.product 品牌与型号
        for (k, v) in &opts.props {
            cmd.args(["-prop", &format!("{}={}", k, v)]);
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.spawn().map_err(|e| AvdError::Boot(e.to_string()))
    }
}
