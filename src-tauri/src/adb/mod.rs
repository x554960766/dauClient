//! adb 客户端（设计文档 §5）
//! v1.1 P0-1：所有进程统一注入私有 adb server 环境变量，与用户的
//! Android Studio / 全局 adb server 完全并存，互不干扰。

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

    async fn run_on(&self, serial: &str, args: &[&str]) -> Result<String, AdbError> {
        let mut full: Vec<&str> = vec!["-s", serial];
        full.extend_from_slice(args);
        match self.run(&full).await {
            Err(AdbError::CommandFailed { ref stderr, .. }) if stderr.contains("device offline") => {
                Err(AdbError::DeviceOffline { serial: serial.to_string() })
            }
            other => other,
        }
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

    /// 等待启动完成（boot_completed=1），等价脚本 wait_boot
    pub async fn wait_boot(&self, serial: &str, timeout: Duration) -> Result<(), AdbError> {
        tokio::time::timeout(timeout, async {
            let _ = self.run_on(serial, &["wait-for-device"]).await;
            loop {
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
    pub async fn wait_net(&self, serial: &str) -> Result<(), AdbError> {
        for _ in 0..15 {
            if self
                .run_on(serial, &["shell", "ping", "-c", "1", "-W", "2", "223.5.5.5"])
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(AdbError::NetTimeout { serial: serial.into() })
    }

    pub async fn install(&self, serial: &str, apk: &Path, user: Option<u32>) -> Result<(), AdbError> {
        let apk_s = apk.display().to_string();
        match user {
            Some(u) => {
                let u = u.to_string();
                self.run_on(serial, &["install", "--user", &u, "-r", &apk_s]).await?;
            }
            None => {
                self.run_on(serial, &["install", "-r", &apk_s]).await?;
            }
        }
        Ok(())
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

    /// 启动 App。v1.1：多用户场景 monkey 不支持 --user，
    /// 改用 resolve-activity 拿 ComponentName 后 am start --user。
    /// v1.2：无 user 分支 monkey 失败时 fallback 到 am start，
    /// 并对「没有 launcher activity」给出清晰错误（而非裸 monkey exit 251）。
    pub async fn launch_app(&self, serial: &str, pkg: &str, user: Option<u32>) -> Result<(), AdbError> {
        match user {
            None => {
                // 优先 monkey（更接近用户点图标的行为）
                let monkey_ok = self
                    .run_on(serial, &["shell", "monkey", "-p", pkg, "-c", "android.intent.category.LAUNCHER", "1"])
                    .await
                    .is_ok();
                if monkey_ok {
                    return Ok(());
                }
                // monkey 失败 → fallback: resolve-activity + am start
                self.launch_via_am(serial, pkg, None).await?;
            }
            Some(u) => {
                self.launch_via_am(serial, pkg, Some(u)).await?;
            }
        }
        Ok(())
    }

    /// resolve-activity 拿 ComponentName 后 am start；无 launcher activity 时给清晰错误
    async fn launch_via_am(&self, serial: &str, pkg: &str, user: Option<u32>) -> Result<(), AdbError> {
        let resolved = self
            .run_on(serial, &["shell", "cmd", "package", "resolve-activity", "--brief", "-c", "android.intent.category.LAUNCHER", pkg])
            .await?;
        // --brief 输出最后一行是 component（如 com.pkg/com.pkg.MainActivity）；
        // 找不到时输出 "No activity found" 或空，不含 '/'
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
                        "-n", &c,
                    ]).await?;
                } else {
                    self.run_on(serial, &[
                        "shell", "am", "start",
                        "-a", "android.intent.action.MAIN",
                        "-c", "android.intent.category.LAUNCHER",
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
        self.run_on(serial, &["shell", "input", "keyevent", "KEYCODE_HOME"]).await.map(|_| ())
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

    /// 屏幕分辨率 (w, h)。解析 "Physical size: 1080x2400"
    pub async fn screen_size(&self, serial: &str) -> Result<(i32, i32), AdbError> {
        let out = self.shell(serial, &["wm", "size"]).await?;
        for line in out.lines() {
            if let Some(pos) = line.rfind(':') {
                let tail = line[pos + 1..].trim();
                if let Some(x) = tail.find('x') {
                    let w = tail[..x].trim().parse::<i32>().unwrap_or(0);
                    let h = tail[x + 1..].trim().parse::<i32>().unwrap_or(0);
                    if w > 0 && h > 0 {
                        return Ok((w, h));
                    }
                }
            }
        }
        Ok((1080, 2400)) // 常见默认值兜底
    }

    /// 屏幕密度 dpi。解析 "Physical density: 420"
    pub async fn density_dpi(&self, serial: &str) -> Result<i32, AdbError> {
        let out = self.shell(serial, &["wm", "density"]).await?;
        for line in out.lines() {
            if let Some(pos) = line.rfind(':') {
                if let Ok(d) = line[pos + 1..].trim().parse::<i32>() {
                    return Ok(d);
                }
            }
        }
        Ok(420)
    }
}
