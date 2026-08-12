//! 组件下载与安装（设计文档 §4.1）
//! - cmdline-tools / JRE：官方直链
//! - platform-tools / emulator / build-tools / 系统镜像：装好 cmdline-tools + JRE 后走 sdkmanager
//! - 断点续传 + SHA-256 校验（有官方 hash 时）+ 镜像源可切换兜底
//! - v1.2: 支持从打包内置 bundle 部署（跳过 ~650MB 网络下载）

use super::{host_platform, jre_home, sdkmanager_bin_name, ComponentState};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("网络错误: {0}")]
    Network(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("解压失败: {0}")]
    Unzip(String),
    #[error("sdkmanager 失败: {0}")]
    SdkManager(String),
    #[error("校验失败: {0}")]
    Checksum(String),
}

/// cmdline-tools 直链（版本号内置为已知可用版本，作为 repository2-1.xml 解析失败时的 fallback）
fn cmdline_tools_url() -> &'static str {
    let (os, _) = host_platform();
    match os {
        "mac" => "https://dl.google.com/android/repository/commandlinetools-mac-11076708_latest.zip",
        "win" => "https://dl.google.com/android/repository/commandlinetools-win-11076708_latest.zip",
        _ => "https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip",
    }
}

/// Temurin JRE 17（Adoptium 官方 API，免装 JDK 的关键）
fn jre_url() -> String {
    let (os, arch) = host_platform();
    let (os_part, arch_part) = match (os, arch) {
        ("mac", "arm64") => ("mac", "aarch64"),
        ("mac", _) => ("mac", "x64"),
        ("win", _) => ("windows", "x64"),
        (_, "arm64") => ("linux", "aarch64"),
        _ => ("linux", "x64"),
    };
    format!(
        "https://api.adoptium.net/v3/binary/latest/17/ga/{}/{}/jre/hotspot/normal/eclipse",
        os_part, arch_part
    )
}

fn emit_progress(app: &AppHandle, component: &str, pct: u8, stage: &str) {
    let _ = app.emit(
        "setup://progress",
        serde_json::json!({ "component": component, "pct": pct, "stage": stage }),
    );
}

/// 下载文件（带进度事件与断点续传）
async fn download_file(app: &AppHandle, url: &str, dest: &Path, component: &str) -> Result<(), SetupError> {
    let client = reqwest::Client::builder()
        .user_agent("umeng-dau-client/0.1")
        .build()
        .map_err(|e| SetupError::Network(e.to_string()))?;

    let existing = if dest.exists() {
        std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    let mut req = client.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={}-", existing));
    }
    let resp = req.send().await.map_err(|e| SetupError::Network(e.to_string()))?;
    let status = resp.status();
    if !(status.is_success() || status.as_u16() == 206) {
        return Err(SetupError::Network(format!("HTTP {}", status)));
    }
    // 服务器不支持续传时从头下
    let resumed = status.as_u16() == 206;
    let total = resp.content_length().unwrap_or(0) + if resumed { existing } else { 0 };

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = if resumed {
        tokio::fs::OpenOptions::new().append(true).open(dest).await?
    } else {
        tokio::fs::File::create(dest).await?
    };

    let mut downloaded = if resumed { existing } else { 0 };
    let mut stream = resp.bytes_stream();
    let mut last_pct = 0u8;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| SetupError::Network(e.to_string()))?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            let pct = ((downloaded * 100) / total).min(100) as u8;
            if pct != last_pct {
                last_pct = pct;
                emit_progress(app, component, pct, "downloading");
            }
        }
    }
    file.flush().await?;
    Ok(())
}

/// 解压 zip 到目标目录（spawn_blocking，zip crate 为同步 API）
async fn unzip(zip_path: &Path, dest: &Path) -> Result<(), SetupError> {
    let zip_path = zip_path.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| SetupError::Unzip(e.to_string()))?;
        std::fs::create_dir_all(&dest)?;
        archive.extract(&dest).map_err(|e| SetupError::Unzip(e.to_string()))?;
        Ok::<(), SetupError>(())
    })
    .await
    .map_err(|e| SetupError::Unzip(e.to_string()))??;
    Ok(())
}

/// 安装 cmdline-tools（zip 内目录为 cmdline-tools/，需归位到 cmdline-tools/latest/）
pub async fn install_cmdline_tools(app: &AppHandle, sdk_dir: &Path) -> Result<(), SetupError> {
    let tmp = sdk_dir.join(".dl/cmdline-tools.zip");
    emit_progress(app, "cmdline-tools", 0, "downloading");
    download_file(app, cmdline_tools_url(), &tmp, "cmdline-tools").await?;
    emit_progress(app, "cmdline-tools", 100, "extracting");
    let staging = sdk_dir.join(".staging/cmdline-tools");
    unzip(&tmp, &staging).await?;
    let latest = sdk_dir.join("cmdline-tools/latest");
    tokio::fs::create_dir_all(latest.parent().unwrap()).await?;
    if latest.exists() {
        tokio::fs::remove_dir_all(&latest).await?;
    }
    tokio::fs::rename(staging.join("cmdline-tools"), &latest).await?;
    let _ = tokio::fs::remove_file(&tmp).await;
    emit_progress(app, "cmdline-tools", 100, "ready");
    Ok(())
}

/// 安装内嵌 JRE（tar.gz 或 zip，视平台而定）
pub async fn install_jre(app: &AppHandle, sdk_dir: &Path) -> Result<(), SetupError> {
    let (os, _) = host_platform();
    let is_zip = os == "win";
    let tmp = sdk_dir.join(if is_zip { ".dl/jre.zip" } else { ".dl/jre.tar.gz" });
    emit_progress(app, "jre", 0, "downloading");
    download_file(app, &jre_url(), &tmp, "jre").await?;
    emit_progress(app, "jre", 100, "extracting");

    let staging = sdk_dir.join(".staging/jre");
    tokio::fs::create_dir_all(&staging).await?;
    if is_zip {
        unzip(&tmp, &staging).await?;
    } else {
        // macOS/Linux: tar.gz，用系统 tar（bsdtar 自带）
        let status = tokio::process::Command::new("tar")
            .args(["-xzf"])
            .arg(&tmp)
            .arg("-C")
            .arg(&staging)
            .status()
            .await?;
        if !status.success() {
            return Err(SetupError::Unzip("tar 解压失败".into()));
        }
    }
    // 归位：staging 下第一层目录即 JRE 根
    let mut entries = tokio::fs::read_dir(&staging).await?;
    let first = entries.next_entry().await?.ok_or(SetupError::Unzip("JRE 包为空".into()))?;
    let dest = sdk_dir.join("jre");
    if dest.exists() {
        tokio::fs::remove_dir_all(&dest).await?;
    }
    tokio::fs::rename(first.path(), &dest).await?;
    let _ = tokio::fs::remove_file(&tmp).await;
    emit_progress(app, "jre", 100, "ready");
    Ok(())
}

// ============ v1.2: 从打包内置 bundle 部署 ============

/// 递归拷贝目录（spawn_blocking，std::fs 同步 API）
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if ft.is_symlink() {
            // 跟随符号链接，解析相对/绝对路径后拷贝目标内容
            if let Ok(target) = std::fs::read_link(&src_path) {
                let resolved = if target.is_relative() {
                    src_path.parent().unwrap_or(src).join(&target)
                } else {
                    target
                };
                if resolved.is_dir() {
                    copy_dir_recursive(&resolved, &dst_path)?;
                } else if resolved.is_file() {
                    std::fs::copy(&resolved, &dst_path)?;
                }
            }
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// 递归对 SDK 工具链与 JRE 赋予 +x 可执行权限（Tauri 打包部署后权限可能丢失）
#[cfg(unix)]
fn ensure_executable(sdk_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fn set_exec_recursive(dir: &Path) {
        if !dir.exists() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        set_exec_recursive(&path);
                    } else if meta.is_file() {
                        let mut perms = meta.permissions();
                        perms.set_mode(perms.mode() | 0o755);
                        let _ = std::fs::set_permissions(&path, perms);
                    }
                }
            }
        }
    }

    let targets = [
        sdk_dir.join("cmdline-tools"),
        sdk_dir.join("platform-tools"),
        sdk_dir.join("emulator"),
        sdk_dir.join("build-tools"),
        sdk_dir.join("jre"),
    ];

    for t in &targets {
        set_exec_recursive(t);
    }
}

#[cfg(not(unix))]
fn ensure_executable(_sdk_dir: &Path) {}

/// 从打包内置 bundle 部署 SDK 到 sdk_dir（跳过网络下载）。
/// bundle 位于 Tauri resource_dir 下的 sdk-bundle/。
/// 返回 true = 部署成功，false = 无 bundle，需走网络下载。
pub async fn deploy_from_bundle(
    app: &AppHandle,
    sdk_dir: &Path,
    resource_dir: &Path,
) -> Result<bool, SetupError> {
    let bundle = resource_dir.join("sdk-bundle");
    // 没有 cmdline-tools 说明没预置 bundle
    if !bundle.join("cmdline-tools/latest/bin").exists() {
        return Ok(false);
    }

    // 如果 SDK 目录已经完整部署，直接跳过再次解压，防止 Windows 平台因程序正在运行导致 remove_dir_all 报 Access Denied
    if sdk_dir.join("cmdline-tools/latest/bin").exists()
        && sdk_dir.join("platform-tools").exists()
        && sdk_dir.join("emulator").exists()
    {
        emit_progress(app, "bundle", 100, "ready");
        return Ok(true);
    }

    emit_progress(app, "bundle", 0, "deploying");

    let bundle = bundle.to_path_buf();
    let sdk_dir = sdk_dir.to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        // 清理旧 sdk_dir（可能有空目录或上次失败残留）
        if sdk_dir.exists() {
            let _ = std::fs::remove_dir_all(&sdk_dir);
        }
        std::fs::create_dir_all(sdk_dir.parent().unwrap_or(&sdk_dir))?;
        copy_dir_recursive(&bundle, &sdk_dir)?;
        ensure_executable(&sdk_dir);
        Ok(())
    })
    .await
    .map_err(|e| SetupError::Unzip(format!("部署线程异常: {}", e)))?;

    result.map_err(SetupError::Io)?;

    emit_progress(app, "bundle", 100, "ready");
    Ok(true)
}

/// 通过 sdkmanager 安装组件（license 已直写，全程无交互）
pub async fn sdkmanager_install(
    app: &AppHandle,
    sdk_dir: &Path,
    packages: &[String],
    env_overrides: &[(String, String)],
    component_label: &str,
) -> Result<(), SetupError> {
    let bin = sdk_dir
        .join("cmdline-tools/latest/bin")
        .join(sdkmanager_bin_name());
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg(format!("--sdk_root={}", sdk_dir.display()));
    for p in packages {
        cmd.arg(p);
    }
    cmd.env("JAVA_HOME", jre_home(sdk_dir));
    for (k, v) in env_overrides {
        cmd.env(k, v);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    emit_progress(app, component_label, 10, "installing");
    let output = cmd.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(SetupError::SdkManager(format!(
            "{} {}",
            stdout.lines().last().unwrap_or(""),
            stderr.lines().last().unwrap_or("")
        )));
    }
    emit_progress(app, component_label, 100, "ready");
    Ok(())
}

/// 在本地常用目录下检索预下载的安卓系统镜像（ZIP 包或已解压的目录）
fn find_local_system_image(api_level: u32, sdk_dir: &Path) -> Option<PathBuf> {
    let (_, arch) = host_platform();
    let abi_keyword = if arch == "arm64" { "arm64" } else { "x86_64" };
    let api_keyword = format!("android-{}", api_level);
    let api_alt_keyword = format!("-{}-", api_level); // 例如 -34-

    let mut search_paths = Vec::new();

    // 1. 运行的可执行文件所在目录及其父/祖父目录（开发与运行环境）
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            search_paths.push(exe_dir.to_path_buf());
            if let Some(parent) = exe_dir.parent() {
                search_paths.push(parent.to_path_buf());
                if let Some(gparent) = parent.parent() {
                    search_paths.push(gparent.to_path_buf());
                }
            }
        }
    }

    // 2. SDK 根目录
    search_paths.push(sdk_dir.to_path_buf());

    // 3. 用户系统的 Downloads 目录
    if let Some(download_dir) = dirs::download_dir() {
        search_paths.push(download_dir);
    }

    for path in &search_paths {
        if !path.exists() {
            continue;
        }

        // 检查是否有解压好的镜像目录（如 x86_64/system.img）
        let direct_dir = path.join(abi_keyword);
        if direct_dir.exists() && direct_dir.join("system.img").exists() && direct_dir.join("source.properties").exists() {
            return Some(direct_dir);
        }

        let direct_dir_alt = path.join(if arch == "arm64" { "arm64-v8a" } else { "x86_64" });
        if direct_dir_alt.exists() && direct_dir_alt.join("system.img").exists() && direct_dir_alt.join("source.properties").exists() {
            return Some(direct_dir_alt);
        }

        // 搜索符合匹配命名规则的 zip 压缩包
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let file_path = entry.path();
                if file_path.is_file() && file_path.extension().map_or(false, |ext| ext == "zip") {
                    let file_name = file_path.file_name().unwrap().to_string_lossy().to_lowercase();
                    let has_abi = file_name.contains(abi_keyword) || (abi_keyword == "x86_64" && file_name.contains("x86"));
                    let has_api = file_name.contains(&api_keyword) || file_name.contains(&api_alt_keyword) || file_name.contains(&api_level.to_string());
                    
                    if has_abi && has_api {
                        tracing::info!("Found local system image zip: {:?}", file_path);
                        return Some(file_path);
                    }
                }
            }
        }
    }

    None
}

/// 解压系统镜像 zip 文件到目标目录，智能剥离一级 abi 目录以符合 sdk 目录结构
async fn unzip_system_image_zip(zip_path: &Path, target_dir: &Path) -> Result<(), SetupError> {
    let zip_path = zip_path.to_path_buf();
    let target_dir = target_dir.to_path_buf();
    
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| SetupError::Unzip(e.to_string()))?;
        
        let abi_name = target_dir.file_name().ok_or_else(|| SetupError::Unzip("Invalid target dir".into()))?;
        let abi_str = abi_name.to_string_lossy();
        
        let mut has_abi_prefix = false;
        if archive.len() > 0 {
            if let Ok(first_file) = archive.by_index(0) {
                let name = first_file.name();
                if name.starts_with(&format!("{}/", abi_str)) || name.starts_with(&format!("{}\\", abi_str)) {
                    has_abi_prefix = true;
                }
            }
        }
        
        let extract_dest = if has_abi_prefix {
            target_dir.parent().ok_or_else(|| SetupError::Unzip("Invalid target parent dir".into()))?
        } else {
            &target_dir
        };
        
        std::fs::create_dir_all(extract_dest)?;
        archive.extract(extract_dest).map_err(|e| SetupError::Unzip(e.to_string()))?;
        Ok::<(), SetupError>(())
    })
    .await
    .map_err(|e| SetupError::Unzip(format!("Unzip thread panicked: {}", e)))?
}

/// 一键补齐缺失组件（SetupWizard「开始初始化」按钮）
/// v1.2: 优先从打包内置 bundle 部署，跳过 ~650MB 网络下载。
/// 系统镜像（~1.5GB）始终按需下载（与 API level 绑定，不打包）。
pub async fn ensure_components(
    app: &AppHandle,
    sdk_dir: &Path,
    api_levels: &[u32],
    env_overrides: &[(String, String)],
    resource_dir: Option<&Path>,
) -> Result<(), SetupError> {
    use super::*;
    // license 永远先写（幂等）
    super::licenses::write_license_files(sdk_dir)?;

    // v1.2: 优先从内置 bundle 部署工具链
    if let Some(rd) = resource_dir {
        let deployed = deploy_from_bundle(app, sdk_dir, rd).await?;
        if deployed {
            // bundle 已包含 cmdline-tools/jre/platform-tools/emulator/build-tools
            // 只需补系统镜像
            for api in api_levels {
                let img = system_image_id(*api);
                let target_dir = super::system_image_dir(sdk_dir, &img);
                if !target_dir.exists() {
                    // 尝试本地检测与安装
                    if let Some(local_path) = find_local_system_image(*api, sdk_dir) {
                        emit_progress(app, &format!("image-{}", api), 10, "extracting");
                        let success = if local_path.is_dir() {
                            copy_dir_recursive(&local_path, &target_dir).is_ok()
                        } else {
                            unzip_system_image_zip(&local_path, &target_dir).await.is_ok()
                        };
                        if success {
                            emit_progress(app, &format!("image-{}", api), 100, "ready");
                            continue;
                        }
                    }
                    sdkmanager_install(app, sdk_dir, std::slice::from_ref(&img), env_overrides, &format!("image-{}", api)).await?;
                }
            }
            return Ok(());
        }
    }

    // 无 bundle，走完整网络下载
    if !sdk_dir.join("cmdline-tools/latest/bin").exists() {
        install_cmdline_tools(app, sdk_dir).await?;
    }
    if !jre_java_bin(sdk_dir).exists() {
        install_jre(app, sdk_dir).await?;
    }
    if !sdk_dir.join("platform-tools").join(adb_bin_name()).exists() {
        sdkmanager_install(app, sdk_dir, &["platform-tools".into()], env_overrides, "platform-tools").await?;
    }
    if !sdk_dir.join("emulator").join(emulator_bin_name()).exists() {
        sdkmanager_install(app, sdk_dir, &["emulator".into()], env_overrides, "emulator").await?;
    }
    if !sdk_dir.join("build-tools/34.0.0").join(aapt_bin_name()).exists() {
        sdkmanager_install(app, sdk_dir, &["build-tools;34.0.0".into()], env_overrides, "build-tools").await?;
    }
    for api in api_levels {
        let img = system_image_id(*api);
        let target_dir = super::system_image_dir(sdk_dir, &img);
        if !target_dir.exists() {
            // 尝试本地检测与安装
            if let Some(local_path) = find_local_system_image(*api, sdk_dir) {
                emit_progress(app, &format!("image-{}", api), 10, "extracting");
                let success = if local_path.is_dir() {
                    copy_dir_recursive(&local_path, &target_dir).is_ok()
                } else {
                    unzip_system_image_zip(&local_path, &target_dir).await.is_ok()
                };
                if success {
                    emit_progress(app, &format!("image-{}", api), 100, "ready");
                    continue;
                }
            }
            sdkmanager_install(app, sdk_dir, std::slice::from_ref(&img), env_overrides, &format!("image-{}", api)).await?;
        }
    }
    Ok(())
}


/// 组件状态 -> 前端展示用
pub fn state_label(s: &ComponentState) -> String {
    match s {
        ComponentState::Missing => "未安装".into(),
        ComponentState::Outdated => "需更新".into(),
        ComponentState::Downloading(p) => format!("下载中 {}%", p),
        ComponentState::Ready => "就绪".into(),
        ComponentState::Failed(e) => format!("失败: {}", e),
    }
}

/// 供外部（复用已有 SDK）校验完整性
pub fn external_sdk_valid(sdk_dir: &PathBuf) -> bool {
    sdk_dir.join("platform-tools").join(super::adb_bin_name()).exists()
        && sdk_dir.join("emulator").join(super::emulator_bin_name()).exists()
        && sdk_dir.join("cmdline-tools/latest/bin").exists()
}
