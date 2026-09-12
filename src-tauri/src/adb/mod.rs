//! adb 客户端（设计文档 §5）
//! v1.1 P0-1：所有进程统一注入私有 adb server 环境变量，与用户的
//! Android Studio / 全局 adb server 完全并存，互不干扰。

pub mod rotate_ip;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, thiserror::Error, Clone, Serialize, Deserialize)]
pub enum AdbError {
    #[error("设备离线: {serial}")]
    DeviceOffline { serial: String },
    #[error("启动超时: {serial}（已等待 {waited}s）")]
    BootTimeout { serial: String, waited: u32 },
    #[error("网络就绪超时: {serial}")]
    NetTimeout { serial: String },
    #[error("命令失败: {cmd} (exit {code}): {stderr}")]
    CommandFailed { cmd: String, code: i32, stderr: String },
}

/// 私有 adb server 环境变量块（§3 设计决策 1）
#[derive(Debug, Clone)]
pub struct AdbEnv {
    pub sdk_dir: PathBuf,
    pub adb_server_port: u16,
}

impl AdbEnv {
    pub fn apply(&self, cmd: &mut Command) {
        cmd.env("ANDROID_ADB_SERVER_PORT", self.adb_server_port.to_string());
        cmd.env("ANDROID_SDK_ROOT", &self.sdk_dir);
        cmd.env("ANDROID_SDK_HOME", &self.sdk_dir); // 关键：否则 avdmanager 仍写用户 ~/.android
        cmd.env("ANDROID_AVD_HOME", self.sdk_dir.join("avd-home"));
        cmd.env("JAVA_HOME", crate::sdkmgr::jre_home(&self.sdk_dir));
        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
    }
}

/// 探测空闲的私有 adb server 端口（默认 5137 起，避开 5037）
pub fn find_free_adb_port() -> u16 {
    for port in 5137..5200 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    5137
}

/// 桌面图标坐标全局缓存：key = "包名@宽x高"。同一 App 同分辨率下桌面布局一致，
/// 跨设备/跨台复用首次探测到的图标中心坐标；命中即跳过 home+dump+翻页/抽屉搜索直接点击，
/// 批量执行时显著提速。点击后仍校验前台焦点，失效则自动回退全量探测并重写缓存。
static ICON_POS_CACHE: once_cell::sync::Lazy<tokio::sync::Mutex<std::collections::HashMap<String, (i32, i32)>>> =
    once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

#[derive(Debug, Clone)]
pub struct Adb {
    bin: PathBuf,
    env: AdbEnv,
}

impl Adb {
    pub fn new(sdk_dir: &Path, adb_server_port: u16) -> Self {
        Self {
            bin: sdk_dir.join("platform-tools").join(crate::sdkmgr::adb_bin_name()),
            env: AdbEnv { sdk_dir: sdk_dir.to_path_buf(), adb_server_port },
        }
    }

    pub fn env(&self) -> &AdbEnv {
        &self.env
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        self.env.apply(&mut c);
        c
    }

    async fn run(&self, args: &[&str]) -> Result<String, AdbError> {
        let out = self
            .cmd()
            .args(args)
            .output()
            .await
            .map_err(|e| AdbError::CommandFailed {
                cmd: format!("adb {}", args.join(" ")),
                code: -1,
                stderr: e.to_string(),
            })?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if stderr.contains("device offline") {
            return Err(AdbError::DeviceOffline { serial: args.first().unwrap_or(&"").to_string() });
        }
        if !out.status.success() {
            return Err(AdbError::CommandFailed {
                cmd: format!("adb {}", args.join(" ")),
                code: out.status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }
        Ok(stdout)
    }

    pub async fn run_on(&self, serial: &str, args: &[&str]) -> Result<String, AdbError> {
        let mut full: Vec<&str> = vec!["-s", serial];
        full.extend_from_slice(args);
        let res = self.run(&full).await;
        if let Err(ref err) = res {
            if is_device_offline_or_not_found(err) {
                if let Some(port_str) = serial.strip_prefix("emulator-") {
                    if let Ok(port) = port_str.parse::<u16>() {
                        let _ = self.connect(&format!("127.0.0.1:{}", port + 1)).await;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return self.run(&full).await;
                    }
                }
                return Err(AdbError::DeviceOffline { serial: serial.to_string() });
            }
        }
        res
    }

    /// 启动私有 adb server（幂等）
    pub async fn start_server(&self) -> Result<(), AdbError> {
        self.run(&["start-server"]).await.map(|_| ())
    }

    /// 列出挂在私有 server 上的设备
    pub async fn devices(&self) -> Result<Vec<String>, AdbError> {
        let out = self.run(&["devices"]).await?;
        Ok(out
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().next())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect())
    }

    pub async fn connect(&self, addr: &str) -> Result<(), AdbError> {
        let _ = self.run(&["connect", addr]).await;
        Ok(())
    }

    /// 等待启动完成（boot_completed=1），等价脚本 wait_boot
    pub async fn wait_boot(&self, serial: &str, timeout: Duration) -> Result<(), AdbError> {
        let mut adb_target_port = None;
        if let Some(port_str) = serial.strip_prefix("emulator-") {
            if let Ok(port) = port_str.parse::<u16>() {
                adb_target_port = Some(port + 1);
            }
        }

        tokio::time::timeout(timeout, async {
            loop {
                if let Some(p) = adb_target_port {
                    let _ = self.connect(&format!("127.0.0.1:{}", p)).await;
                }
                let out = self
                    .run_on(serial, &["shell", "getprop", "sys.boot_completed"])
                    .await
                    .unwrap_or_default();
                if out.trim() == "1" {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        })
        .await
        .map_err(|_| AdbError::BootTimeout {
            serial: serial.into(),
            waited: timeout.as_secs() as u32,
        })?
    }

    /// 等待网络就绪（v2 修复 #4：boot_completed=1 时网络往往未通）
    /// 支持多 IP / QEMU 网关 / DNS 属性轮询，防止单一 ICMP 被代理或宿主机防火墙阻断
    pub async fn wait_net(&self, serial: &str) -> Result<(), AdbError> {
        for _ in 0..40 {
            if let Ok(dns) = self.run_on(serial, &["shell", "getprop", "net.dns1"]).await {
                let dns_str = dns.trim();
                if !dns_str.is_empty() && dns_str != "0.0.0.0" && !dns_str.contains("device") {
                    return Ok(());
                }
            }
            if self.run_on(serial, &["shell", "ping", "-c", "1", "-W", "1", "223.5.5.5"]).await.is_ok()
                || self.run_on(serial, &["shell", "ping", "-c", "1", "-W", "1", "10.0.2.2"]).await.is_ok()
                || self.run_on(serial, &["shell", "ping", "-c", "1", "-W", "1", "114.114.114.114"]).await.is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(AdbError::NetTimeout { serial: serial.into() })
    }

    /// 等待系统 PackageManager 服务就绪（避免开机早期或重置瞬间安装 APK 失败）
    pub async fn wait_package_manager(&self, serial: &str, timeout: Duration) -> Result<(), AdbError> {
        tokio::time::timeout(timeout, async {
            loop {
                let out = self
                    .run_on(serial, &["shell", "service", "check", "package"])
                    .await
                    .unwrap_or_default();
                if out.contains("found") && !out.contains("not found") {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .map_err(|_| AdbError::CommandFailed {
            cmd: "wait_package_manager".into(),
            code: -1,
            stderr: "PackageManager 服务未就绪（超时）".into(),
        })?
    }

    /// 自动重试的 APK 安装（最多重试 3 次，解决 L3 wipe-data 后 PackageManager 暂未准备就绪导致的偶现安装失败）
    pub async fn install(&self, serial: &str, apk: &Path, user: Option<u32>) -> Result<(), AdbError> {
        let _ = self.wait_package_manager(serial, Duration::from_secs(15)).await;
        let apk_s = apk.display().to_string();
        let mut last_err = None;

        for attempt in 1..=3 {
            let res = match user {
                Some(u) => {
                    let u = u.to_string();
                    self.run_on(serial, &["install", "-r", "--user", &u, &apk_s]).await
                }
                None => {
                    self.run_on(serial, &["install", "-r", &apk_s]).await
                }
            };

            match res {
                Ok(_) => return Ok(()),
                Err(e) => {
                    tracing::warn!("[ADB] 安装 APK 尝试 {}/3 失败: {}, 2s 后重试...", attempt, e);
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| AdbError::CommandFailed {
            cmd: "install".to_string(),
            code: -1,
            stderr: "装包失败（重试 3 次均未成功）".to_string(),
        }))
    }

    pub async fn uninstall(&self, serial: &str, pkg: &str) -> Result<(), AdbError> {
        // 包不存在时 uninstall 失败属正常，忽略
        let _ = self.run_on(serial, &["uninstall", pkg]).await;
        Ok(())
    }

    pub async fn pm_clear(&self, serial: &str, pkg: &str) -> Result<(), AdbError> {
        self.run_on(serial, &["shell", "pm", "clear", pkg]).await.map(|_| ())
    }

    pub async fn rm_rf(&self, serial: &str, paths: &[&str]) -> Result<(), AdbError> {
        for p in paths {
            let _ = self.run_on(serial, &["shell", "rm", "-rf", p]).await;
        }
        Ok(())
    }

    /// 轮询等待指定包名进入前台焦点，每次间隔 300ms，命中即返回 true
    async fn wait_foreground(&self, serial: &str, pkg: &str, attempts: u32) -> bool {
        for _ in 0..attempts {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Ok((focus_pkg, _)) = self.current_focus(serial).await {
                if focus_pkg == pkg {
                    return true;
                }
            }
        }
        false
    }

    /// 启动 App：优先通过模拟点击桌面图标启动（Launcher 发起 startActivity → getReferrer 返回 Launcher 包名
    /// → 友盟识别为「自主启动」），失败时 fallback 到 am start。
    /// label_hint 为 aapt 从 APK 资源表解析的准确显示名称，供图标精确匹配，避免落入模糊探测。
    pub async fn launch_app(&self, serial: &str, pkg: &str, user: Option<u32>, label_hint: &str) -> Result<(), AdbError> {
        // 如果 App 已经处于前台焦点，直接返回成功
        if let Ok((focus_pkg, _)) = self.current_focus(serial).await {
            if focus_pkg == pkg {
                tracing::info!("[LaunchApp] App ({}) 当前已在前台，无需重复启动", pkg);
                return Ok(());
            }
        }

        // 多用户场景无法可靠地通过桌面图标启动，直接走 am start
        if user.is_some() {
            return self.launch_via_am(serial, pkg, user).await;
        }
        match self.launch_via_icon_tap(serial, pkg, label_hint).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("[LaunchApp] 桌面图标点击启动失败 ({}), fallback 到 am start", e);
                self.launch_via_am(serial, pkg, user).await
            }
        }
    }

    /// 快速启动 App（直接 am start，用于脚本执行中途失焦后的快速拉回，不切回桌面）
    pub async fn launch_app_fast(&self, serial: &str, pkg: &str, user: Option<u32>) -> Result<(), AdbError> {
        self.launch_via_am(serial, pkg, user).await
    }

    /// 通过模拟点击桌面图标启动 App（确保 Activity.getReferrer() 返回 Launcher 包名）
    ///
    /// 流程：
    /// 1. 获取 App 显示名称（dumpsys package 取 label 或通过非系统应用候选图标探测）
    /// 2. 按 HOME 键回到桌面
    /// 3. uiautomator dump 桌面 UI 树
    /// 4. 在 UI 树中查找匹配或非系统 candidate 图标节点
    /// 5. input tap 点击该节点中心坐标并校验前台包名
    async fn launch_via_icon_tap(&self, serial: &str, pkg: &str, label_hint: &str) -> Result<(), AdbError> {
        let (sw, sh) = self.screen_size(serial).await.unwrap_or((1080, 2400));
        let cache_key = format!("{}@{}x{}", pkg, sw, sh);

        // 快速路径：命中全局图标坐标缓存 → 回桌面直接点击，跳过 dump/翻页/抽屉探测（批量提速）
        if let Some((cx, cy)) = ICON_POS_CACHE.lock().await.get(&cache_key).copied() {
            self.key_home(serial).await?;
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            self.input_tap(serial, cx, cy).await?;
            if self.wait_foreground(serial, pkg, 8).await {
                tracing::info!("[LaunchApp] 命中缓存图标坐标 ({},{}), 快速拉起 {}", cx, cy, pkg);
                return Ok(());
            }
            tracing::info!("[LaunchApp] 缓存图标坐标失效，重新全量探测");
        }

        // 1. App 显示名称：优先用 aapt 从 APK 资源表解析的明文（准确），否则设备端 dumpsys 兜底
        let label = if !label_hint.trim().is_empty() {
            label_hint.trim().to_string()
        } else {
            self.get_app_label(serial, pkg).await.unwrap_or_default()
        };
        if !label.is_empty() {
            tracing::info!("[LaunchApp] App 显示名称: '{}'", label);
        } else {
            tracing::info!("[LaunchApp] 无法获取 App label，启用桌面/抽屉候选第三方 App 图标探测");
        }

        // 2. 按 HOME 键回到桌面并等待桌面稳定
        self.key_home(serial).await?;
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        let is_system_widget_or_app = |name: &str| -> bool {
            let lower = name.to_lowercase();
            let sys = [
                "phone", "messages", "chrome", "camera", "settings", "photos",
                "play store", "gmail", "maps", "youtube", "drive", "clock",
                "contacts", "calculator", "calendar", "files", "google", "search",
                "webview", "sim toolkit", "dialer", "gallery", "music",
                "电话", "信息", "短信", "相机", "设置", "相册", "时钟", "通讯录",
                "计算器", "日历", "文件", "谷歌", "指南针", "录音机", "应用商店",
                "搜索", "浏览器", "widget", "微件", "壁纸", "wallpaper", "folder",
                "home", "overview", "workspace", "page", "mon,", "tue,", "wed,", "thu,", "fri,", "sat,", "sun,",
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
                "周一", "周二", "周三", "周四", "周五", "周六", "周日", "月", "日", "°c", "°f"
            ];
            sys.iter().any(|&s| lower == s || lower.contains(s))
        };

        // 查找匹配或候选图标（带优先级排序）
        let find_candidate_icons = |dump: &crate::uiautomation::dump::UiDump| -> Vec<(String, crate::uiautomation::dump::Bounds)> {
            let mut exact = Vec::new();
            let mut predicted = Vec::new();
            let mut other_cands = Vec::new();

            for n in &dump.nodes {
                let desc = n.content_desc.trim();
                let text = n.text.trim();
                let raw_title = if !desc.is_empty() { desc } else { text };
                if raw_title.is_empty() {
                    continue;
                }

                if n.bounds.width() < 20 || n.bounds.height() < 20 {
                    continue;
                }

                // 1. 如果有已知 label，进行精确/前缀/包含匹配
                if !label.is_empty() {
                    if raw_title == label || raw_title.starts_with(&label) || raw_title.contains(&label) {
                        exact.push((raw_title.to_string(), n.bounds));
                        continue;
                    }
                }

                // 2. 识别 Launcher 标准的 App 图标（如 "Predicted app: 晨视频" 或 "App: 晨视频"）
                if raw_title.starts_with("Predicted app: ") || raw_title.starts_with("App: ") {
                    let clean_app = raw_title
                        .trim_start_matches("Predicted app: ")
                        .trim_start_matches("App: ")
                        .trim();
                    if !is_system_widget_or_app(clean_app) {
                        predicted.push((raw_title.to_string(), n.bounds));
                        continue;
                    }
                }

                // 3. 其他非系统 Widget / 页签的候选图标
                if !is_system_widget_or_app(raw_title) && n.bounds.width() <= 500 && n.bounds.height() <= 500 {
                    other_cands.push((raw_title.to_string(), n.bounds));
                }
            }

            if !exact.is_empty() {
                return exact;
            }
            if !predicted.is_empty() {
                return predicted;
            }
            other_cands
        };

        // 阶段 1：在当前桌面查找
        let xml = self.uiautomator_dump(serial, true).await?;
        let dump = crate::uiautomation::dump::UiDump::parse(&xml).map_err(|e| AdbError::CommandFailed {
            cmd: "uiautomator dump parse".into(),
            code: -1,
            stderr: e,
        })?;

        let mut candidates = find_candidate_icons(&dump);

        // 阶段 2：桌面第一屏若无候选，左右翻页搜索
        if candidates.is_empty() {
            for swipe_i in 0..2 {
                self.input_swipe(serial, sw * 80 / 100, sh / 2, sw * 20 / 100, sh / 2, 300).await?;
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                if let Ok(xml2) = self.uiautomator_dump(serial, true).await {
                    if let Ok(dump2) = crate::uiautomation::dump::UiDump::parse(&xml2) {
                        let page_cands = find_candidate_icons(&dump2);
                        if !page_cands.is_empty() {
                            tracing::info!("[LaunchApp] 第 {} 屏桌面找到候选图标: {:?}", swipe_i + 2, page_cands);
                            candidates = page_cands;
                            break;
                        }
                    }
                }
            }
        }

        // 阶段 3：如果桌面未找到，进入应用抽屉（尝试 KEYCODE_ALL_APPS 或向上平滑拖拽）
        if candidates.is_empty() {
            self.key_home(serial).await?;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // 先尝试发送 KEYCODE_ALL_APPS (284) 原生唤起抽屉
            let _ = self.run_on(serial, &["shell", "input", "keyevent", "284"]).await;
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            // 补充从底部向上平滑拖拽手势（600ms 保证触发 Launcher3 抽屉展开阈值）
            self.input_swipe(serial, sw / 2, sh * 88 / 100, sw / 2, sh * 18 / 100, 600).await?;
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            if let Ok(xml3) = self.uiautomator_dump(serial, true).await {
                if let Ok(dump3) = crate::uiautomation::dump::UiDump::parse(&xml3) {
                    candidates = find_candidate_icons(&dump3);
                    if !candidates.is_empty() {
                        tracing::info!("[LaunchApp] 应用抽屉中找到候选图标: {:?}", candidates);
                    }
                }
            }
        }

        if candidates.is_empty() {
            return Err(AdbError::CommandFailed {
                cmd: "find_icon_on_desktop".into(),
                code: -1,
                stderr: format!("桌面及应用抽屉均未找到目标/候选图标 (pkg={})", pkg),
            });
        }

        // 依次尝试点击候选图标（坐标 clamp 限制在屏幕可见安全区域）
        for (cand_title, bounds) in candidates {
            let raw_cx = (bounds.x1 + bounds.x2) / 2;
            let raw_cy_icon = bounds.y1 + (bounds.height() * 35 / 100);
            let raw_cy_center = (bounds.y1 + bounds.y2) / 2;

            let tap_x = raw_cx.clamp(40, sw - 40);
            let tap_y1 = raw_cy_icon.clamp(40, sh - 40);
            let tap_y2 = raw_cy_center.clamp(40, sh - 40);

            tracing::info!("[LaunchApp] 尝试模拟点击图标 '{}' (bounds={:?}) 坐标: ({}, {})", cand_title, bounds, tap_x, tap_y1);
            self.input_tap(serial, tap_x, tap_y1).await?;

            // 快速检测前台焦点是否切换至目标应用
            if self.wait_foreground(serial, pkg, 7).await {
                tracing::info!("[LaunchApp] 目标 App ({}) 已通过点击图标 '{}' 成功拉起", pkg, cand_title);
                ICON_POS_CACHE.lock().await.insert(cache_key.clone(), (tap_x, tap_y1));
                return Ok(());
            }

            // 如果第一次点图标上半部没拉起，尝试点中心位置
            if tap_y2 != tap_y1 {
                tracing::info!("[LaunchApp] 重试点击中心坐标: ({}, {})", tap_x, tap_y2);
                self.input_tap(serial, tap_x, tap_y2).await?;
                if self.wait_foreground(serial, pkg, 8).await {
                    tracing::info!("[LaunchApp] 目标 App ({}) 已通过中心点击图标 '{}' 成功拉起", pkg, cand_title);
                    ICON_POS_CACHE.lock().await.insert(cache_key.clone(), (tap_x, tap_y2));
                    return Ok(());
                }
            }
        }

        Err(AdbError::CommandFailed {
            cmd: "launch_via_icon_tap".into(),
            code: -1,
            stderr: format!("点击候选图标后目标 App ({}) 未能在预期时间内切换到前台", pkg),
        })
    }

    /// 获取 App 在桌面上的显示名称（从 dumpsys package 中提取 label）
    async fn get_app_label(&self, serial: &str, pkg: &str) -> Result<String, AdbError> {
        // 方式 1：dumpsys package <pkg> 中找 "label=" 或 "Application label:"
        let out = self.shell(serial, &["dumpsys", "package", pkg]).await?;

        // 搜索 "applicationInfo" 区段中的 label
        // 典型输出: "    labelRes=0x7f0e0057 nonLocalizedLabel=MyApp icon=0x7f080001"
        for line in out.lines() {
            let trimmed = line.trim();
            if trimmed.contains("nonLocalizedLabel=") {
                // 格式: "nonLocalizedLabel=MyApp"
                if let Some(pos) = trimmed.find("nonLocalizedLabel=") {
                    let rest = &trimmed[pos + "nonLocalizedLabel=".len()..];
                    // 取到下一个空格或行尾
                    let label = rest.split_whitespace().next().unwrap_or("").trim();
                    if !label.is_empty() && label != "null" {
                        return Ok(label.to_string());
                    }
                }
            }
        }

        // 方式 2：cmd package dump <pkg> 中 "App label:" 行
        let out2 = self.shell(serial, &["cmd", "package", "dump", pkg]).await.unwrap_or_default();
        for line in out2.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("label=") {
                let label = rest.trim().trim_matches('"');
                if !label.is_empty() && label != "null" {
                    return Ok(label.to_string());
                }
            }
        }

        Err(AdbError::CommandFailed {
            cmd: "get_app_label".into(),
            code: -1,
            stderr: format!("dumpsys package 中未找到 {} 的 label", pkg),
        })
    }

    /// resolve-activity 拿 ComponentName 后标准 am start（fallback 方式）
    async fn launch_via_am(&self, serial: &str, pkg: &str, user: Option<u32>) -> Result<(), AdbError> {
        let resolved = self
            .run_on(serial, &["shell", "cmd", "package", "resolve-activity", "--brief", "-c", "android.intent.category.LAUNCHER", pkg])
            .await?;
        let component = resolved
            .lines()
            .last()
            .map(|l| l.trim().to_string())
            .filter(|s| s.contains('/') && !s.starts_with("Failure"));
        match component {
            Some(c) => {
                if let Some(u) = user {
                    let u = u.to_string();
                    self.run_on(serial, &[
                        "shell", "am", "start", "--user", &u,
                        "-a", "android.intent.action.MAIN",
                        "-c", "android.intent.category.LAUNCHER",
                        "-f", "0x10200000",
                        "--es", "android.intent.extra.REFERRER_NAME", "android-app://com.google.android.apps.nexuslauncher",
                        "--es", "android.intent.extra.REFERRER", "android-app://com.google.android.apps.nexuslauncher",
                        "-n", &c,
                    ]).await?;
                } else {
                    self.run_on(serial, &[
                        "shell", "am", "start",
                        "-a", "android.intent.action.MAIN",
                        "-c", "android.intent.category.LAUNCHER",
                        "-f", "0x10200000",
                        "--es", "android.intent.extra.REFERRER_NAME", "android-app://com.google.android.apps.nexuslauncher",
                        "--es", "android.intent.extra.REFERRER", "android-app://com.google.android.apps.nexuslauncher",
                        "-n", &c,
                    ]).await?;
                }
                Ok(())
            }
            None => Err(AdbError::CommandFailed {
                cmd: format!("launch_app({})", pkg),
                code: 251,
                stderr: format!(
                    "该 APK（{}）没有 launcher activity，无法启动——请确认是完整应用（带桌面图标）而非 SDK/library/插件包",
                    pkg
                ),
            }),
        }
    }

    pub async fn key_home(&self, serial: &str) -> Result<(), AdbError> {
        match self.run_on(serial, &["shell", "input", "keyevent", "KEYCODE_HOME"]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                // 如果 socket 断开或 device not found，尝试快速重连并重试一次
                if let Some(port_str) = serial.strip_prefix("emulator-") {
                    if let Ok(port) = port_str.parse::<u16>() {
                        let _ = self.connect(&format!("127.0.0.1:{}", port + 1)).await;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return self.run_on(serial, &["shell", "input", "keyevent", "KEYCODE_HOME"]).await.map(|_| ());
                    }
                }
                Err(e)
            }
        }
    }

    pub async fn emu_kill(&self, serial: &str) -> Result<(), AdbError> {
        let _ = self.run_on(serial, &["emu", "kill"]).await;
        Ok(())
    }

    pub async fn get_android_id(&self, serial: &str, user: Option<u32>) -> Result<String, AdbError> {
        let out = match user {
            Some(u) => {
                let u = u.to_string();
                self.run_on(serial, &["shell", "settings", "get", "--user", &u, "secure", "android_id"]).await?
            }
            None => self.run_on(serial, &["shell", "settings", "get", "secure", "android_id"]).await?,
        };
        Ok(out.trim().to_string())
    }

    // ---- v1.1 新增：多用户（L2.5）----

    pub async fn create_user(&self, serial: &str, name: &str) -> Result<u32, AdbError> {
        let out = self.run_on(serial, &["shell", "pm", "create-user", name]).await?;
        // 输出形如 "Success: created user id 10"
        out.split_whitespace()
            .last()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| AdbError::CommandFailed {
                cmd: "pm create-user".into(),
                code: -1,
                stderr: format!("无法解析用户 ID: {}", out),
            })
    }

    pub async fn start_user(&self, serial: &str, uid: u32) -> Result<(), AdbError> {
        let u = uid.to_string();
        self.run_on(serial, &["shell", "am", "start-user", &u]).await.map(|_| ())
    }

    pub async fn remove_user(&self, serial: &str, uid: u32) -> Result<(), AdbError> {
        let u = uid.to_string();
        // running 用户直接 remove-user 可能失败，先强制停用户再删
        let _ = self.run_on(serial, &["shell", "am", "stop-user", "-f", &u]).await;
        let _ = self.run_on(serial, &["shell", "pm", "remove-user", &u]).await;
        Ok(())
    }

    /// 解析 `pm list users` → [(uid, name)]
    /// 输出形如：`	UserInfo{10:dau_5554:410} running`
    pub async fn list_users(&self, serial: &str) -> Result<Vec<(u32, String)>, AdbError> {
        let out = self.run_on(serial, &["shell", "pm", "list", "users"]).await?;
        let mut users = Vec::new();
        for line in out.lines() {
            if let Some(start) = line.find("UserInfo{") {
                let rest = &line[start + "UserInfo{".len()..];
                if let Some(end) = rest.find('}') {
                    let mut parts = rest[..end].splitn(3, ':');
                    if let (Some(id), Some(name)) = (parts.next(), parts.next()) {
                        if let Ok(uid) = id.trim().parse::<u32>() {
                            users.push((uid, name.trim().to_string()));
                        }
                    }
                }
            }
        }
        Ok(users)
    }

    /// 清理所有名字以 prefix 开头的次级用户（uid != 0），先切回 owner 再逐个删。
    /// 返回删除数量。用于 L2.5 建用户前防止残留触顶 fw.max_users（模拟器默认 4）。
    pub async fn cleanup_users_by_prefix(&self, serial: &str, prefix: &str) -> Result<u32, AdbError> {
        let users = self.list_users(serial).await.unwrap_or_default();
        let mut removed = 0u32;
        // 先切回 owner，避免当前前台用户在待删列表里删不掉
        let _ = self.switch_user(serial, 0).await;
        for (uid, name) in users {
            if uid != 0 && name.starts_with(prefix) {
                self.remove_user(serial, uid).await?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    // ---- v1.1 新增：标识提取（identity 模块底层）----

    /// debug 包免 root 读应用私有目录（run-as）；失败时调用方兜底 root
    pub async fn run_as_read(&self, serial: &str, pkg: &str, pattern: &str) -> Result<String, AdbError> {
        self.run_on(serial, &["shell", &format!("run-as {} cat {}", pkg, pattern)])
            .await
    }

    pub async fn root(&self, serial: &str) -> Result<(), AdbError> {
        let _ = self.run_on(serial, &["root"]).await;
        // adb root 会断开连接并以 root 权限重启 adbd 守护进程，需等待 adbd 重新上线
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            self.run_on(serial, &["wait-for-device"]),
        )
        .await;
        Ok(())
    }

    /// ADB 协议级 remount（解除 /system 只读挂载，需 -writable-system 启动）
    /// 注意：这是 "adb remount"，不是 "adb shell remount"——两者完全不同。
    pub async fn remount(&self, serial: &str) -> Result<(), AdbError> {
        self.run_on(serial, &["remount"]).await.map(|_| ())
    }

    /// ADB 协议级 disable-verity（关闭 dm-verity 分区校验，使 remount 可写）
    pub async fn disable_verity(&self, serial: &str) -> Result<(), AdbError> {
        self.run_on(serial, &["disable-verity"]).await.map(|_| ())
    }

    /// 发送 reboot 重启指令（带 3 秒超时防卡死，因为重启会让 adbd 突然断开连接导致进程 stdout 挂起）
    pub async fn reboot(&self, serial: &str) -> Result<(), AdbError> {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            self.run_on(serial, &["reboot"]),
        )
        .await;
        Ok(())
    }

    pub async fn shell_read(&self, serial: &str, path: &str) -> Result<String, AdbError> {
        self.run_on(serial, &["shell", "cat", path]).await
    }

    // ---- UI 自动化扩展（uiautomation 模块底层，设计文档 §3/§11/§12）----

    /// exec-out 读文件原始字节（不走 pty，避免 \n → \r\n 转换破坏文本匹配，§11.7）
    pub async fn exec_out(&self, serial: &str, args: &[&str]) -> Result<Vec<u8>, AdbError> {
        let mut full: Vec<&str> = vec!["-s", serial, "exec-out"];
        full.extend_from_slice(args);
        let out = self
            .cmd()
            .args(&full)
            .output()
            .await
            .map_err(|e| AdbError::CommandFailed {
                cmd: format!("adb {}", full.join(" ")),
                code: -1,
                stderr: e.to_string(),
            })?;
        if !out.status.success() {
            return Err(AdbError::CommandFailed {
                cmd: format!("adb {}", full.join(" ")),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(out.stdout)
    }

    /// uiautomator dump UI 树（/data/local/tmp 路径，多用户/分区存储安全，§11.7）。
    /// compressed=true 用 --compressed（快但可能丢 importantForAccessibility != yes 的节点，§11.4）。
    /// 返回原始 XML 字符串。
    pub async fn uiautomator_dump(&self, serial: &str, compressed: bool) -> Result<String, AdbError> {
        const DUMP_PATH: &str = "/data/local/tmp/umeng_dau_ui.xml";
        let mut args: Vec<&str> = vec!["shell", "uiautomator", "dump"];
        if compressed {
            args.push("--compressed");
        }
        args.push(DUMP_PATH);
        let out = self.run_on(serial, &args).await;
        match &out {
            Err(e) => return Err(e.clone()),
            Ok(s) if s.contains("could not get idle state") => {
                // §11.5：持续动画导致 idle 超时
                return Err(AdbError::CommandFailed {
                    cmd: "uiautomator dump".into(),
                    code: -1,
                    stderr: "could not get idle state".into(),
                });
            }
            _ => {}
        }
        let bytes = self.exec_out(serial, &["cat", DUMP_PATH]).await?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    /// 通用 shell（返回 stdout 字符串；用于 settings/pm grant/wm 等）
    pub async fn shell(&self, serial: &str, args: &[&str]) -> Result<String, AdbError> {
        let mut full: Vec<&str> = vec!["shell"];
        full.extend_from_slice(args);
        self.run_on(serial, &full).await
    }

    pub async fn input_tap(&self, serial: &str, x: i32, y: i32) -> Result<(), AdbError> {
        let (xs, ys) = (x.to_string(), y.to_string());
        // 使用 input swipe 同点 100ms 模拟真实人手点击（避开 Compose/Splash 过滤 0ms 极速 tap 导致点击失效的问题）
        self.shell(serial, &["input", "swipe", &xs, &ys, &xs, &ys, "100"]).await.map(|_| ())
    }

    pub async fn input_swipe(
        &self,
        serial: &str,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        duration_ms: u32,
    ) -> Result<(), AdbError> {
        let (x1s, y1s, x2s, y2s, ds) = (
            x1.to_string(),
            y1.to_string(),
            x2.to_string(),
            y2.to_string(),
            duration_ms.to_string(),
        );
        self.shell(serial, &["input", "swipe", &x1s, &y1s, &x2s, &y2s, &ds])
            .await
            .map(|_| ())
    }

    pub async fn keyevent(&self, serial: &str, code: &str) -> Result<(), AdbError> {
        self.shell(serial, &["input", "keyevent", code]).await.map(|_| ())
    }

    /// 当前焦点窗口（dumpsys window mCurrentFocus）。返回 (package, activity) 若可解析。
    /// 例: "mCurrentFocus=Window{xxx u0 com.pkg/com.pkg.MainActivity}" 或 "u10 com.pkg/..."
    pub async fn current_focus(&self, serial: &str) -> Result<(String, String), AdbError> {
        let parse_line = |line: &str| -> Option<(String, String)> {
            if !line.contains("mCurrentFocus") { return None; }
            // 匹配 " u<digits> " 后面的 "pkg/activity"
            let idx = line.find(" u")?;
            let rest = &line[idx + 2..];
            let space_idx = rest.find(' ')?;
            let user_str = &rest[..space_idx];
            if !user_str.chars().all(|c| c.is_ascii_digit()) { return None; }
            let tail = &rest[space_idx + 1..];
            let end = tail.find('}').unwrap_or(tail.len());
            let comp = tail[..end].trim();
            let slash = comp.find('/')?;
            Some((comp[..slash].to_string(), comp[slash + 1..].to_string()))
        };

        let out = self.shell(serial, &["dumpsys", "window", "windows"]).await?;
        for line in out.lines() {
            if let Some(res) = parse_line(line.trim()) {
                return Ok(res);
            }
        }
        let out2 = self.shell(serial, &["dumpsys", "window"]).await?;
        for line in out2.lines() {
            if let Some(res) = parse_line(line.trim()) {
                return Ok(res);
            }
        }
        Ok((String::new(), String::new()))
    }

    /// 当前前台用户 id（am get-current-user）
    pub async fn get_current_user(&self, serial: &str) -> Result<u32, AdbError> {
        let out = self.shell(serial, &["am", "get-current-user"]).await?;
        out.trim().parse::<u32>().map_err(|_| AdbError::CommandFailed {
            cmd: "am get-current-user".into(),
            code: -1,
            stderr: format!("无法解析: {}", out),
        })
    }

    /// 切换前台用户（§11.8：L2.5 必须 switch-user，start-user 只后台拉起）
    pub async fn switch_user(&self, serial: &str, uid: u32) -> Result<(), AdbError> {
        let u = uid.to_string();
        self.shell(serial, &["am", "switch-user", &u]).await.map(|_| ())
    }

    /// 等待某用户成为前台用户（switch-user 后 launcher 冷启动，需等稳定）
    pub async fn wait_user_foreground(
        &self,
        serial: &str,
        uid: u32,
        timeout: Duration,
    ) -> Result<(), AdbError> {
        let start = std::time::Instant::now();
        loop {
            if let Ok(cur) = self.get_current_user(serial).await {
                if cur == uid {
                    return Ok(());
                }
            }
            if start.elapsed() >= timeout {
                return Err(AdbError::BootTimeout {
                    serial: serial.into(),
                    waited: timeout.as_secs() as u32,
                });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// 设置全局 settings（§11.5 关动画用）
    pub async fn settings_put_global(&self, serial: &str, key: &str, value: &str) -> Result<(), AdbError> {
        self.shell(serial, &["settings", "put", "global", key, value])
            .await
            .map(|_| ())
    }

    /// pm grant 预授权（§11.1）。失败返回 Err，调用方决定是否容忍。
    pub async fn pm_grant(&self, serial: &str, pkg: &str, perm: &str, user: Option<u32>) -> Result<(), AdbError> {
        match user {
            Some(u) => {
                let us = u.to_string();
                self.shell(serial, &["pm", "grant", "--user", &us, pkg, perm]).await?;
            }
            None => {
                self.shell(serial, &["pm", "grant", pkg, perm]).await?;
            }
        }
        Ok(())
    }

    /// 失败截图（exec-out screencap -p，不走 pty 保 PNG 完整，§15.7）
    pub async fn screencap_png(&self, serial: &str) -> Result<Vec<u8>, AdbError> {
        self.exec_out(serial, &["screencap", "-p"]).await
    }

    /// 屏幕分辨率 (w, h)。优先解析 "Override size: ...", 其次 "Physical size: ..."
    pub async fn screen_size(&self, serial: &str) -> Result<(i32, i32), AdbError> {
        let out = self.shell(serial, &["wm", "size"]).await?;
        let mut physical = None;
        let mut override_size = None;
        for line in out.lines() {
            let t = line.trim();
            if let Some(pos) = t.rfind(':') {
                let tail = t[pos + 1..].trim();
                if let Some(x) = tail.find('x') {
                    let w = tail[..x].trim().parse::<i32>().unwrap_or(0);
                    let h = tail[x + 1..].trim().parse::<i32>().unwrap_or(0);
                    if w > 0 && h > 0 {
                        if t.to_lowercase().starts_with("override") {
                            override_size = Some((w, h));
                        } else {
                            physical = Some((w, h));
                        }
                    }
                }
            }
        }
        if let Some(s) = override_size {
            return Ok(s);
        }
        if let Some(s) = physical {
            return Ok(s);
        }
        Ok((1080, 2400)) // 常见默认值兜底
    }

    /// 屏幕密度 dpi。优先解析 "Override density: ...", 其次 "Physical density: ..."
    pub async fn density_dpi(&self, serial: &str) -> Result<i32, AdbError> {
        let out = self.shell(serial, &["wm", "density"]).await?;
        let mut physical = None;
        let mut override_dpi = None;
        for line in out.lines() {
            let t = line.trim();
            if let Some(pos) = t.rfind(':') {
                if let Ok(d) = t[pos + 1..].trim().parse::<i32>() {
                    if t.to_lowercase().starts_with("override") {
                        override_dpi = Some(d);
                    } else {
                        physical = Some(d);
                    }
                }
            }
        }
        if let Some(d) = override_dpi {
            return Ok(d);
        }
        if let Some(d) = physical {
            return Ok(d);
        }
        Ok(420)
    }
}

fn is_device_offline_or_not_found(err: &AdbError) -> bool {
    match err {
        AdbError::DeviceOffline { .. } => true,
        AdbError::CommandFailed { stderr, .. } => {
            let s = stderr.to_lowercase();
            s.contains("device offline")
                || s.contains("device not found")
                || (s.contains("device") && (s.contains("not found") || s.contains("offline")))
        }
        _ => false,
    }
}
