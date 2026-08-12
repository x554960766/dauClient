//! UI 自动化子系统（设计文档 umeng-android-ui-automation-design.md v1.2）
//!
//! 模块划分：
//! - dump        uiautomator XML 解析与查询（带 parent 索引，§12.3）
//! - interactor  input tap/swipe/keyevent 封装（无抖动，§3.2 修正版）
//! - probe       廉价状态探测（current_focus，§11.10）
//! - script      配置类型（§6 Schema）
//! - prepare     prepare_device / teardown_device（§12.2）
//! - onboarding  多层引导循环（§4 + §12.5 修正版）
//! - coverage    CoverageWalker 确定性埋点覆盖遍历（§5 + §12.6）

pub mod coverage;
pub mod dump;
pub mod interactor;
pub mod onboarding;
pub mod prepare;
pub mod probe;
pub mod script;

pub use dump::{Bounds, UiDump, UiNodeOwned};
pub use interactor::{Interactor, ScreenInfo};
pub use onboarding::{OnboardingRunner, OnboardingStats};
pub use prepare::{extract_permissions, prepare_device, teardown_device, PrepareOutcome};
pub use script::{
    CoverageConfig, CoverageStep, EvidenceConfig, OnboardingConfig, PrepareConfig,
    StepAction, TargetSelector, UiAutomationConfig,
};
