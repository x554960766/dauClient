//! IPC 接口层（设计文档 §8）

use crate::engine::{EngineConfig, ResetLevel};
use crate::pipeline::*;
use crate::state::SharedState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio_util::sync::CancellationToken;

// ---------------- 初始化向导 ----------------

#[tauri::command]
pub async fn setup_status(state: State<'_, SharedState>) -> Result<crate::sdkmgr::SetupReport, String> {
    let report = crate::sdkmgr::setup_report(&state.sdk_dir, &[34]);
    tracing::info!(
        "setup_status: sdk_dir={}, components={:?}",
        state.sdk_dir.display(),
        report.components.iter().map(|c| (&c.id, &c.state)).collect::<Vec<_>>()
    );
    Ok(report)
}

#[tauri::command]
pub async fn setup_download(
    app: AppHandle,
    state: State<'_, SharedState>,
    api_levels: Vec<u32>,
) -> Result<(), String> {
    let sdk = state.sdk_dir.clone();
    let levels = if api_levels.is_empty() { vec![34] } else { api_levels };
    let env: Vec<(String, String)> = vec![];
    // v1.2: 优先从打包内置 bundle 部署（跳过 ~650MB 网络下载）
    let resource_dir = app.path().resource_dir().ok();
    crate::sdkmgr::download::ensure_components(&app, &sdk, &levels, &env, resource_dir.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypervisorSetupResult {
    pub ready: bool,
    pub detail: String,
}

#[tauri::command]
pub async fn windows_hypervisor_setup() -> Result<HypervisorSetupResult, String> {
    let hv = crate::sdkmgr::hypervisor_state();
    // AEHD 实际安装流程（下载 + 静默安装 + UAC）在 M5 Windows 移植时完善
    Ok(HypervisorSetupResult { ready: hv.ready, detail: hv.detail })
}

// ---------------- Phase 0 ----------------

#[tauri::command]
pub async fn preflight_check(
    state: State<'_, SharedState>,
    config: Option<EngineConfig>,
) -> Result<PreflightReport, String> {
    let cfg = config.unwrap_or_default();
    Ok(crate::pipeline::preflight_check(&state, &cfg).await)
}

#[tauri::command]
pub async fn inspect_apk(
    state: State<'_, SharedState>,
    apk_path: String,
    declared_appkey: String,
) -> Result<ApkInfo, String> {
    crate::pipeline::inspect_apk(&state, &apk_path, &declared_appkey).await
}

// ---------------- Phase 1 ----------------

#[tauri::command]
pub async fn pilot_start(app: AppHandle, state: State<'_, SharedState>, config: EngineConfig) -> Result<(), String> {
    let st = Arc::clone(&state);
    tauri::async_runtime::spawn(async move {
        let _ = crate::pipeline::pilot_run(app, st, config).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn pilot_reset_trial(
    app: AppHandle,
    state: State<'_, SharedState>,
    config: EngineConfig,
    level: String,
) -> Result<crate::identity::DeviceIdentity, String> {
    crate::pipeline::pilot_reset_trial(app, Arc::clone(&state), config, ResetLevel::from_str(&level)).await
}

#[tauri::command]
pub async fn pilot_gate1_submit(answers: Gate1Answers, evidence_ok: bool) -> Result<Gate1Verdict, String> {
    Ok(crate::pipeline::gate1_verdict(&answers, evidence_ok))
}

// ---------------- Phase 2 ----------------

#[tauri::command]
pub async fn batch_start(app: AppHandle, state: State<'_, SharedState>, config: EngineConfig) -> Result<String, String> {
    let run_id = format!("dau-run-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let cancel = CancellationToken::new();
    state.runs.lock().await.insert(run_id.clone(), cancel.clone());
    let st = Arc::clone(&state);
    let rid = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::pipeline::batch_run(app, st, config, rid, cancel).await;
    });
    Ok(run_id)
}

#[tauri::command]
pub async fn batch_stop(state: State<'_, SharedState>, run_id: String) -> Result<(), String> {
    if let Some(token) = state.runs.lock().await.remove(&run_id) {
        token.cancel();
    }
    Ok(())
}

// ---------------- 手机 USB 换 IP ----------------

#[tauri::command]
pub async fn detect_usb_phones(state: State<'_, SharedState>) -> Result<Vec<crate::adb::rotate_ip::UsbPhoneInfo>, String> {
    Ok(crate::adb::rotate_ip::detect_usb_phones(&state.sdk_dir).await)
}

#[tauri::command]
pub async fn test_rotate_ip(
    state: State<'_, SharedState>,
    serial: Option<String>,
    disconnect_wait_s: Option<u32>,
    reconnect_wait_s: Option<u32>,
) -> Result<crate::adb::rotate_ip::RotateIpResult, String> {
    crate::adb::rotate_ip::rotate_ip_via_adb(
        &state.sdk_dir,
        serial.as_deref(),
        disconnect_wait_s.unwrap_or(4),
        reconnect_wait_s.unwrap_or(6),
        None,
    ).await
}

// ---------------- 清理与报告 ----------------

#[tauri::command]
pub async fn cleanup_orphans(state: State<'_, SharedState>) -> Result<usize, String> {
    let sdk = state.sdk_dir.clone();
    let adb = crate::adb::Adb::new(&sdk, state.adb_server_port);
    let avdm = crate::avd::AvdManager::new(&sdk, adb.env().clone());
    let list = avdm.list().await.map_err(|e| e.to_string())?;
    let ours: Vec<String> = list
        .into_iter()
        .filter(|n| n.starts_with("dau-") || n.starts_with("dau_") || n.contains("dau"))
        .collect();
    let count = ours.len();
    for name in &ours {
        tracing::info!(avd = %name, "自动清理残留 AVD");
        let _ = avdm.delete(name).await;
        crate::avd::clean_avd_lock_files(name);
    }
    Ok(count)
}

#[tauri::command]
pub async fn export_report(state: State<'_, SharedState>, run_id: String, save_path: String) -> Result<(), String> {
    let run_dir = state.runs_dir.join(&run_id);
    let result = crate::report::RunResult::load(&run_dir).await.ok_or("运行结果不存在")?;
    let md = crate::report::render_markdown(&result);
    tokio::fs::write(&save_path, md).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reconcile_t_plus_1(
    state: State<'_, SharedState>,
    run_id: String,
    backend_list: String,
) -> Result<crate::report::ReconcileReport, String> {
    let run_dir = state.runs_dir.join(&run_id);
    let result = crate::report::RunResult::load(&run_dir).await.ok_or("运行结果不存在")?;
    Ok(crate::report::reconcile(&result, &backend_list))
}

#[tauri::command]
pub async fn list_runs(state: State<'_, SharedState>) -> Result<Vec<String>, String> {
    let mut runs = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&state.runs_dir).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            if e.path().join("result.json").exists() {
                runs.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    runs.sort_by(|a, b| b.cmp(a));
    Ok(runs)
}

// ---------------- 设备池管理（留存方案）----------------

#[tauri::command]
pub async fn pool_status(state: State<'_, SharedState>) -> Result<crate::engine::pool::DevicePool, String> {
    Ok(crate::engine::pool::DevicePool::load(&state.pool_file).await)
}

#[tauri::command]
pub async fn pool_clear(state: State<'_, SharedState>) -> Result<usize, String> {
    let pool = crate::engine::pool::DevicePool::load(&state.pool_file).await;
    let count = pool.len();

    // 删除池中所有 AVD
    let sdk = state.sdk_dir.clone();
    let adb = crate::adb::Adb::new(&sdk, state.adb_server_port);
    let avdm = crate::avd::AvdManager::new(&sdk, adb.env().clone());
    for dev in &pool.devices {
        tracing::info!(avd = %dev.avd_name, "清理设备池 AVD");
        let _ = avdm.delete(&dev.avd_name).await;
        crate::avd::clean_avd_lock_files(&dev.avd_name);
    }

    // 清空池文件
    let empty = crate::engine::pool::DevicePool::default();
    let _ = empty.save(&state.pool_file).await;

    Ok(count)
}

// ---------------- 身份档案库管理（300+ 留存方案 A）----------------

#[tauri::command]
pub async fn list_profiles(
    state: State<'_, SharedState>,
) -> Result<Vec<crate::engine::profile_archive::IdentityProfile>, String> {
    Ok(crate::engine::profile_archive::ProfileArchiveManager::load_all(&state.profiles_dir).await)
}

#[tauri::command]
pub async fn clear_profiles(state: State<'_, SharedState>) -> Result<usize, String> {
    crate::engine::profile_archive::ProfileArchiveManager::clear(&state.profiles_dir)
        .await
        .map_err(|e| e.to_string())
}

// ---------------- 动态档案堆栈管理（300 容量 FIFO + 防重）----------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackStatusReport {
    pub capacity: usize,
    pub total_entries: usize,
    pub available_today: usize,
    pub used_today: usize,
    pub last_cleared_date: Option<String>,
}

#[tauri::command]
pub async fn get_stack_status(state: State<'_, SharedState>) -> Result<StackStatusReport, String> {
    let mut stack = crate::engine::profile_stack::ProfileStackStore::load(&state.stack_file).await;
    if stack.check_and_auto_reset() {
        let _ = stack.save(&state.stack_file).await;
    }

    let today = crate::engine::profile_stack::ProfileStackStore::beijing_now_info().0;
    let available = stack.available_count_for_today(&today);
    let total = stack.entries.len();

    Ok(StackStatusReport {
        capacity: stack.capacity,
        total_entries: total,
        available_today: available,
        used_today: total.saturating_sub(available),
        last_cleared_date: stack.last_cleared_date.clone(),
    })
}

#[tauri::command]
pub async fn reset_profile_usage(state: State<'_, SharedState>) -> Result<(), String> {
    let mut stack = crate::engine::profile_stack::ProfileStackStore::load(&state.stack_file).await;
    stack.manual_reset();
    stack.save(&state.stack_file).await.map_err(|e| e.to_string())
}
