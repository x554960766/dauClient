//! sdkmgr：环境自动准备（设计文档 §4）
//! - 组件检测 / 下载 / SHA-256 校验 / 解压归位
//! - license 直写 hash 文件（v1.1 P0-2）
//! - Windows 短路径与虚拟化引导（v1.1 P0-3）

pub mod download;
pub mod licenses;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 客户端私有 SDK 根目录
/// v1.1 P0-3a：Windows 默认短路径，避开 260 字符限制。
/// macOS/Linux：必须用**无空格**路径 —— sdkmanager 启动脚本用 `eval` 展开
/// `$APP_HOME`（含 `-Dcom.android.sdklib.toolsdir=$APP_HOME`），路径一旦含空格，
/// 该 -D 参数会被 shell 劈裂，剩余片段被当成 Java 主类，触发
/// `ClassNotFoundException: Support.umeng-dau-client.sdk.cmdline-tools.latest`。
/// `~/Library/Application Support` 带空格，故改用 `~/Library/Caches`（无空格）。
pub fn default_sdk_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"C:\umeng-dau-sdk")
    }
    #[cfg(not(target_os = "windows"))]
    {
        dirs::cache_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("umeng-dau-client")
            .join("sdk")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "detail")]
pub enum ComponentState {
    Missing,
    Outdated,
    Downloading(u8),
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub label: String,
    pub size_hint_mb: u32,
    pub required: bool,
    pub state: ComponentState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupReport {
    pub sdk_dir: String,
    pub platform: String,
    pub arch: String,
    pub components: Vec<Component>,
    pub hypervisor: HypervisorState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypervisorState {
    pub platform: String,
    /// macOS 恒为 ready（Hypervisor.framework 系统自带）
    pub ready: bool,
    pub detail: String,
}

/// 当前平台/架构描述
pub fn host_platform() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "win"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    (os, arch)
}

/// 系统镜像 ID 按宿主机架构自动选择（用户不可改架构——上游 0.2 的坑代码层消除）
pub fn system_image_id(api_level: u32) -> String {
    let (_, arch) = host_platform();
    let abi = if arch == "arm64" { "arm64-v8a" } else { "x86_64" };
    format!("system-images;android-{};google_apis;{}", api_level, abi)
}

/// 系统镜像在磁盘上的目录路径（统一辅助函数）。
///
/// `image_id` 形如 `"system-images;android-34;google_apis;arm64-v8a"`，
/// `replace(';', "/")` 后已含 `system-images/` 前缀，直接 join 即可。
///
/// **历史教训**：此前三处调用方各自做 `sdk.join("system-images").join(&dir_name)`，
/// 导致 `system-images` 被拼了两次，路径变成 `sdk/system-images/system-images/...`，
/// 引发「组件清单显示未安装」「预检红叉」「每次初始化重复下载 1.5GB 镜像」三个 bug。
/// 统一走此函数可杜绝此类问题。
pub fn system_image_dir(sdk_dir: &std::path::Path, image_id: &str) -> PathBuf {
    sdk_dir.join(image_id.replace(';', "/"))
}

/// 组件清单（体积为估算值，用于 SetupWizard 展示）
pub fn component_list(sdk_dir: &std::path::Path, api_levels: &[u32]) -> Vec<Component> {
    let mk = |id: &str, label: &str, size: u32, ready: bool| Component {
        id: id.to_string(),
        label: label.to_string(),
        size_hint_mb: size,
        required: true,
        state: if ready {
            ComponentState::Ready
        } else {
            ComponentState::Missing
        },
    };
    let mut list = vec![
        mk(
            "cmdline-tools",
            "Android SDK 命令行工具",
            130,
            sdk_dir.join("cmdline-tools/latest/bin").exists(),
        ),
        mk(
            "jre",
            "内嵌 Java 运行时（供 sdkmanager 使用）",
            50,
            jre_java_bin(sdk_dir).exists(),
        ),
        mk(
            "platform-tools",
            "platform-tools (adb)",
            15,
            sdk_dir.join("platform-tools").join(adb_bin_name()).exists(),
        ),
        mk(
            "emulator",
            "Android Emulator",
            400,
            sdk_dir.join("emulator").join(emulator_bin_name()).exists(),
        ),
        mk(
            "build-tools",
            "build-tools 34.0.0 (aapt)",
            60,
            sdk_dir.join("build-tools/34.0.0").join(aapt_bin_name()).exists(),
        ),
    ];
    for api in api_levels {
        list.push(mk(
            &format!("image-{}", api),
            &format!("系统镜像 android-{}", api),
            1500,
            system_image_dir(sdk_dir, &system_image_id(*api)).exists(),
        ));
    }
    list
}

pub fn adb_bin_name() -> &'static str {
    if cfg!(windows) { "adb.exe" } else { "adb" }
}
pub fn emulator_bin_name() -> &'static str {
    if cfg!(windows) { "emulator.exe" } else { "emulator" }
}
pub fn aapt_bin_name() -> &'static str {
    if cfg!(windows) { "aapt.exe" } else { "aapt" }
}
pub fn sdkmanager_bin_name() -> &'static str {
    if cfg!(windows) { "sdkmanager.bat" } else { "sdkmanager" }
}
pub fn avdmanager_bin_name() -> &'static str {
    if cfg!(windows) { "avdmanager.bat" } else { "avdmanager" }
}

pub fn jre_java_bin(sdk_dir: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        sdk_dir.join("jre/Contents/Home/bin/java")
    }
    #[cfg(not(target_os = "macos"))]
    {
        sdk_dir.join(if cfg!(windows) { "jre/bin/java.exe" } else { "jre/bin/java" })
    }
}

pub fn jre_home(sdk_dir: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        sdk_dir.join("jre/Contents/Home")
    }
    #[cfg(not(target_os = "macos"))]
    {
        sdk_dir.join("jre")
    }
}

/// 汇总初始化状态（SetupWizard / Preflight 共用）
pub fn setup_report(sdk_dir: &std::path::Path, api_levels: &[u32]) -> SetupReport {
    let (os, arch) = host_platform();
    SetupReport {
        sdk_dir: sdk_dir.display().to_string(),
        platform: os.to_string(),
        arch: arch.to_string(),
        components: component_list(sdk_dir, api_levels),
        hypervisor: hypervisor_state(),
    }
}

/// 虚拟化检测（macOS 恒 ready；Windows 检测 Hyper-V/WHPX 或 AEHD）
pub fn hypervisor_state() -> HypervisorState {
    #[cfg(target_os = "macos")]
    {
        HypervisorState {
            platform: "macos".into(),
            ready: true,
            detail: "Hypervisor.framework 系统自带，无需任何操作".into(),
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 1. 优先检测 Google AEHD 独立驱动服务 (gvm)
        let mut sc_cmd = std::process::Command::new("sc");
        sc_cmd.args(["query", "gvm"]).creation_flags(0x08000000);
        let aehd = sc_cmd
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("RUNNING"))
            .unwrap_or(false);

        // 2. 若 AEHD 未运行，通过 PowerShell 探索 Windows 原生 Hyper-V / 虚拟化支持
        let mut ps_cmd = std::process::Command::new("powershell");
        ps_cmd
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance -ClassName Win32_ComputerSystem).HypervisorPresent",
            ])
            .creation_flags(0x08000000);
        let hyperv = ps_cmd
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("True"))
            .unwrap_or(false);

        // 3. 汇总状态输出
        HypervisorState {
            platform: "windows".into(),
            ready: aehd || hyperv,
            detail: if aehd {
                "AEHD 驱动已安装并运行".into()
            } else if hyperv {
                "Hyper-V 已启用，将走 WHPX 加速".into()
            } else {
                "未检测到虚拟化支持，需要安装 AEHD（一次性，需管理员授权）".into()
            },
        }
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let kvm = std::path::Path::new("/dev/kvm").exists();
        HypervisorState {
            platform: "linux".into(),
            ready: kvm,
            detail: if kvm { "/dev/kvm 可用".into() } else { "未检测到 /dev/kvm".into() },
        }
    }
}
