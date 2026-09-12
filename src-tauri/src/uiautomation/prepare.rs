//! prepare_device / teardown_device（设计文档 §12.2）
//! 把所有环境修正（锁屏/动画/装包/pm grant/多用户切换）收口成一个函数，
//! 所有重置级别、所有阶段共用，避免散落各处漏做。

use crate::adb::{Adb, AdbError};
use crate::uiautomation::script::PrepareConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 从 build.prop 文件文本中解析指定属性值。
/// 用于 apply_device_spoofing 的早退判断：读文件（反映是否真正改写过），
/// 不读 getprop（getprop 会被 boot 时 -prop 注入污染，不反映文件状态）。
fn prop_from_file(build_prop: &str, key: &str) -> String {
    let prefix = format!("{}=", key);
    for line in build_prop.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix(&prefix) {
            return v.trim().to_string();
        }
    }
    String::new()
}

/// 读取 system 分区的 build.prop 文件内容，尝试多个可能路径。
/// Android 14 动态分区设备上可能位于 /system/build.prop, /system/system/build.prop 或 /system/etc/build.prop。
async fn read_system_build_prop(adb: &Adb, serial: &str) -> String {
    for path in ["/system/build.prop", "/system/system/build.prop", "/system/etc/build.prop", "/vendor/build.prop", "/product/build.prop"] {
        if let Ok(content) = adb.shell(serial, &["cat", path]).await {
            let t = content.trim();
            if !t.is_empty() && !t.contains("No such file") && !t.contains("Permission denied") {
                return t.to_string();
            }
        }
    }
    String::new()
}

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
    if let Some(uid) = user_id {
        let uid_s = uid.to_string();
        let _ = adb
            .shell(
                serial,
                &["settings", "--user", &uid_s, "put", "secure", "immersive_mode_confirmations", "confirmed"],
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

/// 方案 1：通过 adb root + disable-verity + remount + 修正 build.prop + 平滑重启 QEMU 进程覆盖设备品牌与型号
pub async fn apply_device_spoofing(
    adb: &Adb,
    emulator: &Emulator,
    avd_name: &str,
    port: u16,
    opts: &BootOpts,
) {
    let serial = format!("emulator-{}", port);
    tracing::info!("[DeviceSpoof] 开始检查设备指纹: serial={}", serial);

    // 优先从 opts.props 中获取指定的硬件属性（如档案回放指定的型号/指纹），否则随机生成
    let (props, _profile_info) = if !opts.props.is_empty() {
        let model = opts.props.iter().find(|(k, _)| k == "ro.product.model").map(|(_, v)| v.as_str()).unwrap_or("");
        let prof = crate::avd::find_profile_by_model(model);
        (opts.props.clone(), prof)
    } else {
        let info = crate::avd::random_device_info();
        let props = crate::avd::device_props_from_info(&info);
        (props, Some(info))
    };

    // 首先尝试 adb root 以便读取并验证 system 分区文件真实状态
    let _ = adb.root(&serial).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // 早退判断读 build.prop 文件内容（反映是否真正改写过）。Android 14 动态分区尝试多个路径。
    let sys_prop = read_system_build_prop(adb, &serial).await;
    let mut cur_fp = prop_from_file(&sys_prop, "ro.system.build.fingerprint");
    let mut cur_model = prop_from_file(&sys_prop, "ro.product.model");

    if cur_model.is_empty() {
        cur_model = adb.shell(&serial, &["getprop", "ro.product.model"]).await.unwrap_or_default().trim().to_string();
    }
    if cur_fp.is_empty() {
        cur_fp = adb.shell(&serial, &["getprop", "ro.system.build.fingerprint"]).await.unwrap_or_default().trim().to_string();
    }

    tracing::info!("[DeviceSpoof] build.prop 文件指纹: {}, 型号: {}", cur_fp, cur_model);

    // 提取期望的目标型号
    let target_model = props.iter().find(|(k, _)| k == "ro.product.model").map(|(_, v)| v.as_str()).unwrap_or("");

    // 只有当 build.prop 文件里的指纹/型号已是真机且匹配目标时才跳过改写（单启动秒级复用）
    if !cur_fp.is_empty()
        && !cur_fp.contains("sdk_gphone")
        && !cur_fp.contains("google")
        && !cur_model.contains("sdk_gphone")
        && (target_model.is_empty() || cur_model == target_model)
    {
        tracing::info!("[DeviceSpoof] ⚡ build.prop 已是目标真机指纹（{}），跳过二次改写与重启", cur_model);
        sync_display_resolution(adb, &serial).await;
        return;
    }

    tracing::info!("[DeviceSpoof] 待注入的真机属性: {:?}", props);

    // Step 2: disable-verity & remount
    tokio::time::sleep(Duration::from_millis(2000)).await;

    // Step 2: disable-verity & remount
    let dv_res = adb.disable_verity(&serial).await;
    tracing::info!("[DeviceSpoof] disable-verity 结果: {:?}", dv_res);
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let mut remount_ok = adb.remount(&serial).await.is_ok();
    tracing::info!("[DeviceSpoof] 第一次 remount 结果: ok={}", remount_ok);

    let mut no_wipe_opts = opts.clone();
    no_wipe_opts.wipe = false;

    // 在全新 AVD 镜像上，disable-verity 需要一次重启才能解除只读保护使 remount 成功
    if !remount_ok {
        tracing::warn!("[DeviceSpoof] 第一次 remount 失败，重启 QEMU 模拟器生效 disable-verity...");
        let _ = adb.emu_kill(&serial).await;
        if !crate::avd::wait_port_free(port, 10).await {
            crate::avd::kill_emulator_on_port(port).await;
            let _ = crate::avd::wait_port_free(port, 5).await;
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;

        crate::avd::clean_avd_lock_files(avd_name);

        if let Err(e) = emulator.boot(avd_name, port, &no_wipe_opts).await {
            tracing::error!("[DeviceSpoof] 重新启动 QEMU 失败: {:?}", e);
            return;
        }
        if let Err(e) = adb.wait_boot(&serial, Duration::from_secs(90)).await {
            tracing::error!("[DeviceSpoof] 重启后等待开机超时: {:?}", e);
            return;
        }
        let _ = adb.root(&serial).await;
        tokio::time::sleep(Duration::from_millis(2000)).await;
        remount_ok = adb.remount(&serial).await.is_ok();
        tracing::info!("[DeviceSpoof] 重启后第二次 remount 结果: ok={}", remount_ok);
    }

    if !remount_ok {
        tracing::error!("[DeviceSpoof] ❌ remount 依然失败，/system 依旧只读，放弃修改 build.prop（已通过 config.ini 完成注入）");
        return;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 3: 安全修改 build.prop 文件（带备份、精确行首匹配，失败自动还原防 BootLoop）
    // Android 14 动态分区：/system/build.prop 可能是空文件或符号链接，实际内容常在 /system/system/build.prop
    let mut script = String::from(
        "#!/system/bin/sh\n\
        for f in /system/build.prop /system/system/build.prop /vendor/build.prop /product/build.prop /system_ext/build.prop /odm/etc/build.prop; do\n\
            if [ -f \"$f\" ]; then\n\
                cp -f \"$f\" \"$f.bak\"\n\
                sed -i '/^ro\\.product\\..*\\.model=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\.model=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\..*\\.brand=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\.brand=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\..*\\.manufacturer=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\.manufacturer=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\..*\\.device=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\.device=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\..*\\.name=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\.name=/d' \"$f\"\n\
                sed -i '/^ro\\.product\\.board=/d' \"$f\"\n\
                sed -i '/^ro\\..*build\\.fingerprint=/d' \"$f\"\n\
                sed -i '/^ro\\.build\\.flavor=/d' \"$f\"\n\
                sed -i '/^ro\\.build\\.description=/d' \"$f\"\n\
                sed -i '/^ro\\.build\\.product=/d' \"$f\"\n"
    );

    for (k, v) in &props {
        script.push_str(&format!("                echo '{}={}' >> \"$f\"\n", k, v));
    }
    script.push_str(
        "            fi\n\
        done\n\
        exit 0\n"
    );

    let write_cmd = format!("cat << 'EOF' > /data/local/tmp/spoof.sh\n{}\nEOF\nchmod 755 /data/local/tmp/spoof.sh\n", script);
    if let Err(e) = adb.shell(&serial, &["sh", "-c", &write_cmd]).await {
        tracing::error!("[DeviceSpoof] 写入 /data/local/tmp/spoof.sh 失败: {:?}", e);
        return;
    }

    match adb.shell(&serial, &["sh", "/data/local/tmp/spoof.sh"]).await {
        Ok(_) => tracing::info!("[DeviceSpoof] build.prop 修改脚本执行完成"),
        Err(e) => {
            tracing::error!("[DeviceSpoof] build.prop 修改脚本执行失败: {:?}, 自动还原备份...", e);
            let restore_cmd = "for f in /system/build.prop /vendor/build.prop /product/build.prop /system_ext/build.prop /odm/etc/build.prop; do [ -f \"$f.bak\" ] && mv -f \"$f.bak\" \"$f\"; done";
            let _ = adb.shell(&serial, &["sh", "-c", restore_cmd]).await;
            return;
        }
    }
    let _ = adb.shell(&serial, &["rm", "-f", "/data/local/tmp/spoof.sh", "/system/*.bak", "/vendor/*.bak"]).await;

    // Step 4: 平滑重启 QEMU 模拟器进程（通过 emu_kill + 确保完全离线 + emulator.boot）
    tracing::info!("[DeviceSpoof] 优雅重启 QEMU 模拟器进程使新 build.prop 生效...");
    let _ = adb.emu_kill(&serial).await;

    if !crate::avd::wait_port_free(port, 10).await {
        crate::avd::kill_emulator_on_port(port).await;
        let _ = crate::avd::wait_port_free(port, 5).await;
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;

    crate::avd::clean_avd_lock_files(avd_name);

    tracing::info!("[DeviceSpoof] 重新拉起 QEMU 模拟器进程...");
    if let Err(e) = emulator.boot(avd_name, port, &no_wipe_opts).await {
        tracing::error!("[DeviceSpoof] 重启 QEMU 模拟器进程失败: {:?}", e);
        return;
    }
    tracing::info!("[DeviceSpoof] QEMU 进程已重新拉起，等待开机完成 (sys.boot_completed=1)...");

    // Step 5: 等待设备重新上线并完成开机
    if let Err(e) = adb.wait_boot(&serial, Duration::from_secs(90)).await {
        tracing::error!("[DeviceSpoof] 重新开机后等待超时: {:?}", e);
        return;
    }

    // 最终校验读取（读文件，若权限受限则配合 getprop 校验）
    let final_prop = read_system_build_prop(adb, &serial).await;
    let mut final_model = prop_from_file(&final_prop, "ro.product.model");
    let mut final_fp = prop_from_file(&final_prop, "ro.system.build.fingerprint");
    if final_model.is_empty() {
        final_model = adb.shell(&serial, &["getprop", "ro.product.model"]).await.unwrap_or_default().trim().to_string();
    }
    if final_fp.is_empty() {
        final_fp = adb.shell(&serial, &["getprop", "ro.system.build.fingerprint"]).await.unwrap_or_default().trim().to_string();
    }
    sync_display_resolution(adb, &serial).await;
    tracing::info!("[DeviceSpoof] 伪装完成，生效型号: {}, 生效指纹: {}", final_model, final_fp);
}

/// 同步屏幕分辨率与 DPI 到匹配当前设备型号的真机参数（通过 wm size 与 wm density 动态覆盖）
pub async fn sync_display_resolution(adb: &Adb, serial: &str) {
    let model = adb.shell(serial, &["getprop", "ro.product.model"]).await.unwrap_or_default();
    let model = model.trim();
    let profile = crate::avd::find_profile_by_model(model).unwrap_or_else(crate::avd::random_device_info);

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
    let mut cmds: Vec<String> = Vec::new();
    if is_wifi {
        cmds.push("svc wifi enable".into());
        cmds.push("cmd wifi set-wifi-enabled enabled".into());
        cmds.push("settings put global wifi_on 1".into());
        cmds.push(format!("settings put global wifi_ssid \"{}\"", chosen_ssid));
        cmds.push(format!("settings put global wifi_connected_ssid \"{}\"", chosen_ssid));
        cmds.push(format!("setprop net.wifi.ssid \"{}\"", chosen_ssid));
        cmds.push("setprop wlan.driver.status ok".into());
        cmds.push("setprop net.dns1 114.114.114.114".into());
        cmds.push("setprop net.dns2 223.5.5.5".into());
        cmds.push("svc data enable".into());
        cmds.push("settings put global mobile_data 1".into());
    } else {
        cmds.push("svc wifi disable".into());
        cmds.push("cmd wifi set-wifi-enabled disabled".into());
        cmds.push("settings put global wifi_on 0".into());
        cmds.push("svc data enable".into());
        cmds.push("settings put global mobile_data 1".into());
        cmds.push("settings put global mobile_data_always_on 1".into());
        cmds.push("setprop net.dns1 114.114.114.114".into());
        cmds.push("setprop net.dns2 223.5.5.5".into());
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
/// 彻底抹除旧 App 数据、缓存与 ANDROID_ID (SSAID)，并直接写入全新合法 ANDROID_ID，
/// 无需热重启 Zygote（避免引发 PackageManagerService 停机与 QEMU socket 断开），
/// 耗时 <1s，同时 100% 保留已注入的真机 build.prop 与指纹。
pub async fn reset_device_in_place(adb: &Adb, serial: &str, pkg: &str) -> Result<(), AdbError> {
    tracing::info!("[L3FastReset] 正在对 {} 执行单启动极速出厂重置...", serial);

    // 0. 确保具备 root 权限以修改 /data/system/users/0/settings_ssaid.xml
    let _ = adb.root(serial).await;

    // 1. 强杀应用进程与清理包
    let _ = adb.shell(serial, &["am", "force-stop", pkg]).await;
    let _ = adb.shell(serial, &["pm", "clear", pkg]).await;
    let _ = adb.shell(serial, &["pm", "uninstall", pkg]).await;

    // 2. 清理应用私有目录与 SSAID 配置文件
    let clean_cmd = format!(
        "rm -rf /data/data/{pkg} /data/user/0/{pkg} /data/user_de/0/{pkg} /sdcard/Android/data/{pkg} /sdcard/.um /sdcard/.utm; \
         rm -rf /sdcard/DCIM /sdcard/Pictures /sdcard/Download; \
         sync",
        pkg = pkg
    );
    let _ = adb.shell(serial, &["sh", "-c", &clean_cmd]).await;

    // 3. 生成全新合法的 16 位十六进制 ANDROID_ID (SSAID) 并直接写入系统配置与 settings 数据库，
    // 免去重启 Zygote / Android Framework，零系统服务停机时间，避免导致 PackageManagerService 装包失败或模拟器 socket 断开
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let rand_val: u64 = ((nanos ^ (nanos >> 32)) as u64) ^ 0xa341316cbee9u64;
    let new_android_id = format!("{:016x}", rand_val);

    if let Err(e) = crate::engine::profile_archive::inject_ssaid(adb, serial, pkg, &new_android_id).await {
        tracing::warn!("[L3FastReset] 注入新 ANDROID_ID 警告: {}", e);
    } else {
        tracing::info!("[L3FastReset] 成功为 {} 分配全新合法 ANDROID_ID: {}", pkg, new_android_id);
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
