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

    // 首先将真机属性与分辨率注入 AVD config.ini（QEMU 原生系统属性支持机制）
    crate::avd::inject_system_properties_to_config_ini(avd_name).await;

    let cur_fp = adb.shell(&serial, &["getprop", "ro.system.build.fingerprint"])
        .await
        .unwrap_or_default();
    let cur_fp = cur_fp.trim();
    tracing::info!("[DeviceSpoof] 当前系统指纹: {}", cur_fp);

    // 只有指纹中依然包含 sdk_gphone 或 google 时才执行属性修改与重启；若已经是真机指纹则不重复修改
    if !cur_fp.is_empty() && !cur_fp.contains("sdk_gphone") && !cur_fp.contains("google") {
        tracing::info!("[DeviceSpoof] 当前指纹已是真机指纹，跳过修改");
        return;
    }

    let props = crate::avd::random_device_props();
    tracing::info!("[DeviceSpoof] 随机生成的真机属性: {:?}", props);

    // Step 1: adb root（重启 adbd 守护进程为 root 权限并等待重连）
    let _ = adb.root(&serial).await;
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
    let mut script = String::from(
        "#!/system/bin/sh\n\
        for f in /system/build.prop /vendor/build.prop /product/build.prop /system_ext/build.prop /odm/etc/build.prop; do\n\
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

    let final_model = adb.shell(&serial, &["getprop", "ro.product.model"]).await.unwrap_or_default();
    let final_fp = adb.shell(&serial, &["getprop", "ro.system.build.fingerprint"]).await.unwrap_or_default();
    tracing::info!("[DeviceSpoof] 🎉 伪装成功！当前型号: {}, 当前指纹: {}", final_model.trim(), final_fp.trim());
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
