pub mod adb;
pub mod avd;
pub mod commands;
pub mod engine;
pub mod identity;
pub mod logcat;
pub mod pipeline;
pub mod proxy;
pub mod report;
pub mod sdkmgr;
pub mod state;
pub mod uiautomation;

use state::AppState;
use std::sync::Arc;

#[cfg(unix)]
fn boost_fd_limit() {
    unsafe {
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) == 0 {
            let target = (65536 as libc::rlim_t).min(rlim.rlim_max);
            if rlim.rlim_cur < target {
                rlim.rlim_cur = target;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) == 0 {
                    tracing::info!(target, "已成功提升系统文件描述符 (FD) 句柄限制上限");
                }
            }
        }
    }
}

pub fn run() {
    #[cfg(unix)]
    boost_fd_limit();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    use tauri::Manager;

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(AppState::new()))
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<Arc<AppState>>();
                let sdk = state.sdk_dir.clone();
                if sdk.exists() {
                    let adb = crate::adb::Adb::new(&sdk, state.adb_server_port);
                    let avdm = crate::avd::AvdManager::new(&sdk, adb.env().clone());
                    if let Ok(list) = avdm.list().await {
                        let ours: Vec<String> = list
                            .into_iter()
                            .filter(|n| n.starts_with("dau-") || n.starts_with("dau_") || n.contains("dau"))
                            .collect();
                        if !ours.is_empty() {
                            tracing::info!(count = ours.len(), "应用启动：自动清理上次异常挂掉遗留的 AVD: {:?}", ours);
                            for name in &ours {
                                let _ = avdm.delete(name).await;
                                crate::avd::clean_avd_lock_files(name);
                            }
                            tracing::info!("应用启动：残留 AVD 自动清理完成");
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::setup_status,
            commands::setup_download,
            commands::windows_hypervisor_setup,
            commands::preflight_check,
            commands::inspect_apk,
            commands::pilot_start,
            commands::pilot_reset_trial,
            commands::pilot_gate1_submit,
            commands::batch_start,
            commands::batch_stop,
            commands::cleanup_orphans,
            commands::export_report,
            commands::reconcile_t_plus_1,
            commands::list_runs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
