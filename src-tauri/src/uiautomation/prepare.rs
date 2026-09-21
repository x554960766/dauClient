//! prepare_device / teardown_device（设计文档 §12.2）
//! 把所有环境修正（锁屏/动画/装包/pm grant/多用户切换）收口成一个函数，
//! 所有重置级别、所有阶段共用，避免散落各处漏做。

use crate::adb::{Adb, AdbError};
use crate::uiautomation::script::PrepareConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;


#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrepareOutcome {
    /// L2.5 多用户场景的用户 id
    pub user_id: Option<u32>,
    /// 成功授予的运行时权限
    pub granted: Vec<String>,
    /// grant 失败的权限（normal 级权限会报错，属正常）及原因
    pub grant_failed: Vec<(String, String)>,
    pub keyguard_dismissed: bool,
    pub animations_disabled: bool,
}

/// 从 aapt dump permissions 输出枚举 uses-permission（§12.2）
/// 简化策略：全部尝试 grant，失败的记录进 grant_failed，不阻断。
pub fn extract_permissions(aapt_out: &str) -> Vec<String> {
    aapt_out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("uses-permission: name='"))
        .filter_map(|l| l.split('\'').next())
        .map(str::to_string)
        .collect()
}

/// 环境准备主流程。调用前提：设备已 boot（L3 由 boot_emu 完成等开机；L1/L2/L2.5 由调用方保证）。
/// 本函数负责：switch-user（L2.5）→ 解锁屏 → 关动画 → pm grant 预授权。
/// 装包由调用方在适当位置执行（L2 需先 uninstall 再 install，L3 在 wipe 后 install）。
pub async fn prepare_device(
    adb: &Adb,
    serial: &str,
    user_id: Option<u32>,
    cfg: &PrepareConfig,
) -> Result<PrepareOutcome, AdbError> {
    let mut out = PrepareOutcome {
        user_id,
        ..Default::default()
    };

    // ---- 1. 多用户：switch-user 到前台（§11.8：仅 start-user 不够，UI 自动化作用于前台用户）----
    if let Some(uid) = user_id {
        adb.switch_user(serial, uid).await?;
        // 切用户后 launcher 冷启动，必须等稳定，否则后续 dump 抓到的是过渡态
        adb.wait_user_foreground(serial, uid, Duration::from_secs(cfg.user_foreground_timeout_s as u64))
            .await?;
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }

    // ---- 2. 解锁屏（§11.6：wipe-data 后常停在滑动锁屏，此时 launch 看似成功但 dump 抓到 systemui）----
    if cfg.dismiss_keyguard {
        let _ = adb.keyevent(serial, "KEYCODE_WAKEUP").await;
        let _ = adb.keyevent(serial, "82").await;
        let _ = adb.input_swipe(serial, 540, 1800, 540, 500, 200).await;
        match adb.shell(serial, &["wm", "dismiss-keyguard"]).await {
            Ok(_) => out.keyguard_dismissed = true,
            Err(_) => {}
        }
    }

    // ---- 3. 关全局动画（§11.5：L3 wipe 后被重置，必须每次重设）----
    if cfg.disable_animations {
        let mut ok = true;
        for (k, v) in [
            ("window_animation_scale", "0"),
            ("transition_animation_scale", "0"),
            ("animator_duration_scale", "0"),
        ] {
            if adb.settings_put_global(serial, k, v).await.is_err() {
                ok = false;
            }
        }
        out.animations_disabled = ok;
    }

    // ---- 3.5 注入 SIM 卡在位状态、4G/5G 运营商属性与传感器脉冲（避免友盟等 SDK 归类为其他未识别网络）----
    inject_telephony_and_sensors(adb, serial).await;

    // ---- 3.6 同步真机分辨率与 DPI（通过 wm size / density 动态覆盖，使友盟等统计 SDK 读取到多样化真实分辨率）----
    sync_display_resolution(adb, serial).await;

    // ---- 4. 关闭系统沉浸式模式教学提示（v1.3 M3 实测踩坑 + v1.5 L2.5 per-user 修正）----
    // App 进入全屏沉浸模式时系统弹一次性 cling（"Viewing full screen / Got it"），
    // 顶层 package=android 盖在最上层，挡死所有 App 内关卡检测。
    // 用 settings 预置 confirmed 直接让它不出现。
    //
    // ⚠️ secure 命名空间是 **per-user** 的：不带 --user 只写进 user 0。
    // L2.5 多用户方案里 App 跑在新建用户下 → user 0 的预置对它无效 →
    // 新用户 App 全屏时仍反复弹 cling，onboarding 每轮点它却到不了主页 →
    // 活锁耗尽 max_rounds → 报「未到主页: fail_reason=None」。
    // 修复：显式对目标用户写一份（settings 的 --user 是全局选项，须放在 put 之前），
    // 同时保留 user 0 那份做双保险（无害）。
    let _ = adb
        .shell(serial, &["settings", "put", "secure", "immersive_mode_confirmations", "confirmed"])
        .await;
    let _ = adb
        .shell(serial, &["settings", "put", "secure", "anr_show_background", "0"])
        .await;
    let _ = adb
        .shell(serial, &["settings", "put", "global", "hide_error_dialogs", "1"])
        .await;
    let _ = adb
        .shell(serial, &["settings", "put", "secure", "show_first_crash_dialog", "0"])
        .await;
    if let Some(uid) = user_id {
        let uid_s = uid.to_string();
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "secure", "immersive_mode_confirmations", "confirmed"],
            )
            .await;
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "secure", "anr_show_background", "0"],
            )
            .await;
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "secure", "show_first_crash_dialog", "0"],
            )
            .await;
        // 标记该副用户已 100% 初始化完成与准备就绪，彻底解除系统后台 UserSetup 磁盘与服务锁（防 ANR）
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "secure", "user_setup_complete", "1"],
            )
            .await;
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "secure", "device_provisioned", "1"],
            )
            .await;
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "global", "device_provisioned", "1"],
            )
            .await;
    }

    Ok(out)
}

/// pm grant 预授权（§11.1：装包后、启动前执行，消灭系统权限弹窗关卡 4）。
/// perms 来自 extract_permissions + cfg.extra_permissions；容忍单条失败。
pub async fn grant_permissions(
    adb: &Adb,
    serial: &str,
    pkg: &str,
    perms: &[String],
    user_id: Option<u32>,
    out: &mut PrepareOutcome,
) {
    // 如果是 L2.5 多用户模式，同步对 AppOps 敏感权限全量放行（防止副用户读 TelephonyManager/ANDROID_ID 报 SecurityException 阻塞 UI 主线程）
    if let Some(uid) = user_id {
        let u_str = uid.to_string();
        for op in &[
            "READ_PHONE_STATE",
            "GET_USAGE_STATS",
            "SYSTEM_ALERT_WINDOW",
            "WRITE_SETTINGS",
            "MANAGE_EXTERNAL_STORAGE",
            "READ_EXTERNAL_STORAGE",
            "WRITE_EXTERNAL_STORAGE",
            "ACCESS_FINE_LOCATION",
            "ACCESS_COARSE_LOCATION",
        ] {
            let _ = adb.shell(serial, &["appops", "set", "--user", &u_str, pkg, op, "allow"]).await;
        }
    }

    for perm in perms {
        match adb.pm_grant(serial, pkg, perm, user_id).await {
            Ok(_) => out.granted.push(perm.clone()),
            Err(e) => out
                .grant_failed
                .push((perm.clone(), format!("{}", e))),
        }
    }
}

/// L2.5 收尾对称处理（§12.2：pm remove-user 删不掉当前前台用户，必须先切回 user 0）
pub async fn teardown_device(adb: &Adb, serial: &str, out: &PrepareOutcome) {
    if let Some(uid) = out.user_id {
        let _ = adb.switch_user(serial, 0).await;
        let _ = adb
            .wait_user_foreground(serial, 0, Duration::from_secs(30))
            .await;
        let _ = adb.remove_user(serial, uid).await;
    }
}

use crate::avd::{BootOpts, Emulator};

/// 终极单启动方案：真机品牌、型号、指纹与硬件参数已在开机前由 config.ini 与 QEMU 命令行 -prop 注入，
/// 随 Android init 启动即为真机环境，彻底消除 Android 14 只读分区的 remount 失败与冗余的二次/三次重启！
/// 此处仅需同步屏幕物理分辨率与 DPI（耗时仅 ~0.2s）。
pub async fn apply_device_spoofing(
    adb: &Adb,
    _emulator: &Emulator,
    _avd_name: &str,
    port: u16,
    _opts: &BootOpts,
) {
    let serial = format!("emulator-{}", port);
    let model = adb.shell(&serial, &["getprop", "ro.product.model"]).await.unwrap_or_default();
    let fp = adb.shell(&serial, &["getprop", "ro.system.build.fingerprint"]).await.unwrap_or_default();
    tracing::info!(
        "[DeviceSpoof] ⚡ 真机硬件属性已由 config.ini 与 -prop 生效 (model={}, fp={})，单启动秒级同步屏幕物理分辨率与 DPI: serial={}",
        model.trim(), fp.trim(), serial
    );
    sync_display_resolution(adb, &serial).await;
}

/// 同步屏幕分辨率与 DPI 到匹配当前设备型号的真机参数（通过 wm size 与 wm density 动态覆盖）
pub async fn sync_display_resolution(adb: &Adb, serial: &str) {
    sync_display_resolution_with_profile(adb, serial, None).await;
}

pub async fn sync_display_resolution_with_profile(
    adb: &Adb,
    serial: &str,
    custom_info: Option<&crate::avd::DeviceProfileInfo>,
) {
    let profile = match custom_info {
        Some(info) => info.clone(),
        None => {
            // 若系统已被 reset_device_in_place 等设置了自定义 Override size，跳过避免覆盖
            if let Ok(out) = adb.shell(serial, &["wm", "size"]).await {
                if out.contains("Override size:") {
                    return;
                }
            }
            let model = adb.shell(serial, &["getprop", "ro.product.model"]).await.unwrap_or_default();
            let model = model.trim();
            crate::avd::find_profile_by_model(model).unwrap_or_else(crate::avd::random_device_info)
        }
    };

    tracing::info!(
        "[DisplaySync] 正在为 {} 同步屏幕分辨率与 DPI: model={}, size={}x{}, density={}",
        serial, profile.model, profile.width, profile.height, profile.density
    );

    let size_str = format!("{}x{}", profile.width, profile.height);
    let density_str = profile.density.to_string();

    let _ = adb.shell(serial, &["wm", "size", &size_str]).await;
    let _ = adb.shell(serial, &["wm", "density", &density_str]).await;
}

/// 注入 SIM 卡在位状态、传感器脉冲，并按 60% 4G (LTE) / 40% Wi-Fi 动态分配网络环境
/// 注入 SIM 卡在位状态、传感器脉冲，并按 60% 4G (LTE) / 40% Wi-Fi 动态分配网络环境。
/// 性能优化：把网络切换 + 15 条 setprop + 传感器脉冲合并成一次 `adb shell sh -c` 批量执行，
/// 从 ~19 次串行 adb 往返（~19s）降到 1 次（~1.5s）。4G 路径的 `adb emu gsm` 非 shell 命令，单独发。
pub async fn inject_telephony_and_sensors(adb: &Adb, serial: &str) {
    // 随机在中国三大运营商中轮换 (中国移动 46000, 中国联通 46001, 中国电信 46011)
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let carriers = [
        ("46000", "中国移动", "CMCC", "cn"),
        ("46001", "中国联通", "CUCC", "cn"),
        ("46011", "中国电信", "CTCC", "cn"),
    ];
    let (num, alpha, short_name, country) = carriers[(nanos as usize) % carriers.len()];

    // 60% 4G 蜂窝移动网络 vs 40% Wi-Fi 网络分配
    let is_wifi = ((nanos / 1000) % 100) < 40;

    // 常用家庭与企业拟真 Wi-Fi 名称，规避官方模拟器特征明显的 "AndroidWifi"
    let wifi_names = ["TP-LINK_5G_68F2", "Xiaomi_WiFi6_Plus", "ChinaNet-Fast5G", "HUAWEI_Home_WiFi", "MERCURY_5G_C890"];
    let chosen_ssid = wifi_names[(nanos as usize) % wifi_names.len()];

    // ---- 组装批量 shell 命令：网络模式切换 + SIM/运营商 setprop + 传感器脉冲 ----
    // 关键修正：QEMU 模拟器的用户态网络与 -http-proxy 完全依赖虚拟网络桥接，
    // 绝对不能执行 `svc wifi disable`，否则会导致网络路由瘫痪（os error 65 No route to host）。
    // 在 4G 模式下，仅通过 setprop / telephony 状态属性伪装蜂窝基站，同时保持网络通道畅通。
    let mut cmds: Vec<String> = Vec::new();
    cmds.push("svc wifi enable".into());
    cmds.push("svc data enable".into());
    cmds.push("settings put global mobile_data 1".into());

    // 关键优化：开启 Android 系统低流量保护模式，禁止系统后台在手机热点下偷跑无用数据
    cmds.push("settings put global captive_portal_mode 0".into());
    cmds.push("settings put global captive_portal_detection_enabled 0".into());
    cmds.push("settings put global auto_sync 0".into());
    cmds.push("settings put global data_saver_enabled 1".into());
    cmds.push("settings put global low_power 1".into());
    cmds.push("cmd netpolicy set restrict-background true".into());

    // 关键防漏：封死 QUIC (UDP 443) 协议穿透！
    // 现代广告 SDK 和视频 CDN 会默认走 UDP 443 绕过 HTTP 代理直连外网，单次偷跑 10~20MB。
    // 封堵 UDP 443 后，网络库（OkHttp/Chromium/Cronet）自动平滑降级至 TCP HTTPS，100% 走 CountingProxy 接受黑名单审查！
    // （注：Google 服务流量已在代理层由 TLS SNI 毫秒级阻断，下行流量为 0 B，严禁在系统层使用 pm disable-user 以免破坏 Android 14 system_server 稳定性）
    cmds.push("iptables -I OUTPUT -p udp --dport 443 -j DROP 2>/dev/null || true".into());
    cmds.push("ip6tables -I OUTPUT -p udp --dport 443 -j DROP 2>/dev/null || true".into());

    if is_wifi {
        cmds.push("settings put global wifi_on 1".into());
        cmds.push(format!("settings put global wifi_ssid \"{}\"", chosen_ssid));
        cmds.push(format!("settings put global wifi_connected_ssid \"{}\"", chosen_ssid));
        cmds.push(format!("setprop net.wifi.ssid \"{}\"", chosen_ssid));
        cmds.push("setprop wlan.driver.status ok".into());
    } else {
        cmds.push("settings put global mobile_data_always_on 1".into());
        cmds.push("setprop gsm.network.type LTE".into());
    }

    // 无论 Wi-Fi 还是 4G，手机均具备 SIM 卡和蜂窝基站状态属性
    let props = [
        ("gsm.sim.state", "5,5"),
        ("gsm.sim.operator.numeric", num),
        ("gsm.sim.operator.alpha", alpha),
        ("gsm.sim.operator.iso-country", country),
        ("gsm.operator.numeric", num),
        ("gsm.operator.alpha", alpha),
        ("gsm.operator.iso-country", country),
        ("gsm.operator.isroaming", "false"),
        ("gsm.network.type", "LTE,LTE"),
        ("gsm.voice.network.type", "LTE,LTE"),
        ("gsm.data.network.type", "LTE,LTE"),
        ("ril.data.network.type", "13"),
        ("telephony.lteOnCdmaDevice", "1"),
        ("gsm.current.phone-type", "1"),
        ("persist.radio.network_mode", "9"),
        ("gsm.signal.strength", "31,99"),
    ];
    for (k, v) in &props {
        cmds.push(format!("setprop {} {}", k, v));
    }

    // 触发传感器重力加速度模拟脉冲（让 SensorManager 不处于完全死的空置状态）
    cmds.push("sensor set acceleration 0.15:9.80:0.35".into());

    // 一次 adb shell 往返执行全部命令（每条失败容忍，不影响后续）
    let batch = cmds.join("; ");
    let _ = adb.shell(serial, &["sh", "-c", &batch]).await;

    // 4G 路径额外向 QEMU Modem 下发 LTE 切换指令（adb emu 子命令，非 shell，单独发）
    if !is_wifi {
        let _ = adb.run_on(serial, &["emu", "gsm", "data", "lte"]).await;
        let _ = adb.run_on(serial, &["emu", "gsm", "voice", "lte"]).await;
        let _ = adb.run_on(serial, &["emu", "gsm", "status", "home"]).await;
        let _ = adb.run_on(serial, &["emu", "gsm", "signal-bars", "4"]).await;
    }

    tracing::info!(
        "[NetworkSpoof] 为 {} 配置网络模式: {} (SSID: {}, 运营商: {} - {})，批量注入完成",
        serial,
        if is_wifi { "Wi-Fi" } else { "4G LTE" },
        if is_wifi { chosen_ssid } else { "None (Cellular)" },
        alpha,
        short_name
    );
}

/// 极速单启动 L3 重置（In-place Clean Reset）：
/// 彻底抹除旧 App 数据、外部存储持久缓存与 SSAID，并通过重启 system_server 触发系统生成全新合法 ANDROID_ID (SSAID)，
/// 耗时仅 ~2-3s，同时 100% 保留已注入的真机 build.prop 与指纹。
pub async fn reset_device_in_place(
    adb: &Adb,
    serial: &str,
    pkg: &str,
    custom_info: Option<&crate::avd::DeviceProfileInfo>,
) -> Result<(), AdbError> {
    tracing::info!("[L3FastReset] 正在对 {} 执行单启动极速出厂重置...", serial);

    // 0. 确保具备 root 权限以修改 /data/system/users/0/
    let _ = adb.root(serial).await;

    // 1. 强杀应用进程并卸载清理
    let _ = adb.shell(serial, &["am", "force-stop", pkg]).await;
    let _ = adb.shell(serial, &["pm", "clear", pkg]).await;
    let _ = adb.shell(serial, &["pm", "uninstall", pkg]).await;

    // 2. 清理应用私有目录与 SSAID 配置文件，以及友盟外部存储持久缓存
    let clean_cmd = format!(
        "rm -rf /data/data/{pkg} /data/user/0/{pkg} /data/user_de/0/{pkg} /sdcard/Android/data/{pkg} /sdcard/.um /sdcard/.utm; \
         rm -rf /sdcard/DCIM /sdcard/Pictures /sdcard/Download; \
         rm -f /data/system/users/0/settings_ssaid*; \
         sync",
        pkg = pkg
    );
    let _ = adb.shell(serial, &["sh", "-c", &clean_cmd]).await;

    // 3. 生成全新合法的 16 位十六进制 ANDROID_ID 并写入 secure 设置作为全局兜底
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let rand_val: u64 = ((nanos ^ (nanos >> 32)) as u64) ^ 0xa341316cbee9u64;
    let new_android_id = format!("{:016x}", rand_val);
    let _ = adb.shell(serial, &["settings", "put", "secure", "android_id", &new_android_id]).await;

    // 4. 重启 system_server 触发 SettingsProvider 重新生成全新的 random userkey
    // 耗时仅 ~1-3s，确保下次应用安装与启动时，Android 系统为其自动分配独一无二的全新合法 SSAID
    let _ = adb.shell(serial, &["setprop", "sys.boot_completed", "0"]).await;
    let _ = adb.shell(serial, &["killall", "-9", "system_server"]).await;

    let wait_start = std::time::Instant::now();
    let mut booted = false;
    tokio::time::sleep(Duration::from_millis(500)).await;
    while wait_start.elapsed() < Duration::from_secs(12) {
        if let Ok(out) = adb.shell(serial, &["getprop", "sys.boot_completed"]).await {
            if out.trim() == "1" {
                booted = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    if booted {
        tracing::info!("[L3FastReset] system_server 重启就绪，耗时 {:?}", wait_start.elapsed());
    } else {
        tracing::warn!("[L3FastReset] 等待 system_server 重启超时 (12s)，继续尝试执行");
    }

    // 5. 若指定了全新机型 profile，同步该机型的物理分辨率与 DPI
    if let Some(profile) = custom_info {
        let size_str = format!("{}x{}", profile.width, profile.height);
        let density_str = profile.density.to_string();
        let _ = adb.shell(serial, &["wm", "size", &size_str]).await;
        let _ = adb.shell(serial, &["wm", "density", &density_str]).await;
        tracing::info!(
            "[L3FastReset] 已同步机型分辨率与 DPI: {}x{}, density={}",
            profile.width, profile.height, profile.density
        );
    }

    let _ = adb.shell(serial, &["sync"]).await;

    tracing::info!("[L3FastReset] 单启动极速重置完成，系统已生成全新身份且真机指纹完好");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_permissions() {
        let aapt = r#"package: com.xxx.app
uses-permission: name='android.permission.INTERNET'
uses-permission: name='android.permission.ACCESS_COARSE_LOCATION'
uses-permission-sdk-23: name='android.permission.ACCESS_FINE_LOCATION'
sdkVersion: '24'
"#;
        let perms = extract_permissions(aapt);
        assert_eq!(perms.len(), 2);
        assert!(perms.contains(&"android.permission.INTERNET".to_string()));
        assert!(perms.contains(&"android.permission.ACCESS_COARSE_LOCATION".to_string()));
        // uses-permission-sdk-23 前缀不匹配 strip_prefix("uses-permission: name='")，跳过属预期
    }
}
