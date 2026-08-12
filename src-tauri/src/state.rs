use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::proxy::ProxyStats;

/// 全局共享状态
pub struct AppState {
    /// 客户端私有 SDK 根目录（macOS/Linux: ~/Library/Caches/umeng-dau-client/sdk 无空格，Windows: C:\umeng-dau-sdk）
    pub sdk_dir: PathBuf,
    /// 私有 adb server 端口（默认 5137，冲突时自动探测空闲端口）
    pub adb_server_port: u16,
    /// 正在运行的任务取消令牌：run_id -> token
    pub runs: Mutex<HashMap<String, CancellationToken>>,
    /// 内嵌代理实际监听端口（setup/试点/放量共享一个实例）
    pub proxy_port: Mutex<Option<u16>>,
    /// 内嵌代理统计句柄（共享一个实例；试 Lx 用其前后快照算本次增量）
    pub proxy_stats: Mutex<Option<Arc<RwLock<ProxyStats>>>>,
    /// 内嵌代理取消令牌（清理旧代理任务用）
    pub proxy_shutdown: Mutex<Option<CancellationToken>>,
    /// 已标定结果持久化文件
    pub settings_file: PathBuf,
    /// 运行输出根目录（runs/dau-run-<ts>/）
    pub runs_dir: PathBuf,
}

impl AppState {
    pub fn new() -> Self {
        let base = crate::sdkmgr::default_sdk_root();
        let runs_dir = base
            .parent()
            .unwrap_or(&base)
            .join("runs");
        let settings_file = base
            .parent()
            .unwrap_or(&base)
            .join("settings.json");
        // 确保 runs_dir 存在——否则预检的 disk_free_gb() 在不存在的路径上 statvfs 失败
        // 会返回 0，误判为「磁盘不足」红 X
        let _ = std::fs::create_dir_all(&runs_dir);
        Self {
            sdk_dir: base,
            adb_server_port: crate::adb::find_free_adb_port(),
            runs: Mutex::new(HashMap::new()),
            proxy_port: Mutex::new(None),
            proxy_stats: Mutex::new(None),
            proxy_shutdown: Mutex::new(None),
            settings_file,
            runs_dir,
        }
    }
}

pub type SharedState = Arc<AppState>;
