//! CoverageWalker：确定性埋点覆盖遍历（设计文档 §5 重写 + §12.6 参考实现）
//! 核心原则：确定性、可断言、可复现。每台设备走同一条路径，失败可一比一复现。
//! 事件断言主路径是 logcat（§12.1：代理只能看 CONNECT host，看不到事件名）。

use crate::adb::{Adb, AdbError};
use crate::uiautomation::dump::{UiDump, UiNodeOwned};
use crate::uiautomation::interactor::{Interactor, ScreenInfo};
use crate::uiautomation::script::{CoverageConfig, StepAction, TargetSelector};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum StepStatus {
    Ok,
    TargetNotFound,
    SkippedDanger,
    EventMissing { expected: Vec<String>, observed: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub name: String,
    #[serde(flatten)]
    pub status: StepStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoverageReport {
    pub steps_total: u32,
    pub steps_executed: u32,
    pub events_expected: u32,
    pub events_observed: u32,
    pub relaunches: u32,
    pub results: Vec<StepResult>,
    pub duration_s: f64,
}

impl CoverageReport {
    fn push(&mut self, name: &str, status: StepStatus) {
        self.steps_executed += 1;
        match &status {
            StepStatus::Ok => {}
            StepStatus::EventMissing { .. } => {}
            _ => {}
        }
        self.results.push(StepResult {
            name: name.to_string(),
            status,
        });
    }
}

/// 证据采集器（v1 简化实现）：用 logcat -d 快照做增量对比。
/// 每步前记录已见事件集合，step 后取差集。
pub struct EvidenceCollector {
    seen_events: HashSet<String>,
    patterns: Vec<String>,
}

impl EvidenceCollector {
    pub fn new(patterns: Vec<String>) -> Self {
        Self {
            seen_events: HashSet::new(),
            patterns,
        }
    }

    /// 从 logcat -d 输出中提取事件名（M3 用真实日志标定 patterns，§12.1）
    fn extract_events(&self, logcat_out: &str) -> HashSet<String> {
        let mut found = HashSet::new();
        for line in logcat_out.lines() {
            for pat in &self.patterns {
                if line.contains(pat.as_str()) {
                    found.insert(pat.clone());
                }
            }
        }
        found
    }

    pub async fn snapshot(&mut self, adb: &Adb, serial: &str) {
        let out = adb
            .shell(serial, &["logcat", "-d"])
            .await
            .unwrap_or_default();
        self.seen_events = self.extract_events(&out);
    }

    /// step 后调用：返回新出现的事件
    pub async fn delta(&mut self, adb: &Adb, serial: &str) -> Vec<String> {
        let out = adb
            .shell(serial, &["logcat", "-d"])
            .await
            .unwrap_or_default();
        let now = self.extract_events(&out);
        let delta: Vec<String> = now.difference(&self.seen_events).cloned().collect();
        self.seen_events = now;
        delta
    }
}

pub struct CoverageWalker {
    pub adb: Adb,
    pub serial: String,
    pub pkg: String,
    pub cfg: CoverageConfig,
    pub screen: ScreenInfo,
    pub dump_dir: Option<std::path::PathBuf>,
}

impl CoverageWalker {
    pub async fn run(&self, ev: &mut EvidenceCollector) -> Result<CoverageReport, AdbError> {
        let started = std::time::Instant::now();
        let mut report = CoverageReport {
            steps_total: self.cfg.steps.len() as u32,
            ..Default::default()
        };
        report.events_expected = self
            .cfg
            .steps
            .iter()
            .map(|s| s.expect_events.len() as u32)
            .sum();

        let iac = Interactor::new(self.adb.clone(), self.serial.clone(), self.screen);
        let mut back_streak = 0u32;

        for step in &self.cfg.steps {
            // 每步前确保还在 App 内（防上一步误入二级页或退出）
            let dump = match self.dump_once().await {
                Ok(d) => d,
                Err(_) => {
                    report.push(&step.name, StepStatus::TargetNotFound);
                    continue;
                }
            };
            if dump.top_package != self.pkg {
                iac.back().await?;
                back_streak += 1;
                iac.dwell_ms(800).await;
                let d2 = self.dump_once().await.ok();
                if d2.as_ref().map(|d| d.top_package.as_str() == self.pkg.as_str()) != Some(true) {
                    // 回不来 → 重新 launch
                    let _ = self.adb.launch_app(&self.serial, &self.pkg, None).await;
                    iac.dwell_ms(2000).await;
                    report.relaunches += 1;
                    back_streak = 0;
                }
                if back_streak >= self.cfg.max_back_before_relaunch.max(1) {
                    let _ = self.adb.launch_app(&self.serial, &self.pkg, None).await;
                    iac.dwell_ms(2000).await;
                    report.relaunches += 1;
                    back_streak = 0;
                }
            }

            // 重新 dump（可能刚 back/launch 过）
            let dump = match self.dump_once().await {
                Ok(d) => d,
                Err(_) => {
                    report.push(&step.name, StepStatus::TargetNotFound);
                    continue;
                }
            };

            // 定位目标
            let target = match self.resolve_selector(&dump, &step.target) {
                Some(t) => t,
                None => {
                    report.push(&step.name, StepStatus::TargetNotFound);
                    if self.cfg.abort_on_target_not_found {
                        break;
                    }
                    continue;
                }
            };

            // 危险按钮黑名单（防误点毁掉设备状态，与反作弊无关）
            if self.is_danger(target) {
                report.push(&step.name, StepStatus::SkippedDanger);
                continue;
            }

            // 记录动作前的事件基线
            ev.snapshot(&self.adb, &self.serial).await;

            // 执行动作
            match step.action {
                StepAction::Tap => {
                    let (cx, cy) = target.bounds.center();
                    iac.tap(cx, cy).await?;
                }
                StepAction::SwipeUp => iac.swipe_up().await?,
                StepAction::SwipeLeft => iac.swipe_left().await?,
                StepAction::Back => iac.back().await?,
                StepAction::Dwell => {}
            }
            iac.dwell_ms(step.settle_ms).await;

            // 断言：主路径 logcat 事件名（§12.1）
            let observed = ev.delta(&self.adb, &self.serial).await;
            if step.expect_events.is_empty() {
                report.push(&step.name, StepStatus::Ok);
            } else {
                let missing: Vec<String> = step
                    .expect_events
                    .iter()
                    .filter(|e| !observed.iter().any(|o| o.contains(e.as_str())))
                    .cloned()
                    .collect();
                if missing.is_empty() {
                    report.events_observed += step.expect_events.len() as u32;
                    report.push(&step.name, StepStatus::Ok);
                } else {
                    report.events_observed +=
                        (step.expect_events.len() - missing.len()) as u32;
                    report.push(
                        &step.name,
                        StepStatus::EventMissing {
                            expected: missing,
                            observed,
                        },
                    );
                }
            }
        }

        report.duration_s = started.elapsed().as_secs_f64();
        Ok(report)
    }

    async fn dump_once(&self) -> Result<UiDump, AdbError> {
        let xml = self.adb.uiautomator_dump(&self.serial, true).await?;
        UiDump::parse(&xml).map_err(|e| AdbError::CommandFailed {
            cmd: "uiautomator dump parse".into(),
            code: -1,
            stderr: e,
        })
    }

    fn resolve_selector<'a>(
        &self,
        dump: &'a UiDump,
        sel: &TargetSelector,
    ) -> Option<&'a UiNodeOwned> {
        match sel {
            TargetSelector::IdSuffix(s) => dump
                .find_by_id_suffix(s)
                .into_iter()
                .next()
                .map(|n| dump.resolve_tap_target(n)),
            TargetSelector::Text(t) => dump
                .find_by_text_exact(t)
                .into_iter()
                .next()
                .map(|n| dump.resolve_tap_target(n)),
            TargetSelector::Ratio(rx, ry) => {
                let (x, y) = (self.screen.ratio_x(*rx), self.screen.ratio_y(*ry));
                dump.nodes
                    .iter()
                    .filter(|n| n.clickable && n.bounds.contains(x, y))
                    .min_by_key(|n| n.bounds.area())
            }
        }
    }

    fn is_danger(&self, node: &UiNodeOwned) -> bool {
        if self
            .cfg
            .danger_text_patterns
            .iter()
            .any(|p| node.text.contains(p.as_str()))
        {
            return true;
        }
        if self
            .cfg
            .danger_id_hints
            .iter()
            .any(|p| node.resource_id.contains(p.as_str()))
        {
            return true;
        }
        node.class.contains("systemui")
    }
}
