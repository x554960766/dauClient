//! Phase 0–3 流程编排（设计文档 §7，状态机）

use crate::adb::Adb;
use crate::avd::{AvdManager, BootOpts, Emulator};
use crate::engine::*;
use crate::proxy::{CountingProxy, ProxyStats};
use crate::state::SharedState;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightItem {
    pub id: String,
    pub label: String,
    pub ok: bool,
    pub detail: String,
    pub fix_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightReport {
    pub items: Vec<PreflightItem>,
    pub recommended_concurrency: u32,
    pub all_green: bool,
    /// v1.1 P1-4：跨零点预估
    pub est_finish: String,
    pub crosses_midnight: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkInfo {
    pub pkg: String,
    #[serde(default)]
    pub app_label: String,
    pub appkey: String,
    pub debuggable: bool,
    pub min_sdk: String,
    pub appkey_matches_declared: Option<bool>,
    pub blacklist_hit: bool,
    /// v1.2 新增：APK 声明的全部权限（供 prepare 阶段 pm grant，§11.1）
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate1Answers {
    pub backend_received: bool,
    pub backend_device_count_increased: bool,
    pub observed_device_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate1Verdict {
    pub passed: bool,
    pub branch: String,
    pub message: String,
}

fn emit_state(app: &AppHandle, topic: &str, payload: serde_json::Value) {
    let _ = app.emit(topic, payload);
}

// ---------------- Phase 0：预检 ----------------

pub async fn preflight_check(state: &SharedState, cfg: &EngineConfig) -> PreflightReport {
    let sdk = &state.sdk_dir;
    let mut items = Vec::new();

    // 1. 工具链四件套 + JRE
    let tools_ok = sdk.join("platform-tools").join(crate::sdkmgr::adb_bin_name()).exists()
        && sdk.join("emulator").join(crate::sdkmgr::emulator_bin_name()).exists()
        && sdk.join("cmdline-tools/latest/bin").exists()
        && crate::sdkmgr::jre_java_bin(sdk).exists();
    items.push(PreflightItem {
        id: "toolchain".into(),
        label: "工具链四件套 + 内嵌 JRE".into(),
        ok: tools_ok,
        detail: if tools_ok { "adb / emulator / cmdline-tools / JRE 均已就绪".into() } else { "组件缺失".into() },
        fix_hint: "回到初始化向导完成下载".into(),
    });

    // 2. 系统镜像架构匹配
    let img_ok = crate::sdkmgr::system_image_dir(sdk, &cfg.system_image).exists();
    items.push(PreflightItem {
        id: "image".into(),
        label: "系统镜像（架构匹配宿主机）".into(),
        ok: img_ok,
        detail: cfg.system_image.clone(),
        fix_hint: "在初始化向导中勾选对应 API 级别".into(),
    });

    // 3. license
    let lic_ok = crate::sdkmgr::licenses::licenses_ok(sdk);
    items.push(PreflightItem {
        id: "license".into(),
        label: "SDK license 已落盘".into(),
        ok: lic_ok,
        detail: "licenses/ hash 文件".into(),
        fix_hint: "客户端初始化时自动写入，勿手工删除".into(),
    });

    // 4. 私有 adb server 探活（v1.1：不碰用户全局 server）
    let adb = Adb::new(sdk, state.adb_server_port);
    let adb_ok = adb.start_server().await.is_ok() && adb.devices().await.is_ok();
    items.push(PreflightItem {
        id: "adb-server".into(),
        label: format!("私有 adb server（端口 {}）", state.adb_server_port),
        ok: adb_ok,
        detail: "与用户 Android Studio 完全并存，互不影响".into(),
        fix_hint: "检查平台工具是否完整".into(),
    });

    // 5. 磁盘/内存/CPU 余量 → 智能推荐并发（跨平台：macOS + Windows）
    let mem_gb = physical_memory_gb();
    let cores = cpu_cores();
    let recommended = if mem_gb >= 64 && cores >= 16 {
        8
    } else if mem_gb >= 32 && cores >= 10 {
        6
    } else if mem_gb >= 16 && cores >= 6 {
        4
    } else if mem_gb >= 12 && cores >= 4 {
        3
    } else if mem_gb >= 8 {
        2
    } else {
        1
    }.min(8);
    let disk_ok = disk_free_gb(&state.runs_dir) >= 10;
    items.push(PreflightItem {
        id: "resources".into(),
        label: "资源余量".into(),
        ok: disk_ok,
        detail: format!("内存 {}GB, CPU {}核，推荐并发 {}", mem_gb, cores, recommended),
        fix_hint: "可用磁盘 < 10GB 时清理空间".into(),
    });

    // 6. APK 校验
    let apk_ok = !cfg.apk_path.is_empty() && Path::new(&cfg.apk_path).exists();
    items.push(PreflightItem {
        id: "apk".into(),
        label: "测试 APK".into(),
        ok: apk_ok,
        detail: cfg.apk_path.clone(),
        fix_hint: "拖入模拟器架构的测试包".into(),
    });

    // 7. Windows 虚拟化
    let hv = crate::sdkmgr::hypervisor_state();
    items.push(PreflightItem {
        id: "hypervisor".into(),
        label: "虚拟化支持".into(),
        ok: hv.ready,
        detail: hv.detail.clone(),
        fix_hint: "Windows：引导安装 AEHD 或启用 Hyper-V".into(),
    });

    let (est_finish, crosses) = estimate_finish(cfg);
    let all_green = items.iter().all(|i| i.ok);
    PreflightReport { items, recommended_concurrency: recommended, all_green, est_finish, crosses_midnight: crosses }
}

fn cpu_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn physical_memory_gb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl").args(["-n", "hw.memsize"]).output() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                if let Ok(bytes) = s.trim().parse::<u64>() {
                    return bytes / 1024 / 1024 / 1024;
                }
            }
        }
        0
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 优先使用 wmic，若失败（如 Win11 22H2+ 已默认移除 wmic）则使用 PowerShell 读取 CIM
        if let Ok(out) = std::process::Command::new("wmic")
            .args(["computersystem", "get", "TotalPhysicalMemory", "/value"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(eq) = s.find('=') {
                if let Ok(bytes) = s[eq + 1..].trim().parse::<u64>() {
                    let gb = bytes / 1024 / 1024 / 1024;
                    if gb > 0 {
                        return gb;
                    }
                }
            }
        }
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            "[math]::Floor((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB)",
        ]);
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        if let Ok(out) = cmd.output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(gb) = s.parse::<u64>() {
                if gb > 0 {
                    return gb;
                }
            }
        }
        0
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            for line in meminfo.lines() {
                if line.starts_with("MemTotal:") {
                    if let Some(kb) = line.split_whitespace().nth(1).and_then(|s| s.parse::<u64>().ok()) {
                        return kb / 1024 / 1024;
                    }
                }
            }
        }
        0
    }
}

fn disk_free_gb(path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        // 路径不存在时 statvfs 失败，递归尝试 parent（避免「runs/ 还没建」就误判）
        let mut current: &Path = path;
        loop {
            let c_path = std::ffi::CString::new(current.as_os_str().as_bytes()).unwrap_or_default();
            unsafe {
                let mut stat: libc::statvfs = std::mem::zeroed();
                if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                    return (stat.f_bavail as u64 * stat.f_frsize as u64) / 1024 / 1024 / 1024;
                }
            }
            match current.parent() {
                Some(p) if p != current => current = p,
                _ => break,
            }
        }
        0
    }
    #[cfg(windows)]
    {
        let _ = path;
        999 // 简化：Windows 侧由 SetupWizard 明示磁盘占用
    }
}

// ---------------- APK 解析（aapt） ----------------

pub async fn inspect_apk(state: &SharedState, apk_path: &str, declared_appkey: &str) -> Result<ApkInfo, String> {
    let aapt = state.sdk_dir.join("build-tools/34.0.0").join(crate::sdkmgr::aapt_bin_name());
    let out = tokio::process::Command::new(&aapt)
        .args(["dump", "xmltree", apk_path, "AndroidManifest.xml"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    let pkg = extract_xml_attr(&text, "package").unwrap_or_default();
    let appkey = find_line_value(&text, "UMENG_APPKEY").unwrap_or_default();
    let debuggable = text.contains("android:debuggable") && text.contains("0xffffffff");
    let min_sdk = find_line_value(&text, "minSdkVersion").unwrap_or_default();

    let matches = if declared_appkey.is_empty() {
        None
    } else {
        Some(appkey == declared_appkey)
    };

    // 可选：生产 AppKey 黑名单（settings.json 中维护）
    let blacklist_hit = check_blacklist(&state.settings_file, &appkey).await;

    // 提取 App 显示名称（通过 aapt dump badging 解析资源表，能拿到 @string 引用的明文）
    let app_label = extract_app_label(&state.sdk_dir, apk_path).await;
    let permissions = match tokio::process::Command::new(&aapt)
        .args(["dump", "permissions", apk_path])
        .output()
        .await
    {
        Ok(o) => crate::uiautomation::prepare::extract_permissions(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    };

    Ok(ApkInfo { pkg, app_label, appkey, debuggable, min_sdk, appkey_matches_declared: matches, blacklist_hit, permissions })
}

/// 用 `aapt dump badging` 从 APK 解析 App 显示名称（资源表级，能拿到 @string 引用的明文）。
/// 优先中文本地化 label（设备为中文环境），否则退回通用 application-label / application: label。
/// 供 inspect_apk 与启动链路的图标精确匹配复用——替代设备端 dumpsys 猜测，避免落入
/// 「Predicted app:」模糊探测与翻页/抽屉搜索。
pub async fn extract_app_label(sdk_dir: &std::path::Path, apk_path: &str) -> String {
    let aapt = sdk_dir.join("build-tools/34.0.0").join(crate::sdkmgr::aapt_bin_name());
    let out = match tokio::process::Command::new(&aapt)
        .args(["dump", "badging", apk_path])
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return String::new(),
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut zh_label = String::new();
    let mut generic_label = String::new();
    let mut app_attr_label = String::new();
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with("application-label-zh-CN:") || t.starts_with("application-label-zh:") {
            if let Some(val) = t.split('\'').nth(1) {
                if !val.is_empty() && zh_label.is_empty() {
                    zh_label = val.to_string();
                }
            }
        } else if t.starts_with("application-label:") {
            if let Some(val) = t.split('\'').nth(1) {
                if !val.is_empty() && generic_label.is_empty() {
                    generic_label = val.to_string();
                }
            }
        } else if t.starts_with("application:") && t.contains("label='") {
            if let Some(pos) = t.find("label='") {
                let rest = &t[pos + 7..];
                if let Some(end) = rest.find('\'') {
                    app_attr_label = rest[..end].to_string();
                }
            }
        }
    }
    if !zh_label.is_empty() { zh_label } else if !generic_label.is_empty() { generic_label } else { app_attr_label }
}

fn extract_xml_attr(text: &str, attr: &str) -> Option<String> {
    // aapt xmltree 输出形如：A: package="com.xxx.app" (Raw: "com.xxx.app")
    for line in text.lines() {
        if line.contains(&format!("{}=\"", attr)) || line.contains(&format!("{} (", attr)) {
            if let Some(raw) = line.split("Raw: \"").nth(1) {
                return raw.split('"').next().map(|s| s.to_string());
            }
        }
    }
    None
}

fn find_line_value(text: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.contains(key) {
            // android:value 通常在下一两行
            for next in lines.iter().skip(i).take(3) {
                if next.contains("android:value") || next.contains("Raw:") {
                    if let Some(raw) = next.split("Raw: \"").nth(1) {
                        return raw.split('"').next().map(|s| s.to_string());
                    }
                }
            }
        }
    }
    None
}

async fn check_blacklist(settings_file: &Path, appkey: &str) -> bool {
    if appkey.is_empty() {
        return false;
    }
    if let Ok(content) = tokio::fs::read_to_string(settings_file).await {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(list) = json.get("prod_appkey_blacklist").and_then(|v| v.as_array()) {
                return list.iter().any(|k| k.as_str() == Some(appkey));
            }
        }
    }
    false
}

// ---------------- Phase 1：试点 ----------------

pub async fn pilot_run(app: AppHandle, state: SharedState, mut cfg: EngineConfig) -> Result<(), String> {
    tracing::info!("[Pilot] ===== 试点运行触发 (pilot_run) =====");
    cfg.count = 1;
    cfg.concurrency = 1;
    let sdk = state.sdk_dir.clone();
    // 启动提速：若未传 App 显示名称，从 APK 用 aapt 解析一次供图标精确匹配
    if cfg.app_label.is_empty() {
        cfg.app_label = extract_app_label(&sdk, &cfg.apk_path).await;
    }
    let adb = Adb::new(&sdk, state.adb_server_port);
    let avdm = AvdManager::new(&sdk, adb.env().clone());
    let emulator = Emulator::new(&sdk, adb.env().clone());

    let port: u16 = 5554;
    let serial = format!("emulator-{}", port);
    tracing::info!("[Pilot] 清理旧端口与残留 AVD: serial={}", serial);
    let _ = adb.emu_kill(&serial).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let old_avd = format!("dau-pilot-{}", std::process::id());
    let _ = avdm.delete(&old_avd).await;

    emit_state(&app, "pilot://state", serde_json::json!({"state": "CreatingPilot"}));

    // 启动内嵌代理
    if let Some(token) = state.proxy_shutdown.lock().await.take() {
        token.cancel();
    }
    tracing::info!("[Pilot] 启动内嵌代理...");
    let proxy = CountingProxy::start().await.map_err(|e| e.to_string())?;
    let proxy_addr: std::net::SocketAddr = format!("127.0.0.1:{}", proxy.port).parse().unwrap();
    *state.proxy_port.lock().await = Some(proxy.port);
    *state.proxy_stats.lock().await = Some(proxy.stats.clone());
    *state.proxy_shutdown.lock().await = Some(proxy.shutdown.clone());

    let avd_name = old_avd; // 复用变量名
    let run_dir = state.runs_dir.join(format!("pilot-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S")));
    let registry = AvdRegistry::new(&run_dir.join("avds.json"));

    let result: Result<(), String> = async {
        tracing::info!("[Pilot] 开始创建试点 AVD: name={}", avd_name);
        avdm.create(&avd_name, &cfg.system_image, &cfg.device_profile)
            .await
            .map_err(|e| e.to_string())?;
        registry.register(&avd_name, port).await;

        emit_state(&app, "pilot://state", serde_json::json!({"state": "Booting"}));
        tracing::info!("[Pilot] 开始启动 QEMU 模拟器 (首次 Boot)...");
        let opts = BootOpts {
            wipe: true,
            mem_mb: Some(cfg.emu_mem_mb),
            http_proxy: if cfg.use_proxy { Some(proxy_addr) } else { None },
            max_users: cfg.max_users,
            props: crate::avd::device_props_for_avd(&avd_name),
        };
        emulator.boot(&avd_name, port, &opts).await.map_err(|e| e.to_string())?;
        tracing::info!("[Pilot] 等待 QEMU 模拟器开机 (sys.boot_completed=1)...");
        let timeout_s = (cfg.boot_timeout_s as u64).max(120);
        adb.wait_boot(&serial, Duration::from_secs(timeout_s)).await.map_err(|e| e.to_string())?;

        tracing::info!("[Pilot] 模拟器开机完成，准备进行设备型号与指纹伪装...");
        // 动态覆盖真机设备型号（修改 build.prop 并重载）
        crate::uiautomation::prepare::apply_device_spoofing(&adb, &emulator, &avd_name, port, &opts).await;

        emit_state(&app, "pilot://state", serde_json::json!({"state": "NetWaiting"}));
        adb.wait_net(&serial).await.map_err(|e| e.to_string())?;

        emit_state(&app, "pilot://state", serde_json::json!({"state": "Installing"}));
        adb.install(&serial, Path::new(&cfg.apk_path), None).await.map_err(|e| e.to_string())?;
        // install 后 PackageManager 需要时间注册 activity，立即 monkey 会 exit 251
        tokio::time::sleep(Duration::from_secs(2)).await;

        // v1.2：prepare_device（解锁屏 / 关全局动画 / pm grant 预授权）
        if cfg.ui.enabled {
            cfg.runs_dir = run_dir.display().to_string();
            let mut prep = crate::uiautomation::prepare::prepare_device(
                &adb, &serial, None, &cfg.ui.prepare,
            )
            .await
            .map_err(|e| e.to_string())?;
            crate::uiautomation::prepare::grant_permissions(
                &adb, &serial, &cfg.pkg, &cfg.runtime_permissions, None, &mut prep,
            )
            .await;
            emit_state(&app, "pilot://evidence", serde_json::json!({
                "prepare": prep,
            }));
        }

        // logcat 辅助流
        let mut logcat = crate::logcat::LogcatStream::clear_and_spawn(app.clone(), adb.env(), &serial)
            .await
            .map_err(|e| e.to_string())?;

        emit_state(&app, "pilot://state", serde_json::json!({"state": "Launching"}));
        adb.launch_app(&serial, &cfg.pkg, None, &cfg.app_label).await.map_err(|e| e.to_string())?;
        tokio::time::sleep(Duration::from_secs(cfg.dwell_s as u64)).await;

        // v1.2：onboarding 多层引导循环（协议 → 引导页 → 功能指引 → 主页）
        if cfg.ui.enabled && cfg.ui.onboarding.enabled {
            emit_state(&app, "pilot://state", serde_json::json!({"state": "Onboarding"}));
            let screen = crate::uiautomation::interactor::Interactor::fetch_screen(&adb, &serial)
                .await
                .map_err(|e| e.to_string())?;
            let runner = crate::uiautomation::OnboardingRunner {
                adb: adb.clone(),
                serial: serial.clone(),
                pkg: cfg.pkg.clone(),
                cfg: cfg.ui.onboarding.clone(),
                screen,
                dump_dir: Some(run_dir.join("dumps")),
            };
            let onb = runner.run().await.map_err(|e| e.to_string())?;
            emit_state(&app, "pilot://evidence", serde_json::json!({
                "onboarding": onb,
            }));
            if !onb.reached_home {
                return Err(format!(
                    "onboarding 未到主页: {:?} (round {:?}) | rounds={} 用时{:.0}s \
                     | cling×{} 权限弹窗×{} 引导滑动×{} 跳过×{} 协议={} 引导进入={} 覆盖层={} 全量dump救回×{}\n\
                     失败现场已保存: {}",
                    onb.fail_reason,
                    onb.failed_at_round,
                    onb.rounds_used,
                    onb.duration_s,
                    onb.system_cling_dismissed,
                    onb.permission_dialogs_seen,
                    onb.guide_swipes,
                    onb.guide_skip_count,
                    onb.privacy_agreed,
                    onb.guide_entered,
                    onb.overlay_skipped,
                    onb.full_dump_rescues,
                    run_dir.join("dumps").display(),
                ));
            }
        }

        let _ = adb.key_home(&serial).await;
        tokio::time::sleep(Duration::from_secs(cfg.flush_dwell_s as u64)).await;

        logcat.stop().await;

        emit_state(&app, "pilot://state", serde_json::json!({"state": "EvidenceCollect"}));
        let identity = crate::identity::extract(&adb, &serial, &cfg.pkg, None).await;
        let stats = proxy.snapshot().await;
        emit_state(&app, "pilot://evidence", serde_json::json!({
            "android_id": identity.android_id,
            "umid": identity.umid,
            "identity_method": identity.method,
            "umeng_hits": stats.umeng_hits,
            "total_connects": stats.total_connects,
            "hosts": stats.hosts,
        }));

        emit_state(&app, "pilot://state", serde_json::json!({"state": "Gate1Review"}));
        Ok(())
    }
    .await;

    // 试点设备保留运行供人工对比/重置试验；清理由 reset 试验或显式 cleanup 处理
    if let Err(e) = &result {
        registry.cleanup(&adb, &avdm).await;
        proxy.stop();
        emit_state(&app, "pilot://state", serde_json::json!({"state": "Failed", "detail": e}));
        return Err(e.clone());
    }

    // 保存试点上下文供后续重置试验/清理
    save_pilot_context(&state, &avd_name, port, &run_dir).await;
    Ok(())
}

async fn save_pilot_context(state: &SharedState, avd: &str, port: u16, run_dir: &Path) {
    let ctx = serde_json::json!({
        "avd": avd,
        "port": port,
        "run_dir": run_dir.display().to_string(),
    });
    let _ = tokio::fs::write(state.settings_file.with_file_name("pilot-context.json"), ctx.to_string()).await;
}

/// 试点重置试验（标定向导逐级调用）：重置 → 重跑 → 提取新标识 → 推送对比
pub async fn pilot_reset_trial(
    app: AppHandle,
    state: SharedState,
    cfg: EngineConfig,
    level: ResetLevel,
) -> Result<crate::identity::DeviceIdentity, String> {
    let ctx_str = tokio::fs::read_to_string(state.settings_file.with_file_name("pilot-context.json"))
        .await
        .map_err(|_| "试点上下文不存在，请先运行试点".to_string())?;
    let ctx_json: serde_json::Value = serde_json::from_str(&ctx_str).map_err(|e| e.to_string())?;
    let avd = ctx_json["avd"].as_str().unwrap_or("").to_string();
    let port = ctx_json["port"].as_u64().unwrap_or(5554) as u16;
    // v1.x 修复：trial 此前从不设置 runs_dir → run_app 里 dump_dir=None → onboarding
    // 失败时一张现场都不落盘，报错只剩「fail_reason=None failed_at_round=None」无从诊断。
    // 复用试点自己的 run_dir（save_pilot_context 已写入 context），落盘到 {run_dir}/dumps/。
    let trial_run_dir = ctx_json["run_dir"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| state.runs_dir.display().to_string());
    let serial = format!("emulator-{}", port);

    let sdk = state.sdk_dir.clone();
    let adb = Adb::new(&sdk, state.adb_server_port);
    let avdm = AvdManager::new(&sdk, adb.env().clone());
    let emulator = Emulator::new(&sdk, adb.env().clone());
    let registry = AvdRegistry::new(&state.runs_dir.join("pilot-avds.json"));
    let proxy_addr = state
        .proxy_port
        .lock()
        .await
        .map(|p| format!("127.0.0.1:{}", p).parse().unwrap());

    let mut trial_cfg = cfg.clone();
    trial_cfg.reset_level = level;
    trial_cfg.runs_dir = trial_run_dir.clone();
    let ctx = SlotCtx {
        adb: adb.clone(),
        avdm,
        emulator,
        config: Arc::new(trial_cfg),
        registry: Arc::new(registry),
        proxy_addr,
        cancel: CancellationToken::new(),
    };

    // v1.x：试验也要实时日志（logcat 流），否则 L3 试验 ~100s 前端像卡死
    let mut logcat = crate::logcat::LogcatStream::clear_and_spawn(app.clone(), adb.env(), &serial)
        .await
        .map_err(|e| e.to_string())?;

    emit_state(&app, "pilot://state", serde_json::json!({"state": format!("ResetTrial-{}", level.as_str())}));

    // v1.x 修复：试验结束必须把状态切回 Gate1Review（成功）或 Failed（失败），
    // 否则前端 running 判定一直为真、试 Lx 按钮持续 disabled，UI 永久卡死。
    //
    // v1.5 修复（L2.5 试验失败后残留用户导致后续试点全废）：
    // 原实现在 run_app 失败时 ? 短路，跳过了 switch_user(0) + remove_user，
    // 模拟器停留在 L2.5 创建的用户上。后续试点跑在错误用户 → onboarding 永远到不了主页。
    // 改为：先执行 reset_device 拿到 user，再用 match 处理 run_app 的成功/失败，
    //       无论成功失败都必须清理用户（switch_user 0 + remove_user）。
    let user_result: Result<crate::identity::DeviceIdentity, String> = async {
        let user = reset_device(&ctx, &avd, port).await.map_err(|e| e.to_string())?;

        // v1.x：本次试验「重置 → 重跑」前后的代理命中增量，定位「App 到底有没有打到友盟」
        let before = match state.proxy_stats.lock().await.as_ref() {
            Some(s) => s.read().await.clone(),
            None => ProxyStats::default(),
        };

        // 用 match 而非 ? —— 确保失败路径也能拿到 user 做清理
        let app_result = run_app(&ctx, port, user, 0).await;

        let after = match state.proxy_stats.lock().await.as_ref() {
            Some(s) => s.read().await.clone(),
            None => ProxyStats::default(),
        };
        let trial_umeng_hits = after.umeng_hits.saturating_sub(before.umeng_hits);
        let trial_total_connects = after.total_connects.saturating_sub(before.total_connects);

        let identity = crate::identity::extract(&adb, &serial, &cfg.pkg, user).await;

        // v1.5：无论 run_app 成功还是失败都必须清理用户（switch_user 0 + remove_user）
        // 原代码用 ? 短路，run_app 失败时这段被跳过 → 模拟器停在 L2.5 用户上
        // → 后续试点跑在错误用户 → onboarding 永远到不了主页
        if let Some(uid) = user {
            let _ = adb.switch_user(&serial, 0).await;
            let _ = adb.wait_user_foreground(&serial, 0, Duration::from_secs(30)).await;
            let _ = adb.remove_user(&serial, uid).await;
        }

        let outcome = app_result.map_err(|e| e.to_string())?;

        emit_state(&app, "pilot://evidence", serde_json::json!({
            "trial_level": level.as_str(),
            "android_id": identity.android_id,
            "umid": identity.umid,
            "identity_method": identity.method,
            "onboarding": outcome.onboarding,
            "trial_umeng_hits": trial_umeng_hits,
            "trial_total_connects": trial_total_connects,
        }));
        Ok(identity)
    }
    .await;

    logcat.stop().await;

    match user_result {
        Ok(identity) => {
            emit_state(&app, "pilot://state", serde_json::json!({"state": "Gate1Review"}));
            Ok(identity)
        }
        Err(e) => {
            // v1.5：试验失败后确保模拟器切回 user 0（L2.5 残留用户的兜底清理）
            let _ = adb.switch_user(&serial, 0).await;
            // 带上现场路径，和正常试点失败的提示对齐，避免「报错了但不知道去哪看」
            let detail = format!("{}，失败现场已保存: {}/dumps", e, trial_run_dir);
            emit_state(&app, "pilot://state", serde_json::json!({"state": "Failed", "detail": detail}));
            Err(detail)
        }
    }
}

/// GATE 1 人工确认（沿用上游 §4 分支处置）
pub fn gate1_verdict(answers: &Gate1Answers, evidence_ok: bool) -> Gate1Verdict {
    if !evidence_ok {
        return Gate1Verdict {
            passed: false,
            branch: "no-client-evidence".into(),
            message: "客户端侧未观测到上报（代理 CONNECT 计数为 0），先排查 App 集成与网络".into(),
        };
    }
    if !answers.backend_received {
        return Gate1Verdict {
            passed: false,
            branch: "backend-filtered".into(),
            message: "后台完全没数据 → 友盟过滤模拟器流量，方案 A 整体不可用，转方案 B".into(),
        };
    }
    if !answers.backend_device_count_increased {
        return Gate1Verdict {
            passed: false,
            branch: "device-merged".into(),
            message: "有数据但设备数不涨 → 跨应用共享标识或服务端指纹合并，方案 A 不可用，转方案 B".into(),
        };
    }
    Gate1Verdict {
        passed: true,
        branch: "go".into(),
        message: "GATE 1 通过，可放量".into(),
    }
}

// ---------------- Phase 2：放量 ----------------

async fn maybe_rotate_ip(
    app: &AppHandle,
    sdk: &std::path::Path,
    cfg: &EngineConfig,
    cancel: &CancellationToken,
) {
    if !cfg.auto_rotate_ip || cancel.is_cancelled() {
        return;
    }

    emit_state(app, "batch://ip_status", serde_json::json!({
        "rotating": true,
        "message": "当前批次完成，正在通过手机 USB 飞行模式切换 IP...",
        "ip": null,
    }));

    match crate::adb::rotate_ip::rotate_ip_via_adb(
        sdk,
        cfg.rotate_ip_serial.as_deref(),
        cfg.rotate_ip_disconnect_wait_s,
        cfg.rotate_ip_reconnect_wait_s,
        Some(cancel),
    ).await {
        Ok(res) => {
            emit_state(app, "batch://ip_status", serde_json::json!({
                "rotating": false,
                "message": res.message,
                "ip": res.new_ip,
            }));
        }
        Err(err) => {
            tracing::warn!(error = %err, "自动换 IP 失败");
            emit_state(app, "batch://ip_status", serde_json::json!({
                "rotating": false,
                "message": format!("换 IP 失败: {}", err),
                "ip": null,
            }));
        }
    }
}

/// 等待后台换 IP 动作就绪（冷启动时完全并行，若换 IP 已完成则瞬间放行 0 延迟）
async fn wait_ip_ready(rx_opt: Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(mut rx) = rx_opt {
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

pub async fn batch_run(
    app: AppHandle,
    state: SharedState,
    cfg: EngineConfig,
    run_id: String,
    cancel: CancellationToken,
) -> Result<crate::report::RunResult, String> {
    let sdk = state.sdk_dir.clone();
    let run_dir = state.runs_dir.join(&run_id);
    tokio::fs::create_dir_all(&run_dir).await.map_err(|e| e.to_string())?;

    // v1.2：失败 dump/截图落盘目录
    let mut cfg = cfg;
    cfg.runs_dir = run_dir.display().to_string();

    // 启动提速：若前端未传 App 显示名称，从 APK 用 aapt 解析一次，供桌面图标精确匹配
    //（替代设备端 dumpsys 猜测，避免落入「Predicted app」模糊探测 + 翻页/抽屉搜索）。
    if cfg.app_label.is_empty() {
        cfg.app_label = extract_app_label(&state.sdk_dir, &cfg.apk_path).await;
        if !cfg.app_label.is_empty() {
            tracing::info!(app_label = %cfg.app_label, "[Batch] 已从 APK 提取 App 显示名称，用于图标精确匹配启动");
        }
    }

    // 代理（若试点已启动则复用端口，否则新起）
    let proxy = CountingProxy::start().await.map_err(|e| e.to_string())?;
    let proxy_addr: std::net::SocketAddr = format!("127.0.0.1:{}", proxy.port).parse().unwrap();

    let registry = Arc::new(AvdRegistry::new(&run_dir.join("avds.json")));
    let results = Arc::new(tokio::sync::Mutex::new(Vec::<DeviceResult>::new()));
    let cfg = Arc::new(cfg);
    let started_at = beijing_now();

    if cfg.auto_rotate_ip {
        let app_clone = app.clone();
        tokio::spawn(async move {
            if let Some(ip) = crate::adb::rotate_ip::get_current_public_ip().await {
                emit_state(&app_clone, "batch://ip_status", serde_json::json!({
                    "rotating": false,
                    "message": format!("当前出口公网 IP: {}", ip),
                    "ip": ip,
                }));
            }
        });
    }

    let replay_count = cfg.profile_replay_count;
    let retention_count = cfg.retention_pool_size;
    let new_count = if retention_count > 0 || replay_count > 0 {
        cfg.new_device_count
    } else {
        cfg.count
    };
    let total = replay_count + retention_count + new_count;

    // ---- 动态档案堆栈模式 (Profile Stack Mode) ----
    if cfg.enable_stack_mode {
        let mut stack = crate::engine::profile_stack::ProfileStackStore::load(&state.stack_file).await;
        stack.capacity = cfg.stack_capacity as usize;
        let _ = stack.check_and_auto_reset();
        let _ = stack.save(&state.stack_file).await;

        let today = crate::engine::profile_stack::ProfileStackStore::beijing_now_info().0;
        let total_count = cfg.count;
        let concurrency = cfg.concurrency.min(total_count).min(8);
        let mut handles = Vec::new();
        let mut current_ip_rx: Option<tokio::sync::watch::Receiver<bool>> = None;

        for device_idx in 1..=total_count {
            if cancel.is_cancelled() {
                break;
            }
            let slot = (device_idx - 1) % concurrency;

            let roll = crate::engine::pseudo_random_f64(device_idx as u64);
            let force_l3 = roll < cfg.l3_probability;

            let mut selected_profile = None;
            if !force_l3 {
                let mut stack_mut = crate::engine::profile_stack::ProfileStackStore::load(&state.stack_file).await;
                if let Some(prof) = stack_mut.pop_available_for_today(&today, cfg.stack_read_mode) {
                    let _ = stack_mut.save(&state.stack_file).await;
                    selected_profile = Some(prof);
                } else {
                    tracing::info!(device_idx, reset_level = %cfg.reset_level.as_str(), "堆栈中无可用的未用档案（全用或为空），执行设备重置");
                }
            }

            let app_w = app.clone();
            let cfg_w = cfg.clone();
            let results = results.clone();
            let cancel_w = cancel.clone();
            let sdk_w = sdk.clone();
            let adb_port = state.adb_server_port;
            let proxy_addr_s = if cfg.use_proxy { Some(proxy_addr) } else { None };
            let registry_s = registry.clone();
            let stack_file_s = state.stack_file.clone();
            let ip_rx = current_ip_rx.clone();

            if let Some(prof) = selected_profile {
                handles.push(tokio::spawn(async move {
                    profile_replay_worker(
                        app_w, sdk_w, adb_port, slot, device_idx, cfg_w,
                        results, registry_s, prof, proxy_addr_s, cancel_w,
                        ip_rx,
                    ).await;
                }));
            } else {
                handles.push(tokio::spawn(async move {
                    run_single_l3_and_push_stack(
                        app_w, sdk_w, adb_port, slot, device_idx, cfg_w,
                        results, registry_s, proxy_addr_s, stack_file_s, cancel_w,
                        ip_rx,
                    ).await;
                }));
            }

            tokio::time::sleep(Duration::from_secs(3)).await;

            if handles.len() >= concurrency as usize {
                // 无论个别设备执行成功还是失败，均安全等待当前批次全部完成
                for h in handles.drain(..) {
                    let _ = h.await;
                }
                // 当前批次完成！若后续还有设备需要运行，立即在后台异步启动换 IP！
                // 下一批新设备在下一轮循环中立即启动冷启动，冷启动与手机飞行模式换 IP 完全并行！
                if device_idx < total_count && !cancel.is_cancelled() && cfg.auto_rotate_ip {
                    let (tx, rx) = tokio::sync::watch::channel(false);
                    current_ip_rx = Some(rx);
                    let app_c = app.clone();
                    let sdk_c = sdk.clone();
                    let cfg_c = cfg.clone();
                    let cancel_c = cancel.clone();
                    tokio::spawn(async move {
                        maybe_rotate_ip(&app_c, &sdk_c, &cfg_c, &cancel_c).await;
                        let _ = tx.send(true);
                    });
                }
            }
        }

        for h in handles {
            let _ = h.await;
        }

        let adb2 = Adb::new(&sdk, state.adb_server_port);
        let avdm2 = AvdManager::new(&sdk, adb2.env().clone());
        registry.cleanup(&adb2, &avdm2).await;
        let stats = proxy.snapshot().await;
        proxy.stop();

        let devices = results.lock().await.clone();
        let ok = devices.iter().filter(|d| d.status == "ok").count() as u32;
        let fail = devices.iter().filter(|d| d.status == "fail").count() as u32;

        let result = crate::report::RunResult {
            run_id: run_id.clone(),
            api_level: extract_api_level(&cfg.system_image),
            reset_level: cfg.reset_level.as_str().into(),
            target: total_count,
            ok,
            fail,
            timezone_note: "所有时间戳为北京时间（UTC+8）；已启用动态堆栈防重与概率轮换".into(),
            started_at,
            finished_at: beijing_now(),
            devices,
            compliance_ack: true,
        };
        let _ = result.save(&run_dir).await;

        emit_state(&app, "batch://progress", serde_json::json!({
            "ok": ok, "fail": fail, "running": 0, "queued": 0,
            "done": true, "umeng_hits": stats.umeng_hits,
        }));

        return Ok(result);
    }

    let mut handles = Vec::new();

    // ---- Phase 0：身份档案回放（方案 A：300+ 留存场景）----
    if replay_count > 0 {
        let mut profiles = crate::engine::profile_archive::ProfileArchiveManager::load_all(&state.profiles_dir).await;
        // 随机抽取：按时间戳微秒伪随机打乱已保存的档案序列，实现每次运行随机抽取不同留存档案
        if !profiles.is_empty() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let len = profiles.len();
            for i in (1..len).rev() {
                let j = (nanos as usize + i * 17 + 31) % (i + 1);
                profiles.swap(i, j);
            }
        }
        let to_replay: Vec<_> = profiles.into_iter().take(replay_count as usize).collect();
        let concurrency_p = cfg.concurrency.min(to_replay.len() as u32).min(8);

        let mut current_ip_rx: Option<tokio::sync::watch::Receiver<bool>> = None;
        if !to_replay.is_empty() {
            for (batch_idx, chunk) in to_replay.chunks(concurrency_p as usize).enumerate() {
                for (i, prof) in chunk.iter().enumerate() {
                    let app_w = app.clone();
                    let cfg_w = cfg.clone();
                    let results = results.clone();
                    let cancel_w = cancel.clone();
                    let sdk_w = sdk.clone();
                    let adb_port = state.adb_server_port;
                    let prof = prof.clone();
                    let device_index = (batch_idx * concurrency_p as usize + i + 1) as u32;
                    let slot = i as u32;
                    let proxy_addr_p = if cfg.use_proxy { Some(proxy_addr) } else { None };
                    let registry_p = registry.clone();
                    let ip_rx = current_ip_rx.clone();

                    handles.push(tokio::spawn(async move {
                        profile_replay_worker(
                            app_w, sdk_w, adb_port, slot, device_index, cfg_w,
                            results, registry_p, prof, proxy_addr_p, cancel_w,
                            ip_rx,
                        ).await;
                    }));
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                for h in handles.drain(..) {
                    let _ = h.await;
                }
                if ((batch_idx + 1) * concurrency_p as usize) < to_replay.len() && !cancel.is_cancelled() && cfg.auto_rotate_ip {
                    let (tx, rx) = tokio::sync::watch::channel(false);
                    current_ip_rx = Some(rx);
                    let app_c = app.clone();
                    let sdk_c = sdk.clone();
                    let cfg_c = cfg.clone();
                    let cancel_c = cancel.clone();
                    tokio::spawn(async move {
                        maybe_rotate_ip(&app_c, &sdk_c, &cfg_c, &cancel_c).await;
                        let _ = tx.send(true);
                    });
                }
            }
        }
    }

    // ---- Phase A：留存设备（从设备池复用已有 AVD）----
    if retention_count > 0 {
        let mut pool = crate::engine::pool::DevicePool::load(&state.pool_file).await;

        // 池中设备不足时自动扩容
        while (pool.len() as u32) < retention_count {
            let info = crate::avd::random_device_info();
            pool.add_device(&info);
        }
        let _ = pool.save(&state.pool_file).await;

        let pool_devices = pool.acquire(retention_count);
        let concurrency_r = cfg.concurrency.min(retention_count).min(8);
        let mut current_ip_rx: Option<tokio::sync::watch::Receiver<bool>> = None;

        // 留存设备按批次串行调度（每批 concurrency_r 台并发）
        for (batch_idx, chunk) in pool_devices.chunks(concurrency_r as usize).enumerate() {
            for (i, pool_dev) in chunk.iter().enumerate() {
                let app_w = app.clone();
                let cfg_w = cfg.clone();
                let results = results.clone();
                let cancel_w = cancel.clone();
                let sdk_w = sdk.clone();
                let adb_port = state.adb_server_port;
                let pool_dev = pool_dev.clone();
                let pool_file = state.pool_file.clone();
                let device_index = replay_count + (batch_idx * concurrency_r as usize + i + 1) as u32;
                let slot = i as u32;
                let proxy_addr_r = if cfg.use_proxy { Some(proxy_addr) } else { None };
                let ip_rx = current_ip_rx.clone();

                handles.push(tokio::spawn(async move {
                    retention_worker(
                        app_w, sdk_w, adb_port, slot, device_index, cfg_w,
                        results, pool_dev, pool_file, proxy_addr_r, cancel_w,
                        ip_rx,
                    ).await;
                }));
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            // 等当前批次完成再启下一批
            for h in handles.drain(..) {
                let _ = h.await;
            }
            if ((batch_idx + 1) * concurrency_r as usize) < pool_devices.len() && !cancel.is_cancelled() && cfg.auto_rotate_ip {
                let (tx, rx) = tokio::sync::watch::channel(false);
                current_ip_rx = Some(rx);
                let app_c = app.clone();
                let sdk_c = sdk.clone();
                let cfg_c = cfg.clone();
                let cancel_c = cancel.clone();
                tokio::spawn(async move {
                    maybe_rotate_ip(&app_c, &sdk_c, &cfg_c, &cancel_c).await;
                    let _ = tx.send(true);
                });
            }
        }
    }

    // ---- Phase B：新增设备（一次性 AVD，现有 L3 流程）----
    if new_count > 0 {
        // 更新 count 以适配 slot_worker 的分片逻辑
        let mut new_cfg = (*cfg).clone();
        new_cfg.count = new_count;
        let new_cfg = Arc::new(new_cfg);

        let concurrency_n = new_cfg.concurrency.min(new_count).min(8);

        for slot in 0..concurrency_n {
            let app = app.clone();
            let cfg = new_cfg.clone();
            let results = results.clone();
            let registry = registry.clone();
            let cancel = cancel.clone();
            let sdk = sdk.clone();
            let adb_port = state.adb_server_port;
            let run_dir_slot = run_dir.clone();

            handles.push(tokio::spawn(async move {
                slot_worker(app, sdk, adb_port, slot, cfg, results, registry, proxy_addr, cancel, run_dir_slot).await;
            }));
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        for h in handles {
            let _ = h.await;
        }
    }

    // 清场（只清理一次性 AVD，不删除池设备）
    let adb2 = Adb::new(&sdk, state.adb_server_port);
    let avdm2 = AvdManager::new(&sdk, adb2.env().clone());
    registry.cleanup(&adb2, &avdm2).await;
    let stats = proxy.snapshot().await;
    proxy.stop();

    let devices = results.lock().await.clone();
    let ok = devices.iter().filter(|d| d.status == "ok").count() as u32;
    let fail = devices.iter().filter(|d| d.status == "fail").count() as u32;

    let result = crate::report::RunResult {
        run_id: run_id.clone(),
        api_level: extract_api_level(&cfg.system_image),
        reset_level: cfg.reset_level.as_str().into(),
        target: total,
        ok,
        fail,
        timezone_note: "所有时间戳为北京时间（UTC+8）；批次覆盖自然日以 started_at 为准".into(),
        started_at,
        finished_at: beijing_now(),
        devices,
        compliance_ack: true,
    };
    let _ = result.save(&run_dir).await;

    emit_state(&app, "batch://progress", serde_json::json!({
        "ok": ok, "fail": fail, "running": 0, "queued": 0,
        "done": true, "umeng_hits": stats.umeng_hits,
    }));

    Ok(result)
}

/// 身份档案回放工作线程（方案 A：300+ 留存）
/// 还原档案中的 ANDROID_ID (SSAID) 与 shared_prefs XML，友盟识别为对应老用户回访
#[allow(clippy::too_many_arguments)]
async fn profile_replay_worker(
    app: AppHandle,
    sdk: std::path::PathBuf,
    adb_port: u16,
    slot: u32,
    device_index: u32,
    cfg: Arc<EngineConfig>,
    results: Arc<tokio::sync::Mutex<Vec<DeviceResult>>>,
    registry: Arc<AvdRegistry>,
    prof: crate::engine::profile_archive::IdentityProfile,
    proxy_addr: Option<std::net::SocketAddr>,
    cancel: CancellationToken,
    ip_ready_rx: Option<tokio::sync::watch::Receiver<bool>>,
) {
    let adb = Adb::new(&sdk, adb_port);
    let avdm = AvdManager::new(&sdk, adb.env().clone());
    let emulator = Emulator::new(&sdk, adb.env().clone());
    let avd = format!("dau-replay-{}-{}", slot, std::process::id());
    let port = 5554 + slot as u16 * 2;
    let serial = format!("emulator-{}", port);
    let started = std::time::Instant::now();
    let started_at = beijing_now();

    if cancel.is_cancelled() {
        return;
    }

    let _ = adb.emu_kill(&serial).await;
    if !crate::avd::wait_port_free(port, 2).await {
        crate::avd::kill_emulator_on_port(port).await;
        let _ = crate::avd::wait_port_free(port, 3).await;
    }

    if avdm.create(&avd, &cfg.system_image, &cfg.device_profile).await.is_err() {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("档案回放: 创建 AVD 失败".into()),
            reset_level: "archive-replay".into(),
            started_at,
            ..Default::default()
        }).await;
        return;
    }
    registry.register(&avd, port).await;

    // 使用档案保存的硬件品牌属性（保持与建库时一致）
    let opts = BootOpts {
        wipe: true,
        mem_mb: Some(cfg.emu_mem_mb),
        http_proxy: proxy_addr,
        max_users: None,
        props: prof.to_boot_props(),
    };

    if emulator.boot(&avd, port, &opts).await.is_err()
        || adb.wait_boot(&serial, Duration::from_secs(cfg.boot_timeout_s as u64)).await.is_err()
    {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("档案回放: 启动失败".into()),
            reset_level: "archive-replay".into(),
            started_at,
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // 关键步骤：覆盖真机设备型号（修改 build.prop 并重载），确保友盟统计获取到真实的档案机型而非 sdk_gphone
    crate::uiautomation::prepare::apply_device_spoofing(&adb, &emulator, &avd, port, &opts).await;

    // 关键优化：冷启动完成，等待换 IP 在后台就绪（二者完全并行，几乎零额外等待）
    wait_ip_ready(ip_ready_rx).await;

    if adb.wait_net(&serial).await.is_err()
        || adb.install(&serial, Path::new(&cfg.apk_path), None).await.is_err()
    {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("档案回放: 网络/装包失败".into()),
            reset_level: "archive-replay".into(),
            started_at,
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // 关键步骤：还原档案中的 ANDROID_ID (SSAID) 与 shared_prefs XML
    if let Err(e) = crate::engine::profile_archive::restore_to_device(&adb, &serial, &cfg.pkg, &prof).await {
        tracing::warn!(profile_id = %prof.profile_id, error = %e, "档案还原警告，继续尝试运行");
    }

    let ctx = SlotCtx {
        adb: adb.clone(),
        avdm: avdm.clone(),
        emulator: emulator.clone(),
        config: cfg.clone(),
        registry: registry.clone(),
        proxy_addr,
        cancel: cancel.clone(),
    };

    let mut result = DeviceResult {
        index: device_index,
        slot,
        status: "fail".into(),
        reset_level: "archive-replay".into(),
        started_at,
        is_retention: true,
        device_model: format!("{} {}", prof.brand, prof.model),
        ..Default::default()
    };

    match run_app(&ctx, port, None, device_index).await {
        Ok(outcome) => {
            result.prepare = outcome.prepare;
            result.onboarding = outcome.onboarding;
            result.coverage = outcome.coverage;
        }
        Err(e) => {
            result.error = Some(format!("档案回放运行失败: {}", e));
            result.duration_s = started.elapsed().as_secs_f64();
            let _ = adb.emu_kill(&serial).await;
            push_result(&app, &results, result).await;
            return;
        }
    }

    let id = crate::identity::extract(&adb, &serial, &cfg.pkg, None).await;
    result.android_id = if !id.android_id.is_empty() { id.android_id } else { prof.android_id };
    result.umid = if !id.umid.is_empty() { id.umid } else { prof.umid };
    result.status = "ok".into();
    result.duration_s = started.elapsed().as_secs_f64();

    tracing::info!(
        device_index,
        profile_id = %prof.profile_id,
        android_id = %result.android_id,
        model = %result.device_model,
        "【留存设备测试成功】已成功从档案库调取身份档案并完成运行"
    );

    let _ = adb.emu_kill(&serial).await;
    push_result(&app, &results, result).await;
}

/// 执行单台 L3 新增设备，成功后进行 50%-70% 概率入栈与出栈 (FIFO Eviction)
#[allow(clippy::too_many_arguments)]
async fn run_single_l3_and_push_stack(
    app: AppHandle,
    sdk: std::path::PathBuf,
    adb_port: u16,
    slot: u32,
    device_index: u32,
    cfg: Arc<EngineConfig>,
    results: Arc<tokio::sync::Mutex<Vec<DeviceResult>>>,
    registry: Arc<AvdRegistry>,
    proxy_addr: Option<std::net::SocketAddr>,
    stack_file: std::path::PathBuf,
    cancel: CancellationToken,
    ip_ready_rx: Option<tokio::sync::watch::Receiver<bool>>,
) {
    let adb = Adb::new(&sdk, adb_port);
    let avdm = AvdManager::new(&sdk, adb.env().clone());
    let emulator = Emulator::new(&sdk, adb.env().clone());
    let is_l25 = cfg.reset_level == crate::engine::ResetLevel::L25;
    let is_l3 = cfg.reset_level == crate::engine::ResetLevel::L3;

    // 槽位 AVD 名字采用固定名称，实现多台间复用同台模拟器（无需每次都创建新 AVD / 冷启动）
    let avd = if is_l25 {
        format!("dau-slot-{}", slot)
    } else {
        format!("dau-stack-{}-{}", slot, std::process::id())
    };
    let port = 5554 + slot as u16 * 2;
    let serial = format!("emulator-{}", port);
    let started = std::time::Instant::now();
    let started_at = beijing_now();

    if cancel.is_cancelled() {
        return;
    }

    // 检查槽位模拟器是否已经在在线运行中
    let is_running = adb.shell(&serial, &["getprop", "sys.boot_completed"])
        .await
        .unwrap_or_default()
        .trim() == "1";

    if !is_running {
        if avdm.create(&avd, &cfg.system_image, &cfg.device_profile).await.is_err() {
            push_result(&app, &results, DeviceResult {
                index: device_index, slot, status: "fail".into(),
                error: Some(format!("{} 设备: 创建 AVD 失败", cfg.reset_level.as_str())),
                reset_level: if is_l25 { "L2.5 多用户".into() } else { cfg.reset_level.as_str().into() },
                started_at,
                ..Default::default()
            }).await;
            return;
        }
        registry.register(&avd, port).await;
    }

    // 同一槽位 AVD 复用：机型按 AVD 名固定，首次 L3 完整伪装后 build.prop 持久，
    // 后续 L3 wipe 重启命中 spoofing 早退（省 root/remount/重启）→ 单启动。
    // info 与 props 必须来自同一确定性 profile，保证型号显示/档案与实际伪装一致。
    let info = crate::avd::device_info_for_avd(&avd);
    let opts = BootOpts {
        wipe: is_l3, // 仅 L3 恢复出厂才执行 wipe-data
        mem_mb: Some(cfg.emu_mem_mb),
        http_proxy: proxy_addr,
        max_users: cfg.max_users,
        props: crate::avd::device_props_from_info(&info),
    };

    if is_l25 && is_running {
        tracing::info!(device_index, serial = %serial, "⚡ [L2.5 多用户] 复用当前槽位已开机模拟器，无需重新冷启动！");
    } else if is_l3 && is_running {
        // 关键优化：如果当前槽位模拟器在线且已完成指纹注入，直接调用 reset_device_in_place，
        // 彻底清理前序应用数据与 SSAID，热重启 Zygote 生成全新合法 ANDROID_ID，跳过冷启动与二次改写！
        tracing::info!(device_index, serial = %serial, "⚡ [L3 新增] 复用当前槽位已开机且已注入指纹的模拟器，执行单启动极速重置！");
        if let Err(e) = crate::uiautomation::prepare::reset_device_in_place(&adb, &serial, &cfg.pkg).await {
            tracing::warn!("reset_device_in_place 遇到警告 ({})，继续运行", e);
        }
    } else {
        if emulator.boot(&avd, port, &opts).await.is_err()
            || adb.wait_boot(&serial, Duration::from_secs(cfg.boot_timeout_s as u64)).await.is_err()
        {
            push_result(&app, &results, DeviceResult {
                index: device_index, slot, status: "fail".into(),
                error: Some(format!("{} 设备启动失败", cfg.reset_level.as_str())),
                reset_level: if is_l25 { "L2.5 多用户".into() } else { cfg.reset_level.as_str().into() },
                started_at,
                ..Default::default()
            }).await;
            let _ = adb.emu_kill(&serial).await;
            return;
        }
        if is_l3 {
            crate::uiautomation::prepare::apply_device_spoofing(&adb, &emulator, &avd, port, &opts).await;
        }
    }

    // 关键优化：冷启动与指纹注入完成，等待换 IP 在后台就绪（二者完全并行，大幅减少总执行耗时）
    wait_ip_ready(ip_ready_rx).await;

    if adb.wait_net(&serial).await.is_err() {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some(format!("{} 设备网络未就序", cfg.reset_level.as_str())),
            reset_level: if is_l25 { "L2.5 多用户".into() } else { cfg.reset_level.as_str().into() },
            started_at,
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // 处理 L2.5 多用户创建（如果配置的是 L2.5）
    let user_id = if is_l25 {
        let _ = adb.cleanup_users_by_prefix(&serial, "dau_").await;
        match adb.create_user(&serial, &format!("dau_{}", port)).await {
            Ok(uid) => {
                let _ = adb.start_user(&serial, uid).await;
                let _ = adb.install(&serial, Path::new(&cfg.apk_path), Some(uid)).await;
                Some(uid)
            }
            Err(e) => {
                push_result(&app, &results, DeviceResult {
                    index: device_index, slot, status: "fail".into(),
                    error: Some(format!("L2.5 创建多用户失败: {}", e)),
                    reset_level: "L2.5 多用户".into(),
                    started_at,
                    ..Default::default()
                }).await;
                let _ = adb.emu_kill(&serial).await;
                return;
            }
        }
    } else {
        if adb.install(&serial, Path::new(&cfg.apk_path), None).await.is_err() {
            push_result(&app, &results, DeviceResult {
                index: device_index, slot, status: "fail".into(),
                error: Some(format!("{} 设备装包失败", cfg.reset_level.as_str())),
                reset_level: cfg.reset_level.as_str().into(),
                started_at,
                ..Default::default()
            }).await;
            let _ = adb.emu_kill(&serial).await;
            return;
        }
        None
    };

    let ctx = SlotCtx {
        adb: adb.clone(),
        avdm: avdm.clone(),
        emulator: emulator.clone(),
        config: cfg.clone(),
        registry: registry.clone(),
        proxy_addr,
        cancel: cancel.clone(),
    };

    let reset_label = if is_l25 {
        "L2.5 多用户".to_string()
    } else {
        cfg.reset_level.as_str().to_string()
    };

    let mut result = DeviceResult {
        index: device_index,
        slot,
        status: "fail".into(),
        reset_level: reset_label,
        started_at,
        is_retention: false,
        device_model: format!("{} {}", info.brand, info.model),
        ..Default::default()
    };

    match run_app(&ctx, port, user_id, device_index).await {
        Ok(outcome) => {
            result.prepare = outcome.prepare;
            result.onboarding = outcome.onboarding;
            result.coverage = outcome.coverage;
        }
        Err(e) => {
            result.error = Some(format!("设备运行失败: {}", e));
            result.duration_s = started.elapsed().as_secs_f64();
            if let Some(uid) = user_id {
                let _ = adb.switch_user(&serial, 0).await;
                let _ = adb.remove_user(&serial, uid).await;
            }
            let _ = adb.emu_kill(&serial).await;
            push_result(&app, &results, result).await;
            return;
        }
    }

    let id = crate::identity::extract(&adb, &serial, &cfg.pkg, user_id).await;
    result.android_id = id.android_id;
    result.umid = id.umid;
    result.status = "ok".into();
    result.duration_s = started.elapsed().as_secs_f64();

    // 清理 L2.5 多用户
    if let Some(uid) = user_id {
        let _ = adb.switch_user(&serial, 0).await;
        let _ = adb.remove_user(&serial, uid).await;
    }


    // 成功后备份档案并根据 50%-70% 概率推入堆栈 (处理容量与 FIFO 淘汰)
    if !result.android_id.is_empty() {
        let profile_id = format!("prof-{:04}", device_index);
        if let Ok(prof) = crate::engine::profile_archive::backup_from_device(
            &adb, &serial, &cfg.pkg, &profile_id, &info
        ).await {
            let mut stack = crate::engine::profile_stack::ProfileStackStore::load(&stack_file).await;
            stack.capacity = cfg.stack_capacity as usize;
            let today = crate::engine::profile_stack::ProfileStackStore::beijing_now_info().0;
            let pushed = stack.push_new_profile(prof, cfg.full_push_probability, Some(&today));
            let _ = stack.save(&stack_file).await;
            if pushed {
                tracing::info!(device_index, "新设备已成功推入堆栈（并锁定为今天已使用，次日/重置后可复用为留存）");
            }
        }
    }

    // L2.5 多用户模式与 L3 模式下保持模拟器开机，供同槽位后续设备极速复用（单启动 ~50s）
    // 批次完成或取消停止时由 registry.cleanup 统一进行安全清理
    push_result(&app, &results, result).await;
}

/// 留存设备工作线程：复用已有池 AVD，不 wipe，保留 ANDROID_ID → 友盟识别为老用户回访
#[allow(clippy::too_many_arguments)]
async fn retention_worker(
    app: AppHandle,
    sdk: std::path::PathBuf,
    adb_port: u16,
    slot: u32,
    device_index: u32,
    cfg: Arc<EngineConfig>,
    results: Arc<tokio::sync::Mutex<Vec<DeviceResult>>>,
    pool_dev: crate::engine::pool::PoolDevice,
    pool_file: std::path::PathBuf,
    proxy_addr: Option<std::net::SocketAddr>,
    cancel: CancellationToken,
    ip_ready_rx: Option<tokio::sync::watch::Receiver<bool>>,
) {
    let adb = Adb::new(&sdk, adb_port);
    let avdm = AvdManager::new(&sdk, adb.env().clone());
    let emulator = Emulator::new(&sdk, adb.env().clone());
    let port = 5554 + slot as u16 * 2;
    let serial = format!("emulator-{}", port);
    let avd = &pool_dev.avd_name;
    let started = std::time::Instant::now();
    let started_at = beijing_now();

    if cancel.is_cancelled() {
        return;
    }

    // 确保 AVD 存在（首次使用时创建）
    if avdm.create(avd, &cfg.system_image, &cfg.device_profile).await.is_err() {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("留存设备: 创建 AVD 失败".into()),
            reset_level: "retention".into(),
            started_at,
            ..Default::default()
        }).await;
        return;
    }

    // 使用池设备固定的品牌/型号属性（不随机），保持跨天一致性
    let opts = BootOpts {
        wipe: false, // 关键：不 wipe，保留 ANDROID_ID
        mem_mb: Some(cfg.emu_mem_mb),
        http_proxy: proxy_addr,
        max_users: None,
        props: pool_dev.to_boot_props(),
    };

    let boot_result = emulator.boot(avd, port, &opts).await;
    if boot_result.is_err()
        || adb.wait_boot(&serial, Duration::from_secs(cfg.boot_timeout_s as u64)).await.is_err()
    {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("留存设备: 启动失败".into()),
            reset_level: "retention".into(),
            started_at,
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // 覆盖真机设备型号（修改 build.prop 并重载），确保留存设备为真实机型
    crate::uiautomation::prepare::apply_device_spoofing(&adb, &emulator, avd, port, &opts).await;

    // 关键优化：冷启动完成，等待换 IP 在后台就绪（二者完全并行，几乎零额外等待）
    wait_ip_ready(ip_ready_rx).await;

    if adb.wait_net(&serial).await.is_err() {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("留存设备: 网络就绪超时".into()),
            reset_level: "retention".into(),
            started_at,
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // L1 重置 App 数据（清除 App 缓存但保留设备身份）
    let _ = adb.pm_clear(&serial, &cfg.pkg).await;

    // 重新安装 APK（install -r 覆盖安装，兼容 APK 版本更新）
    if adb.install(&serial, Path::new(&cfg.apk_path), None).await.is_err() {
        push_result(&app, &results, DeviceResult {
            index: device_index, slot, status: "fail".into(),
            error: Some("留存设备: 装包失败".into()),
            reset_level: "retention".into(),
            started_at,
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 运行 App（与新增设备共用同一流程）
    let ctx = SlotCtx {
        adb: adb.clone(),
        avdm,
        emulator,
        config: cfg.clone(),
        registry: Arc::new(AvdRegistry::new(Path::new("/dev/null"))),
        proxy_addr,
        cancel: cancel.clone(),
    };

    let mut result = DeviceResult {
        index: device_index,
        slot,
        status: "fail".into(),
        reset_level: "retention".into(),
        started_at,
        is_retention: true,
        device_model: format!("{} {}", pool_dev.brand, pool_dev.model),
        ..Default::default()
    };

    match run_app(&ctx, port, None, device_index).await {
        Ok(outcome) => {
            result.prepare = outcome.prepare;
            result.onboarding = outcome.onboarding;
            result.coverage = outcome.coverage;
        }
        Err(e) => {
            result.error = Some(format!("留存设备运行失败: {}", e));
            result.duration_s = started.elapsed().as_secs_f64();
            let _ = adb.emu_kill(&serial).await;
            push_result(&app, &results, result).await;
            return;
        }
    }

    // 提取标识
    let id = crate::identity::extract(&adb, &serial, &cfg.pkg, None).await;
    result.android_id = id.android_id.clone();
    result.umid = id.umid;
    result.status = "ok".into();
    result.duration_s = started.elapsed().as_secs_f64();

    // 更新池设备的 android_id 和 last_used_at
    let mut pool = crate::engine::pool::DevicePool::load(&pool_file).await;
    pool.update_device(avd, &id.android_id);
    let _ = pool.save(&pool_file).await;

    // 关机但不删除 AVD（留待下次复用）
    let _ = adb.emu_kill(&serial).await;

    push_result(&app, &results, result).await;
}

#[allow(clippy::too_many_arguments)]
async fn slot_worker(
    app: AppHandle,
    sdk: std::path::PathBuf,
    adb_port: u16,
    slot: u32,
    cfg: Arc<EngineConfig>,
    results: Arc<tokio::sync::Mutex<Vec<DeviceResult>>>,
    registry: Arc<AvdRegistry>,
    proxy_addr: std::net::SocketAddr,
    cancel: CancellationToken,
    _run_dir: std::path::PathBuf,
) {
    let adb = Adb::new(&sdk, adb_port);
    let avdm = AvdManager::new(&sdk, adb.env().clone());
    let emulator = Emulator::new(&sdk, adb.env().clone());
    let avd = format!("dau-slot-{}-{}", slot, std::process::id());
    let port = 5554 + slot as u16 * 2;
    let serial = format!("emulator-{}", port);

    let ctx = SlotCtx {
        adb: adb.clone(),
        avdm: avdm.clone(),
        emulator: emulator.clone(),
        config: cfg.clone(),
        registry: registry.clone(),
        proxy_addr: if cfg.use_proxy { Some(proxy_addr) } else { None },
        cancel: cancel.clone(),
    };

    // 建 AVD + 初始启动（首台即干净系统，跳过重置——v2 修复 #3）
    if avdm.create(&avd, &cfg.system_image, &cfg.device_profile).await.is_err() {
        push_result(&app, &results, DeviceResult {
            index: 0, slot, status: "fail".into(),
            error: Some("创建 AVD 失败".into()),
            ..Default::default()
        }).await;
        return;
    }
    registry.register(&avd, port).await;

    let opts = BootOpts {
        wipe: true,
        mem_mb: Some(cfg.emu_mem_mb),
        http_proxy: ctx.proxy_addr,
        max_users: cfg.max_users,
        props: crate::avd::device_props_for_avd(&avd),
    };
    if emulator.boot(&avd, port, &opts).await.is_err()
        || adb.wait_boot(&serial, Duration::from_secs(cfg.boot_timeout_s as u64)).await.is_err()
    {
        push_result(&app, &results, DeviceResult {
            index: 0, slot, status: "fail".into(),
            error: Some("初始启动失败".into()),
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // 动态覆盖真机设备型号（修改 build.prop 并重载）
    crate::uiautomation::prepare::apply_device_spoofing(&adb, &emulator, &avd, port, &opts).await;

    if adb.wait_net(&serial).await.is_err()
        || adb.install(&serial, Path::new(&cfg.apk_path), None).await.is_err()
    {
        push_result(&app, &results, DeviceResult {
            index: 0, slot, status: "fail".into(),
            error: Some("初始启动/装包失败".into()),
            ..Default::default()
        }).await;
        let _ = adb.emu_kill(&serial).await;
        return;
    }

    // 该槽负责的设备序号：slot+1, slot+1+C, ...（脚本分片逻辑）
    let mut i = slot + 1;
    let mut first = true;
    while i <= cfg.count {
        if cancel.is_cancelled() {
            break;
        }
        let r = run_one_device(&ctx, &avd, port, i, slot, !first).await;
        push_result(&app, &results, r).await;
        first = false;
        i += cfg.concurrency;
    }

    let _ = adb.emu_kill(&serial).await;
}

async fn push_result(
    app: &AppHandle,
    results: &Arc<tokio::sync::Mutex<Vec<DeviceResult>>>,
    r: DeviceResult,
) {
    let mut g = results.lock().await;
    g.push(r.clone());
    let ok = g.iter().filter(|d| d.status == "ok").count();
    let fail = g.iter().filter(|d| d.status == "fail").count();
    drop(g);
    let _ = app.emit("batch://device", &r);
    let _ = app.emit("batch://progress", serde_json::json!({
        "ok": ok, "fail": fail, "done": false,
    }));
}

fn extract_api_level(system_image: &str) -> u32 {
    system_image
        .split(';')
        .find_map(|p| p.strip_prefix("android-"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(34)
}

impl Default for DeviceResult {
    fn default() -> Self {
        Self {
            index: 0,
            slot: 0,
            status: "fail".into(),
            error: None,
            android_id: String::new(),
            umid: String::new(),
            proxy_hits: 0,
            duration_s: 0.0,
            reset_level: String::new(),
            started_at: String::new(),
            is_retention: false,
            device_model: String::new(),
            prepare: None,
            onboarding: None,
            coverage: None,
        }
    }
}
