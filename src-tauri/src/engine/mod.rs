//! 执行引擎（设计文档 §6）：槽位并发 / 重置阶梯 / 取消 / 清理注册表

pub mod pool;
pub mod profile_archive;
pub mod profile_stack;

use crate::adb::{Adb, AdbError};
use crate::avd::{AvdManager, BootOpts, Emulator};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 重置阶梯（v1.1：L2.5 = 多用户，ssaid 编辑方案已删除）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetLevel {
    L1,  // pm clear                    ~3s
    L2,  // 卸载重装 + 清 sdcard 残留    ~15s
    L25, // pm create-user 多用户       ~15s
    L3,  // -wipe-data 恢复出厂         ~100s
}

impl ResetLevel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "L1" => Self::L1,
            "L2" => Self::L2,
            "L25" | "L2.5" => Self::L25,
            _ => Self::L3,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L25 => "L25",
            Self::L3 => "L3",
        }
    }
    /// 单台耗时估算（秒，不含 dwell），用于 BatchPage 耗时预估与跨零点告警
    pub fn est_seconds(&self) -> u32 {
        match self {
            Self::L1 => 3,
            Self::L2 => 15,
            Self::L25 => 15,
            Self::L3 => 55,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub apk_path: String,
    pub pkg: String,
    pub count: u32,
    pub concurrency: u32,
    pub reset_level: ResetLevel,
    pub system_image: String,
    pub device_profile: String,
    pub dwell_s: u32,
    pub flush_dwell_s: u32,
    pub boot_timeout_s: u32,
    pub emu_mem_mb: u32,
    pub use_proxy: bool,
    pub max_users: Option<u32>,
    /// v1.2 新增：UI 自动化配置（onboarding/prepare/coverage/evidence）
    #[serde(default)]
    pub ui: crate::uiautomation::UiAutomationConfig,
    /// v1.2 新增：APK 声明的运行时权限（inspect_apk 阶段枚举，prepare 阶段 pm grant）
    #[serde(default)]
    pub runtime_permissions: Vec<String>,
    /// v1.2 新增：运行输出根目录（失败 dump/截图落盘用）
    #[serde(default)]
    pub runs_dir: String,
    /// 留存设备池大小（0 = 不启用留存模式，全部为新增设备）
    #[serde(default)]
    pub retention_pool_size: u32,
    /// 每批新增设备数量（留存模式下与 retention_pool_size 共同决定总量）
    #[serde(default)]
    pub new_device_count: u32,
    /// 身份档案回放数量（0 = 不回放，用于 300+ 留存场景）
    #[serde(default)]
    pub profile_replay_count: u32,
    /// 是否在生成新设备时自动备份身份档案到档案库
    #[serde(default)]
    pub enable_profile_backup: bool,
    /// 档案堆栈容量（默认 300）
    #[serde(default = "default_stack_capacity")]
    pub stack_capacity: u32,
    /// 逐台摇号选 L3 新增的概率（范围 0.30 - 0.50，默认 0.40）
    #[serde(default = "default_l3_prob")]
    pub l3_probability: f64,
    /// 堆栈满时入栈淘汰的概率（范围 0.50 - 0.70，默认 0.60）
    #[serde(default = "default_full_push_prob")]
    pub full_push_probability: f64,
    /// 是否开启堆栈防重与概率轮换模式（默认 true）
    #[serde(default = "default_enable_stack")]
    pub enable_stack_mode: bool,
    /// 堆栈读取模式：Random (随机, 默认) | Sequential (顺序)
    #[serde(default)]
    pub stack_read_mode: crate::engine::profile_stack::StackReadMode,
    /// App 显示名称（aapt 解析，供桌面图标精确匹配启动；为空时 batch/pilot 起始时自动提取）
    #[serde(default)]
    pub app_label: String,
    /// 是否在每批并发设备完成后自动通过手机 USB 飞行模式更换 IP（默认 false）
    #[serde(default)]
    pub auto_rotate_ip: bool,
    /// 指定换 IP 的手机序列号（None 则自动选第一台真机）
    #[serde(default)]
    pub rotate_ip_serial: Option<String>,
    /// 飞行模式断网保持秒数（默认 4s）
    #[serde(default = "default_rotate_ip_disconnect")]
    pub rotate_ip_disconnect_wait_s: u32,
    /// 飞行模式恢复后等待入网秒数（默认 6s）
    #[serde(default = "default_rotate_ip_reconnect")]
    pub rotate_ip_reconnect_wait_s: u32,
}

fn default_rotate_ip_disconnect() -> u32 { 4 }
fn default_rotate_ip_reconnect() -> u32 { 6 }
fn default_stack_capacity() -> u32 { 300 }
fn default_l3_prob() -> f64 { 0.40 }
fn default_full_push_prob() -> f64 { 0.60 }
fn default_enable_stack() -> bool { true }

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            apk_path: String::new(),
            pkg: String::new(),
            count: 3,
            concurrency: 2,
            reset_level: ResetLevel::L3,
            system_image: crate::sdkmgr::system_image_id(34),
            device_profile: "pixel_6".into(),
            dwell_s: 5,
            flush_dwell_s: 5,
            boot_timeout_s: 180,
            emu_mem_mb: 1280,
            use_proxy: true,
            max_users: None,
            ui: crate::uiautomation::UiAutomationConfig::default(),
            runtime_permissions: Vec::new(),
            runs_dir: String::new(),
            retention_pool_size: 0,
            new_device_count: 0,
            profile_replay_count: 0,
            enable_profile_backup: true,
            stack_capacity: 300,
            l3_probability: 0.40,
            full_push_probability: 0.60,
            enable_stack_mode: true,
            stack_read_mode: crate::engine::profile_stack::StackReadMode::Random,
            app_label: String::new(),
            auto_rotate_ip: false,
            rotate_ip_serial: None,
            rotate_ip_disconnect_wait_s: 4,
            rotate_ip_reconnect_wait_s: 6,
        }
    }
}

/// 单台设备执行结果（v1.1 P1-3：逐台标识落盘）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceResult {
    pub index: u32,
    pub slot: u32,
    pub status: String, // "ok" | "fail"
    pub error: Option<String>,
    pub android_id: String,
    pub umid: String,
    pub proxy_hits: u32,
    pub duration_s: f64,
    pub reset_level: String,
    pub started_at: String,
    /// 是否为留存设备回放（true = 留存，false = 新增）
    #[serde(default)]
    pub is_retention: bool,
    /// 设备机型与品牌（如 Xiaomi 14 Ultra / OPPO Find X7）
    #[serde(default)]
    pub device_model: String,
    /// v1.2 新增：环境准备结果（pm grant / 解锁屏 / 关动画）
    #[serde(default)]
    pub prepare: Option<crate::uiautomation::prepare::PrepareOutcome>,
    /// v1.2 新增：onboarding 多层引导循环统计
    #[serde(default)]
    pub onboarding: Option<crate::uiautomation::OnboardingStats>,
    /// v1.2 新增：CoverageWalker 覆盖遍历报告
    #[serde(default)]
    pub coverage: Option<crate::uiautomation::coverage::CoverageReport>,
}

/// AVD 清理注册表（替代脚本 trap cleanup；落盘保证强杀后可恢复）
pub struct AvdRegistry {
    file: PathBuf,
    entries: tokio::sync::Mutex<Vec<AvdEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvdEntry {
    pub avd: String,
    pub port: u16,
}

impl AvdRegistry {
    pub fn new(file: &Path) -> Self {
        Self { file: file.to_path_buf(), entries: tokio::sync::Mutex::new(Vec::new()) }
    }

    pub async fn register(&self, avd: &str, port: u16) {
        let mut g = self.entries.lock().await;
        g.push(AvdEntry { avd: avd.to_string(), port });
        let _ = self.persist(&g).await;
    }

    async fn persist(&self, entries: &[AvdEntry]) -> std::io::Result<()> {
        if let Some(p) = self.file.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let json = serde_json::to_string_pretty(entries)?;
        tokio::fs::write(&self.file, json).await
    }

    /// 统一清理：emu kill + delete avd（私有 adb server 限定范围，不会误删用户设备）
    pub async fn cleanup(&self, adb: &Adb, avdm: &AvdManager) {
        let entries = self.entries.lock().await.clone();
        for e in entries {
            let _ = adb.emu_kill(&format!("emulator-{}", e.port)).await;
            let _ = avdm.delete(&e.avd).await;
        }
        let _ = tokio::fs::remove_file(&self.file).await;
    }
}

/// 槽位上下文
pub struct SlotCtx {
    pub adb: Adb,
    pub avdm: AvdManager,
    pub emulator: Emulator,
    pub config: Arc<EngineConfig>,
    pub registry: Arc<AvdRegistry>,
    pub proxy_addr: Option<std::net::SocketAddr>,
    pub cancel: CancellationToken,
}

/// 重置一台设备（对应脚本 reset_device；首台跳过由调用方控制）
pub async fn reset_device(ctx: &SlotCtx, avd: &str, port: u16) -> Result<Option<u32>, AdbError> {
    let serial = format!("emulator-{}", port);
    let cfg = &ctx.config;
    match cfg.reset_level {
        ResetLevel::L1 => {
            ctx.adb.pm_clear(&serial, &cfg.pkg).await?;
            Ok(None)
        }
        ResetLevel::L2 => {
            ctx.adb.uninstall(&serial, &cfg.pkg).await?;
            let sdcard_paths = vec![
                format!("/sdcard/Android/data/{}", cfg.pkg),
                "/sdcard/.um".to_string(),
                "/sdcard/.utm".to_string(),
                format!("/sdcard/Android/obb/{}", cfg.pkg),
            ];
            let refs: Vec<&str> = sdcard_paths.iter().map(|s| s.as_str()).collect();
            ctx.adb.rm_rf(&serial, &refs).await?;
            ctx.adb.install(&serial, Path::new(&cfg.apk_path), None).await?;
            // install 后 PackageManager 需要时间注册 activity，否则 launch 会 monkey exit 251
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(None)
        }
        ResetLevel::L25 => {
            // v1.1 多用户方案：新用户 = 新 ANDROID_ID + 新 App 私有目录
            // 先清理历史残留 dau_* 用户，防止累积触顶 fw.max_users（模拟器默认 4），
            // 否则报 "Cannot add user. Maximum user limit is reached (code 6)"
            let cleaned = ctx
                .adb
                .cleanup_users_by_prefix(&serial, "dau_")
                .await
                .unwrap_or(0);
            if cleaned > 0 {
                tracing::info!(cleaned, "L2.5: 已清理残留 dau_ 用户");
            }
            let uid = ctx
                .adb
                .create_user(&serial, &format!("dau_{}", port))
                .await?;
            ctx.adb.start_user(&serial, uid).await?;
            ctx.adb.install(&serial, Path::new(&cfg.apk_path), Some(uid)).await?;
            // 新用户下 install 后 PackageManager 注册 activity 同样需要缓冲
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(Some(uid))
        }
        ResetLevel::L3 => {
            tracing::info!("[L3Reset] 开始重置模拟器: serial={}", serial);
            // 优化：检查当前模拟器是否在线且已注入真机指纹
            let is_online = ctx.adb.shell(&serial, &["getprop", "sys.boot_completed"]).await.unwrap_or_default().trim() == "1";
            let cur_model = if is_online {
                ctx.adb.shell(&serial, &["getprop", "ro.product.model"]).await.unwrap_or_default().trim().to_string()
            } else {
                String::new()
            };

            // 如果模拟器在线且已成功伪装为真机（非 sdk_gphone），直接执行单启动极速 In-place Reset
            // 耗时仅 ~15s：清空旧 App 数据 + 抹除 settings_ssaid.xml + 平滑热重启 Zygote，
            // 触发系统生成全新合法 ANDROID_ID，同时 100% 保护 /system 的真机 build.prop 不被 wipe 抹除！
            if is_online && !cur_model.is_empty() && !cur_model.contains("sdk_gphone") && !cur_model.contains("google") {
                tracing::info!("[L3Reset] ⚡ 命中已伪装真机模拟器 ({})，执行极速单启动重置...", cur_model);
                if crate::uiautomation::prepare::reset_device_in_place(&ctx.adb, &serial, &cfg.pkg).await.is_ok() {
                    ctx.adb.wait_net(&serial).await?;
                    ctx.adb.install(&serial, Path::new(&cfg.apk_path), None).await?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    return Ok(None);
                }
                tracing::warn!("[L3Reset] 极速重置遇到异常，平滑降级走重启流程");
            }

            let _ = ctx.adb.emu_kill(&serial).await;

            // 等待端口彻底被 OS 释放（确认旧 QEMU 进程已完全终止）
            if !crate::avd::wait_port_free(port, 10).await {
                tracing::warn!("[L3Reset] 旧 QEMU 进程未在 10s 内释放端口 {}，强行 kill 占位进程...", port);
                crate::avd::kill_emulator_on_port(port).await;
                let _ = crate::avd::wait_port_free(port, 5).await;
            } else {
                tracing::info!("[L3Reset] 旧 QEMU 模拟器进程已完全退出，端口 {} 已释放", port);
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;

            crate::avd::clean_avd_lock_files(avd);

            // 关键优化：后续重启不加 -wipe-data，避免抹除 OverlayFS 中的真实 build.prop，
            // 使开机后立即命中 apply_device_spoofing 的早退分支，跳过二次重启，实现单次启动。
            let opts = BootOpts {
                wipe: false,
                mem_mb: Some(cfg.emu_mem_mb),
                http_proxy: if cfg.use_proxy { ctx.proxy_addr } else { None },
                max_users: cfg.max_users,
                props: crate::avd::device_props_for_avd(avd),
            };
            let mut child = ctx.emulator.boot(avd, port, &opts).await.map_err(|e| AdbError::CommandFailed {
                cmd: "emulator boot".into(),
                code: -1,
                stderr: e.to_string(),
            })?;

            // 检查启动后 500ms 内进程是否异常挂掉（如 AVD 锁冲突）
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Ok(Some(status)) = child.try_wait() {
                return Err(AdbError::CommandFailed {
                    cmd: "emulator boot".into(),
                    code: status.code().unwrap_or(-1),
                    stderr: format!("模拟器进程启动后立即异常退出 (exit status: {})", status),
                });
            }

            ctx.adb.wait_boot(&serial, Duration::from_secs(cfg.boot_timeout_s as u64)).await?;

            // 检查并确保指纹（如已匹配则秒级跳过）
            crate::uiautomation::prepare::apply_device_spoofing(&ctx.adb, &ctx.emulator, avd, port, &opts).await;

            ctx.adb.wait_net(&serial).await?;
            ctx.adb.install(&serial, Path::new(&cfg.apk_path), None).await?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(None)
        }
    }
}

/// 单台设备 App 运行结果（v1.2：含 prepare/onboarding/coverage 三段统计）
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RunAppOutcome {
    pub prepare: Option<crate::uiautomation::prepare::PrepareOutcome>,
    pub onboarding: Option<crate::uiautomation::OnboardingStats>,
    pub coverage: Option<crate::uiautomation::coverage::CoverageReport>,
}

/// 启动 App → 多层引导 → 埋点覆盖 → 切后台 flush（v1.2 四段式，对应设计文档 §7.1）
///
/// 四段：
/// 1. prepare_device（§12.2）：switch-user（L2.5）→ 解锁屏 → 关全局动画 → pm grant 预授权
/// 2. OnboardingRunner（§4/§12.5）：多层引导循环（协议 → 引导页 → 功能指引 → 权限兜底 → 主页）
/// 3. CoverageWalker（§5/§12.6）：确定性埋点覆盖遍历（可选，无 steps 时跳过）
/// 4. HOME + flush_dwell（固定时长）
pub async fn run_app(
    ctx: &SlotCtx,
    port: u16,
    user: Option<u32>,
    index: u32,
) -> Result<RunAppOutcome, AdbError> {
    let serial = format!("emulator-{}", port);
    let cfg = &ctx.config;
    let mut outcome = RunAppOutcome::default();

    if !cfg.ui.enabled {
        // 回退 v1.1 旧行为：launch + dwell + HOME + flush
        ctx.adb.launch_app(&serial, &cfg.pkg, user, &cfg.app_label).await?;
        tokio::time::sleep(Duration::from_secs(cfg.dwell_s as u64)).await;
        let _ = ctx.adb.key_home(&serial).await;
        tokio::time::sleep(Duration::from_secs(cfg.flush_dwell_s as u64)).await;
        return Ok(outcome);
    }

    // ---- 1. prepare_device（§12.2）----
    let dump_dir = if cfg.runs_dir.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&cfg.runs_dir).join("dumps"))
    };
    let mut prep = crate::uiautomation::prepare::prepare_device(
        &ctx.adb,
        &serial,
        user,
        &cfg.ui.prepare,
    )
    .await?;
    crate::uiautomation::prepare::grant_permissions(
        &ctx.adb,
        &serial,
        &cfg.pkg,
        &cfg.runtime_permissions,
        user,
        &mut prep,
    )
    .await;
    outcome.prepare = Some(prep.clone());

    // ---- 2. 启动 App，等首屏渲染（固定时长）----
    ctx.adb.launch_app(&serial, &cfg.pkg, user, &cfg.app_label).await?;
    tokio::time::sleep(Duration::from_secs(cfg.dwell_s as u64)).await;

    // ---- 3. onboarding 多层引导循环 ----
    let screen = crate::uiautomation::interactor::Interactor::fetch_screen(&ctx.adb, &serial).await?;
    let runner = crate::uiautomation::OnboardingRunner {
        adb: ctx.adb.clone(),
        serial: serial.clone(),
        pkg: cfg.pkg.clone(),
        cfg: cfg.ui.onboarding.clone(),
        screen,
        dump_dir: dump_dir.clone(),
    };
    let onb = runner.run().await?;
    if !onb.reached_home {
        outcome.onboarding = Some(onb.clone());
        // 未到主页视为本台失败（协议没点上 → SDK 不初始化 → 白跑）
        return Err(AdbError::CommandFailed {
            cmd: format!("onboarding dev-{}", index),
            code: -1,
            stderr: format!(
                // v1.5：带上「活锁指纹」——哪个关卡被反复触发一目了然。
                // 之前只报 fail_reason/failed_at_round，遇到 max_rounds_exhausted
                // （每轮都点到东西却到不了主页）时完全无法判断卡在哪一层。
                "未到主页: fail_reason={:?} failed_at_round={:?} | rounds={} 用时{:.0}s \
                 | cling×{} 权限弹窗×{} 引导滑动×{} 跳过×{} 协议={} 引导进入={} 覆盖层={} 全量dump救回×{}",
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
            ),
        });
    }
    outcome.onboarding = Some(onb);

    // ---- 4. 主页交互：点击顶部轮播位触发内容埋点 + 短暂浏览 ----
    // 性能优化：onboarding 已到主页，此处不再 dump 找轮播节点（dump 命中率低且慢 ~3-4s，
    // 且 dump 在视频类 App 信息流页面可能因持续动画而超时），直接点击主页顶部黄金轮播位即可。
    let iac = crate::uiautomation::Interactor::new(ctx.adb.clone(), serial.clone(), screen);
    let bx = screen.ratio_x(0.50);
    let by = screen.ratio_y(0.25);
    tracing::info!("主页交互: 点击主页顶部轮播区 ({}, {})", bx, by);
    let _ = iac.tap(bx, by).await;

    // 主页浏览停留（8~15s 随机，模拟真实用户主页信息流浏览交互）
    let dwell_secs = pseudo_random_range(8, 15, index as u64);
    tracing::info!("主页交互: 浏览停留 {} 秒...", dwell_secs);
    tokio::time::sleep(Duration::from_secs(dwell_secs)).await;

    // ---- 5. HOME + flush（固定时长）----
    if let Err(e) = ctx.adb.key_home(&serial).await {
        tracing::warn!("HOME keyevent 执行受阻 ({})，继续执行 flush 驻留...", e);
    }
    tokio::time::sleep(Duration::from_secs(cfg.ui.flush_dwell_s as u64)).await;

    Ok(outcome)
}

/// 均匀伪随机浮点数生成器 [0.0, 1.0)（使用 Splitmix64 散列解决 macOS/Windows 微秒对齐导致的 nanos % 1000 偏置 Bug）
pub fn pseudo_random_f64(seed: u64) -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(123456789);

    let mut z = nanos
        .wrapping_add(count)
        .wrapping_add(seed)
        .wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z = z ^ (z >> 31);

    ((z % 10_000) as f64) / 10_000.0
}

/// 范围伪随机整数生成器 [min, max]
pub fn pseudo_random_range(min: u64, max: u64, seed: u64) -> u64 {
    if min >= max {
        return min;
    }
    let r = pseudo_random_f64(seed);
    min + ((max - min + 1) as f64 * r) as u64
}

/// 单台设备完整流程（不含槽位管理）：重置 → 运行 → 提取标识
pub async fn run_one_device(
    ctx: &SlotCtx,
    avd: &str,
    port: u16,
    index: u32,
    slot: u32,
    need_reset: bool,
) -> DeviceResult {
    let started = std::time::Instant::now();
    let started_at = beijing_now();
    let serial = format!("emulator-{}", port);
    let mut result = DeviceResult {
        index,
        slot,
        status: "fail".into(),
        error: None,
        android_id: String::new(),
        umid: String::new(),
        proxy_hits: 0,
        duration_s: 0.0,
        reset_level: ctx.config.reset_level.as_str().into(),
        started_at,
        is_retention: false,
        device_model: String::new(),
        prepare: None,
        onboarding: None,
        coverage: None,
    };

    if ctx.cancel.is_cancelled() {
        result.error = Some("已取消".into());
        return result;
    }

    let user = match if need_reset {
        reset_device(ctx, avd, port).await
    } else {
        Ok(None)
    } {
        Ok(u) => u,
        Err(e) => {
            result.error = Some(format!("重置失败: {}", e));
            result.duration_s = started.elapsed().as_secs_f64();
            return result;
        }
    };

    match run_app(ctx, port, user, index).await {
        Ok(outcome) => {
            result.prepare = outcome.prepare;
            result.onboarding = outcome.onboarding;
            result.coverage = outcome.coverage;
        }
        Err(e) => {
            result.error = Some(format!("运行失败: {}", e));
            result.duration_s = started.elapsed().as_secs_f64();
            // L2.5 收尾（失败也要切回 user 0 再删用户，否则 remove-user 失败）
            if let Some(uid) = user {
                let _ = ctx.adb.switch_user(&serial, 0).await;
                let _ = ctx.adb.remove_user(&serial, uid).await;
            }
            return result;
        }
    }

    // v1.1：逐台标识提取（T+1 对账原料）
    let id = crate::identity::extract(&ctx.adb, &serial, &ctx.config.pkg, user).await;
    result.android_id = id.android_id;
    result.umid = id.umid;

    let brand = ctx.adb.shell(&serial, &["getprop", "ro.product.brand"]).await.unwrap_or_default();
    let model = ctx.adb.shell(&serial, &["getprop", "ro.product.model"]).await.unwrap_or_default();
    result.device_model = format!("{} {}", brand.trim(), model.trim()).trim().to_string();

    // 自动备份身份档案（用于 300+ 留存场景）
    if ctx.config.enable_profile_backup && !result.android_id.is_empty() {
        let profiles_dir = crate::engine::profile_archive::ProfileArchiveManager::default_profiles_dir(&ctx.adb.env().sdk_dir);
        let profile_id = format!("prof-{:04}", index);
        let info = crate::avd::random_device_info();
        if let Ok(prof) = crate::engine::profile_archive::backup_from_device(
            &ctx.adb, &serial, &ctx.config.pkg, &profile_id, &info
        ).await {
            let _ = crate::engine::profile_archive::ProfileArchiveManager::save(&profiles_dir, &prof).await;
            tracing::info!(profile_id = %profile_id, android_id = %prof.android_id, "已自动归档新设备身份快照");
        }
    }

    // L2.5：用完即删用户（v1.2 修正：先切回 user 0，当前前台用户删不掉）
    if let Some(uid) = user {
        let _ = ctx.adb.switch_user(&serial, 0).await;
        let _ = ctx.adb.remove_user(&serial, uid).await;
    }

    result.status = "ok".into();
    result.duration_s = started.elapsed().as_secs_f64();
    result
}

/// 北京时间时间戳（v1.1 P1-4：报告所有时间戳统一北京时间）
pub fn beijing_now() -> String {
    let utc = chrono::Utc::now();
    let bj = utc + chrono::Duration::hours(8);
    bj.format("%Y-%m-%d %H:%M:%S UTC+8").to_string()
}

/// 跨零点预估（v1.1 P1-4）：按 COUNT × 标定耗时 ÷ 并发 估算结束时间
pub fn estimate_finish(cfg: &EngineConfig) -> (String, bool) {
    let per_device = cfg.reset_level.est_seconds() + cfg.dwell_s + cfg.flush_dwell_s;
    let total_s = (cfg.count as u64 * per_device as u64) / cfg.concurrency.max(1) as u64;
    let bj_now = chrono::Utc::now() + chrono::Duration::hours(8);
    let finish = bj_now + chrono::Duration::seconds(total_s as i64);
    let crosses_midnight = bj_now.date_naive() != finish.date_naive();
    (finish.format("%Y-%m-%d %H:%M UTC+8").to_string(), crosses_midnight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pseudo_random_distribution() {
        let mut count_under_40 = 0;
        let mut count_over_40 = 0;

        for i in 0..1000 {
            let r = pseudo_random_f64(i as u64);
            assert!(r >= 0.0 && r <= 1.0);
            if r < 0.40 {
                count_under_40 += 1;
            } else {
                count_over_40 += 1;
            }
        }

        // 验证 1000 次抽取中 r < 0.40 占比处于合理的随机区间 (30% ~ 50% 附近)
        // 绝不会发生像旧逻辑中 100% 返回 0.0 的全偏置 Bug
        assert!(count_under_40 > 250 && count_under_40 < 550);
        assert!(count_over_40 > 450 && count_over_40 < 750);
    }
}
