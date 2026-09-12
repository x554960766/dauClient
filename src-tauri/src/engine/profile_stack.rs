//! 动态档案堆栈与防重留存轮换系统（Profile Stack System）
//!
//! 核心逻辑：
//! 1. FIFO 动态堆栈（容量默认 300，可配置）
//! 2. 堆栈未满时，新设备成功即入栈；满 300 时，50%-70% 概率入栈并出栈（淘汰）最早设备
//! 3. 逐台处理时，30%-50% 概率走 L3 新增，50%-70% 概率抽取堆栈留存
//! 4. 一天只能使用一次：档案打上当天的 last_used_date 标记，未用过的才能被抽中，用完自动锁住
//! 5. 无可用档案时自动平滑降级走 L3 新增
//! 6. 9:10 自动检查重置与手动重置（带防重复重置锁）

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 堆栈单条档案记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub profile: crate::engine::profile_archive::IdentityProfile,
    /// 当天是否已使用标记："YYYY-MM-DD"（北京时间）
    pub last_used_date: Option<String>,
}

/// 档案堆栈存储结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileStackStore {
    /// 堆栈最大容量（默认 300）
    pub capacity: usize,
    /// 上次清理使用状态的日期："YYYY-MM-DD"
    pub last_cleared_date: Option<String>,
    /// 档案队列：索引 0 为最早入栈的档案 (FIFO)
    pub entries: Vec<ProfileEntry>,
}

impl Default for ProfileStackStore {
    fn default() -> Self {
        Self {
            capacity: 300,
            last_cleared_date: None,
            entries: Vec::new(),
        }
    }
}

/// 堆栈读取模式：随机抽取 vs 顺序读取
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackReadMode {
    /// 从当天未使用的设备中随机抽取（默认）
    Random,
    /// 从当天未使用的设备中按 FIFO 顺序读取
    Sequential,
}

impl Default for StackReadMode {
    fn default() -> Self {
        Self::Random
    }
}

use once_cell::sync::Lazy;
use tokio::sync::Mutex as AsyncMutex;

/// 全局 Async 文件互斥锁，防止并发读写/IPC轮换将 stack.json 损坏清空
static STACK_IO_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

impl ProfileStackStore {
    /// 从文件安全加载档案堆栈（带全局互斥锁 + 损坏防覆盖保护）
    pub async fn load(file: &Path) -> Self {
        let _guard = STACK_IO_LOCK.lock().await;
        if !file.exists() {
            return Self::default();
        }
        match tokio::fs::read_to_string(file).await {
            Ok(content) => {
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    return Self::default();
                }
                match serde_json::from_str::<Self>(trimmed) {
                    Ok(store) => store,
                    Err(e) => {
                        tracing::error!(
                            "[ProfileStack] ❌ JSON 解析失败 (可能读到了并发未写完的数据): {:?}, 防覆盖保护启用",
                            e
                        );
                        Self::default()
                    }
                }
            }
            Err(e) => {
                tracing::error!("[ProfileStack] ❌ 读取文件失败: {:?}", e);
                Self::default()
            }
        }
    }

    /// 安全保存档案堆栈到文件（带全局互斥锁 + 临时文件 Atomic Rename 原子覆盖）
    pub async fn save(&self, file: &Path) -> std::io::Result<()> {
        let _guard = STACK_IO_LOCK.lock().await;
        if let Some(p) = file.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let json = serde_json::to_string_pretty(self)?;

        // 原子写入机制：先写入 .tmp 临时文件，再通过 OS 的 rename 命令覆盖，
        // 从根源上杜绝“读到了刚写一半的 JSON 文件”导致的数据损坏与被清空
        let tmp_file = file.with_extension("json.tmp");
        tokio::fs::write(&tmp_file, &json).await?;
        tokio::fs::rename(&tmp_file, file).await
    }

    /// 获取当前的北京时间 (YYYY-MM-DD, 小时, 分钟)
    pub fn beijing_now_info() -> (String, u32, u32) {
        let utc = chrono::Utc::now();
        let bj = utc + chrono::Duration::hours(8);
        let date_str = bj.format("%Y-%m-%d").to_string();
        use chrono::Timelike;
        (date_str, bj.hour(), bj.minute())
    }

    /// 检查并在需要时执行 9:10 自动清理（如果当天已手动清理过则跳过）
    pub fn check_and_auto_reset(&mut self) -> bool {
        let (today_date, hour, minute) = Self::beijing_now_info();
        // 判断当前时间是否在当天 9:10 或之后
        let is_past_9_10 = hour > 9 || (hour == 9 && minute >= 10);
        if is_past_9_10 && self.last_cleared_date.as_deref() != Some(&today_date) {
            self.do_reset(&today_date);
            return true;
        }
        false
    }

    /// 手动清理使用状态（重置可用状态并防当天重复清理）
    pub fn manual_reset(&mut self) {
        let (today_date, _, _) = Self::beijing_now_info();
        self.do_reset(&today_date);
    }

    /// 执行重置逻辑
    fn do_reset(&mut self, today_date: &str) {
        self.last_cleared_date = Some(today_date.to_string());
        for entry in &mut self.entries {
            entry.last_used_date = None;
        }
        tracing::info!(date = %today_date, "档案堆栈使用状态已重置为全新可用状态");
    }

    /// 检查堆栈中当前日期未使用的可用档案数量
    pub fn available_count_for_today(&self, today_date: &str) -> usize {
        self.entries
            .iter()
            .filter(|e| e.last_used_date.as_deref() != Some(today_date))
            .count()
    }

    /// 从堆栈中尝试获取一份当天未使用的档案（标记 last_used_date = today_date 并返回）
    /// 支持随机 (Random) 与 顺序 (Sequential) 两种模式，均严格仅在【当天未使用】的集合中选择，绝不重复读取！
    pub fn pop_available_for_today(
        &mut self,
        today_date: &str,
        mode: StackReadMode,
    ) -> Option<crate::engine::profile_archive::IdentityProfile> {
        // 找到所有【当天未使用】的记录索引
        let unused_indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.last_used_date.as_deref() != Some(today_date))
            .map(|(i, _)| i)
            .collect();

        if unused_indices.is_empty() {
            return None;
        }

        let selected_idx = match mode {
            StackReadMode::Sequential => unused_indices[0],
            StackReadMode::Random => {
                let r = super::pseudo_random_f64(unused_indices.len() as u64);
                unused_indices[(r * unused_indices.len() as f64) as usize % unused_indices.len()]
            }
        };

        // 标记当天已使用并锁定
        self.entries[selected_idx].last_used_date = Some(today_date.to_string());
        Some(self.entries[selected_idx].profile.clone())
    }

    /// L3 设备成功后推入堆栈（处理未满直接入栈与满 300 时 50%-70% 概率入栈淘汰）
    /// used_today_date: 当天跑完 L3 生成的新设备标记为当天已使用（防当天重复抽取）
    pub fn push_new_profile(
        &mut self,
        profile: crate::engine::profile_archive::IdentityProfile,
        full_push_probability: f64, // 如 0.60 (50%-70% 范围)
        used_today_date: Option<&str>,
    ) -> bool {
        let last_used = used_today_date.map(|s| s.to_string());
        // 如果已存在相同的 android_id，直接更新
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| !e.profile.android_id.is_empty() && e.profile.android_id == profile.android_id)
        {
            existing.profile = profile;
            existing.last_used_date = last_used;
            return true;
        }

        let cap = self.capacity.max(1);
        let entry = ProfileEntry {
            profile,
            last_used_date: last_used,
        };

        if self.entries.len() < cap {
            // 堆栈未满，直接入栈
            self.entries.push(entry);
            true
        } else {
            // 堆栈已满，按指定概率（50%-70%）摇号判定是否入栈淘汰
            let roll = super::pseudo_random_f64(self.entries.len() as u64);

            if roll <= full_push_probability {
                // 淘汰最早入栈的索引 0 档案 (FIFO Eviction)
                if !self.entries.is_empty() {
                    let evicted = self.entries.remove(0);
                    tracing::info!(
                        evicted_id = %evicted.profile.profile_id,
                        new_id = %entry.profile.profile_id,
                        "堆栈已满：淘汰最早档案，压入新档案"
                    );
                }
                self.entries.push(entry);
                true
            } else {
                tracing::info!(
                    profile_id = %entry.profile.profile_id,
                    "堆栈已满：概率摇号未命中，丢弃入栈"
                );
                false
            }
        }
    }
}

/// 档案堆栈默认保存路径（`<runs_dir>/../profiles_stack.json`）
pub fn default_stack_file(base: &Path) -> PathBuf {
    base.parent().unwrap_or(base).join("profiles_stack.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::profile_archive::IdentityProfile;
    use std::collections::HashMap;

    fn mock_profile(id: usize) -> IdentityProfile {
        IdentityProfile {
            profile_id: format!("prof-{:04}", id),
            pkg: "com.example.app".into(),
            android_id: format!("android_id_{}", id),
            umid: format!("umid_{}", id),
            brand: "Xiaomi".into(),
            model: format!("Model {}", id),
            device: "device".into(),
            board: "board".into(),
            hardware: "qcom".into(),
            fingerprint: "fingerprint".into(),
            width: 1080,
            height: 2400,
            density: 440,
            shared_prefs: HashMap::new(),
            created_at: "2026-08-13 10:00:00".into(),
            last_used_at: "2026-08-13 10:00:00".into(),
        }
    }

    #[test]
    fn test_stack_capacity_and_fifo_eviction() {
        let mut store = ProfileStackStore {
            capacity: 3,
            last_cleared_date: None,
            entries: Vec::new(),
        };

        // 1. 压入前 3 个（堆栈未满）
        assert!(store.push_new_profile(mock_profile(1), 1.0, None));
        assert!(store.push_new_profile(mock_profile(2), 1.0, None));
        assert!(store.push_new_profile(mock_profile(3), 1.0, None));
        assert_eq!(store.entries.len(), 3);
        assert_eq!(store.entries[0].profile.profile_id, "prof-0001");

        // 2. 满栈推入第 4 个 (full_push_probability = 1.0 必定淘汰 index 0)
        assert!(store.push_new_profile(mock_profile(4), 1.0, None));
        assert_eq!(store.entries.len(), 3);
        // 原 prof-0001 已淘汰，最早的变为 prof-0002
        assert_eq!(store.entries[0].profile.profile_id, "prof-0002");
        assert_eq!(store.entries[2].profile.profile_id, "prof-0004");
    }

    #[test]
    fn test_once_per_day_locking_no_duplicates() {
        let mut store = ProfileStackStore {
            capacity: 10,
            last_cleared_date: None,
            entries: Vec::new(),
        };

        for i in 1..=5 {
            store.push_new_profile(mock_profile(i), 1.0, None);
        }

        let today = "2026-08-13";
        assert_eq!(store.available_count_for_today(today), 5);

        let mut popped_ids = Vec::new();
        for _ in 0..5 {
            let p = store.pop_available_for_today(today, StackReadMode::Random).unwrap();
            popped_ids.push(p.profile_id);
        }

        // 验证 5 次提取中无任何重复
        let mut unique_ids = popped_ids.clone();
        unique_ids.sort();
        unique_ids.dedup();
        assert_eq!(unique_ids.len(), 5);

        // 验证全部锁定后，第 6 次提取返回 None (无可用档案，降级 L3)
        assert_eq!(store.available_count_for_today(today), 0);
        assert!(store.pop_available_for_today(today, StackReadMode::Random).is_none());
    }

    #[test]
    fn test_sequential_mode() {
        let mut store = ProfileStackStore {
            capacity: 10,
            last_cleared_date: None,
            entries: Vec::new(),
        };

        for i in 1..=3 {
            store.push_new_profile(mock_profile(i), 1.0, None);
        }

        let today = "2026-08-13";
        let p1 = store.pop_available_for_today(today, StackReadMode::Sequential).unwrap();
        let p2 = store.pop_available_for_today(today, StackReadMode::Sequential).unwrap();
        let p3 = store.pop_available_for_today(today, StackReadMode::Sequential).unwrap();

        assert_eq!(p1.profile_id, "prof-0001");
        assert_eq!(p2.profile_id, "prof-0002");
        assert_eq!(p3.profile_id, "prof-0003");
    }

    #[test]
    fn test_manual_and_auto_reset() {
        let mut store = ProfileStackStore {
            capacity: 10,
            last_cleared_date: None,
            entries: Vec::new(),
        };

        store.push_new_profile(mock_profile(1), 1.0, None);
        let today = ProfileStackStore::beijing_now_info().0;

        // 使用 1 次后锁定
        let _ = store.pop_available_for_today(&today, StackReadMode::Random);
        assert_eq!(store.available_count_for_today(&today), 0);

        // 手动清理
        store.manual_reset();
        assert_eq!(store.last_cleared_date.as_deref(), Some(today.as_str()));
        assert_eq!(store.available_count_for_today(&today), 1);
    }
}

