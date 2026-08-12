//! Probe：廉价状态探测（§11.10：~50ms 的 mCurrentFocus 代替 0.5-3s 的全量 dump）

use crate::adb::Adb;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusInfo {
    pub package: String,
    pub activity: String,
}

impl FocusInfo {
    pub fn is_empty(&self) -> bool {
        self.package.is_empty()
    }
}

pub struct ActivityProbe {
    adb: Adb,
    serial: String,
}

impl ActivityProbe {
    pub fn new(adb: Adb, serial: String) -> Self {
        Self { adb, serial }
    }

    /// 当前前台 (package, activity)。失败时返回空 FocusInfo（不阻断主流程）
    pub async fn current_focus(&self) -> FocusInfo {
        match self.adb.current_focus(&self.serial).await {
            Ok((pkg, act)) => FocusInfo {
                package: pkg,
                activity: act,
            },
            Err(_) => FocusInfo::default(),
        }
    }

    /// 当前焦点是否是目标 App
    pub async fn is_foreground(&self, pkg: &str) -> bool {
        self.current_focus().await.package == pkg
    }

    /// 当前焦点是否系统弹窗（权限控制器等）
    pub async fn is_system_dialog(&self) -> bool {
        let f = self.current_focus().await;
        super::onboarding::is_system_package(&f.package)
    }
}
