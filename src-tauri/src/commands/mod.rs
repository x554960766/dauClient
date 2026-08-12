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
