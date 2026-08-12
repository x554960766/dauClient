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

    // 5. 磁盘/内存余量 → 推荐并发
    let mem_gb = physical_memory_gb();
    let recommended = if mem_gb >= 64 { 6 } else if mem_gb >= 32 { 4 } else { 2 }.min(8);
    let disk_ok = disk_free_gb(&state.runs_dir) >= 10;
    items.push(PreflightItem {
        id: "resources".into(),
        label: "资源余量".into(),
        ok: disk_ok,
        detail: format!("内存 {}GB，推荐并发 {}", mem_gb, recommended),
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
        // GlobalMemoryStatusEx 简化：用 wmic
        if let Ok(out) = std::process::Command::new("wmic")
            .args(["computersystem", "get", "TotalPhysicalMemory", "/value"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(eq) = s.find('=') {
                if let Ok(bytes) = s[eq + 1..].trim().parse::<u64>() {
                    return bytes / 1024 / 1024 / 1024;
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

    // v1.2：枚举 APK 声明的权限（§11.1，prepare 阶段 pm grant 用）
    let permissions = match tokio::process::Command::new(&aapt)
        .args(["dump", "permissions", apk_path])
        .output()
        .await
    {
        Ok(o) => crate::uiautomation::prepare::extract_permissions(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    };

    Ok(ApkInfo { pkg, appkey, debuggable, min_sdk, appkey_matches_declared: matches, blacklist_hit, permissions })
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
            props: crate::avd::random_device_props(),
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
        adb.launch_app(&serial, &cfg.pkg, None).await.map_err(|e| e.to_string())?;
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

        adb.key_home(&serial).await.map_err(|e| e.to_string())?;
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

    // 代理（若试点已启动则复用端口，否则新起）
    let proxy = CountingProxy::start().await.map_err(|e| e.to_string())?;
    let proxy_addr: std::net::SocketAddr = format!("127.0.0.1:{}", proxy.port).parse().unwrap();

    let registry = Arc::new(AvdRegistry::new(&run_dir.join("avds.json")));
    let results = Arc::new(tokio::sync::Mutex::new(Vec::<DeviceResult>::new()));
    let cfg = Arc::new(cfg);
    let started_at = beijing_now();

    let concurrency = cfg.concurrency.min(cfg.count).min(8);
    let mut handles = Vec::new();

    for slot in 0..concurrency {
        let app = app.clone();
        let cfg = cfg.clone();
        let results = results.clone();
        let registry = registry.clone();
        let cancel = cancel.clone();
        let sdk = sdk.clone();
        let adb_port = state.adb_server_port;
        let run_dir_slot = run_dir.clone();

        handles.push(tokio::spawn(async move {
            slot_worker(app, sdk, adb_port, slot, cfg, results, registry, proxy_addr, cancel, run_dir_slot).await;
        }));
        // 错开启动，避免同时抢 adb server 和磁盘
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    for h in handles {
        let _ = h.await;
    }

    // 清场
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
        target: cfg.count,
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
        props: crate::avd::random_device_props(),
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
            prepare: None,
            onboarding: None,
            coverage: None,
        }
    }
}
