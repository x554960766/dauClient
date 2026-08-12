//! logcat 子进程管理 + 行解析（设计文档 §7.2a）
//! v1.1 定位：辅助信号（展示与排障用），不再作为 GATE 1 判据——
//! 判据由 proxy CONNECT 计数承担。

use crate::adb::AdbEnv;
use crate::sdkmgr::adb_bin_name;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub device: String,
    pub line: String,
    /// 命中的关键词标签（umeng/umlog/mobclick/统计）
    pub tags: Vec<String>,
}

const KEYWORDS: &[&str] = &["umeng", "umlog", "mobclick", "统计"];

pub struct LogcatStream {
    child: Child,
}

impl LogcatStream {
    /// 启动 logcat 流，每行通过 event 推给前端
    pub fn spawn(app: AppHandle, env: &AdbEnv, serial: &str) -> std::io::Result<Self> {
        let bin = env.sdk_dir.join("platform-tools").join(adb_bin_name());
        let mut cmd = Command::new(bin);
        env.apply(&mut cmd);
        cmd.args(["-s", serial, "logcat"]);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;
        if let Some(stdout) = child.stdout.take() {
            let serial = serial.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let lower = line.to_lowercase();
                    let tags: Vec<String> = KEYWORDS
                        .iter()
                        .filter(|k| lower.contains(**k))
                        .map(|k| k.to_string())
                        .collect();
                    if !tags.is_empty() {
                        let _ = app.emit(
                            "pilot://log",
                            LogLine { device: serial.clone(), line, tags },
                        );
                    }
                }
            });
        }
        Ok(Self { child })
    }

    /// 先清缓冲区再开始流式输出
    pub async fn clear_and_spawn(app: AppHandle, env: &AdbEnv, serial: &str) -> std::io::Result<Self> {
        let bin = env.sdk_dir.join("platform-tools").join(adb_bin_name());
        let mut clear = Command::new(bin);
        env.apply(&mut clear);
        let _ = clear.args(["-s", serial, "logcat", "-c"]).output().await;
        Self::spawn(app, env, serial)
    }

    pub async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

impl Drop for LogcatStream {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
