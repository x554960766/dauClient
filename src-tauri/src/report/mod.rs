//! 结果聚合 + Markdown 报告 + T+1 差集对账（设计文档 §7.3）

use crate::engine::DeviceResult;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub run_id: String,
    pub api_level: u32,
    pub reset_level: String,
    pub target: u32,
    pub ok: u32,
    pub fail: u32,
    pub timezone_note: String,
    pub started_at: String,
    pub finished_at: String,
    pub devices: Vec<DeviceResult>,
    /// 合规声明留痕（§1.4）
    pub compliance_ack: bool,
}

impl RunResult {
    pub async fn save(&self, dir: &Path) -> std::io::Result<()> {
        tokio::fs::create_dir_all(dir).await?;
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(dir.join("result.json"), json).await
    }

    pub async fn load(dir: &Path) -> Option<Self> {
        let content = tokio::fs::read_to_string(dir.join("result.json")).await.ok()?;
        serde_json::from_str(&content).ok()
    }
}

/// 生成 Markdown 报告（Phase 3 验收表留空待填 + 差值归因区）
pub fn render_markdown(result: &RunResult) -> String {
    let mut md = String::new();
    md.push_str(&format!("# 友盟 DAU 批量执行报告 — {}\n\n", result.run_id));
    md.push_str(&format!("- 执行时间：{} → {}（北京时间）\n", result.started_at, result.finished_at));
    md.push_str(&format!("- 自然日说明：{}\n", result.timezone_note));
    md.push_str(&format!("- API 级别：android-{}　重置级别：{}\n", result.api_level, result.reset_level));
    md.push_str(&format!("- 目标 {} 台 / 成功 {} / 失败 {}\n\n", result.target, result.ok, result.fail));

    md.push_str("## Phase 3 验收表（T+1 人工核对后填写）\n\n");
    md.push_str("| 检查项 | 客户端报告 | 友盟后台实际 | 差值 |\n|---|---|---|---|\n");
    md.push_str(&format!("| 新增设备数 | {} | ＿＿ | ＿＿ |\n", result.ok));
    md.push_str(&format!("| DAU 增量 | {} | ＿＿ | ＿＿ |\n", result.ok));
    md.push_str("| 机型分布（sdk_gphone/generic） | — | ＿＿ | — |\n\n");
    md.push_str("> 差值不要抹平——它本身就是「友盟对 Android 模拟器流量的过滤率」这个测试结论。\n\n");

    md.push_str("## 逐台明细\n\n");
    md.push_str("| # | 槽位 | 模式 | 设备型号 | 状态 | ANDROID_ID | UMID | 代理命中 | 耗时(s) | 错误 |\n");
    md.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for d in &result.devices {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {} |\n",
            d.index,
            d.slot,
            if d.is_retention { "留存" } else { "新增" },
            if d.device_model.is_empty() { "—" } else { &d.device_model },
            d.status,
            if d.android_id.is_empty() { "—" } else { &d.android_id },
            if d.umid.is_empty() { "—" } else { &d.umid },
            d.proxy_hits,
            d.duration_s,
            d.error.clone().unwrap_or_default(),
        ));
    }
    md
}

/// T+1 逐台差集对账（v1.1 P1-3）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub client_count: usize,
    pub backend_count: usize,
    /// 客户端有、后台没有（被过滤/未上报成功）
    pub missing_in_backend: Vec<DeviceResult>,
    /// 后台有、客户端没有（异常）
    pub extra_in_backend: Vec<String>,
    /// 差值即过滤率结论
    pub filter_rate_pct: f64,
}

pub fn reconcile(result: &RunResult, backend_list_text: &str) -> ReconcileReport {
    let backend_ids: HashSet<String> = backend_list_text
        .lines()
        .flat_map(|l| l.split([',', ';', '\t', ' ']))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() >= 8)
        .collect();

    let ok_devices: Vec<&DeviceResult> = result.devices.iter().filter(|d| d.status == "ok").collect();
    let mut missing = Vec::new();
    for d in &ok_devices {
        let in_backend = (!d.umid.is_empty() && backend_ids.contains(&d.umid.to_lowercase()))
            || (!d.android_id.is_empty() && backend_ids.contains(&d.android_id.to_lowercase()));
        if !in_backend {
            missing.push((*d).clone());
        }
    }

    let client_ids: HashSet<String> = ok_devices
        .iter()
        .flat_map(|d| [d.umid.to_lowercase(), d.android_id.to_lowercase()])
        .filter(|s| !s.is_empty())
        .collect();
    let extra: Vec<String> = backend_ids
        .iter()
        .filter(|id| !client_ids.contains(*id))
        .cloned()
        .collect();

    let filter_rate = if ok_devices.is_empty() {
        0.0
    } else {
        missing.len() as f64 / ok_devices.len() as f64 * 100.0
    };

    ReconcileReport {
        client_count: ok_devices.len(),
        backend_count: backend_ids.len(),
        missing_in_backend: missing,
        extra_in_backend: extra,
        filter_rate_pct: (filter_rate * 10.0).round() / 10.0,
    }
}
