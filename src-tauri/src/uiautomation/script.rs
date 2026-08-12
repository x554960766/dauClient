//! 剧本配置类型（设计文档 §6 Schema）

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}
fn default_total_timeout_s() -> u32 {
    150
}
fn default_max_rounds() -> u32 {
    20
}
fn default_round_interval_ms() -> u64 {
    1000
}
fn default_stable_gap_ms() -> u64 {
    800
}
fn default_max_no_target_streak() -> u32 {
    3
}
fn default_flush_dwell_s() -> u32 {
    8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonMatcher {
    #[serde(default)]
    pub text_patterns: Vec<String>,
    #[serde(default)]
    pub id_hints: Vec<String>,
}

impl Default for ButtonMatcher {
    fn default() -> Self {
        Self {
            text_patterns: Vec::new(),
            id_hints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyDialogConfig {
    pub enabled: bool,
    pub detect_keywords: Vec<String>,
    pub doc_name_keywords: Vec<String>,
    pub agree_button: ButtonMatcher,
    pub timeout_s: u32,
}

impl Default for PrivacyDialogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            detect_keywords: vec![
                "隐私".into(),
                "隐私政策".into(),
                "用户协议".into(),
                "服务协议".into(),
                "Privacy Policy".into(),
                "Terms of Service".into(),
            ],
            doc_name_keywords: vec![
                "《用户协议》".into(),
                "《隐私政策》".into(),
                "《服务协议》".into(),
                "《隐私保护指引》".into(),
                "《儿童隐私政策》".into(),
            ],
            agree_button: ButtonMatcher {
                text_patterns: vec![
                    "同意".into(),
                    "同意并继续".into(),
                    "同意并进入".into(),
                    "Agree".into(),
                    "Accept".into(),
                ],
                id_hints: vec![
                    "btn_agree".into(),
                    "btn_confirm".into(),
                    "agree".into(),
                    "dialog_positive".into(),
                    "positiveButton".into(),
                ],
            },
            timeout_s: 15,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuidePagesConfig {
    pub enabled: bool,
    pub max_swipes: u32,
    pub skip_button: ButtonMatcher,
    pub enter_button: ButtonMatcher,
    pub swipe_dwell_ms: u64,
}

impl Default for GuidePagesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_swipes: 8,
            skip_button: ButtonMatcher {
                text_patterns: vec![
                    "跳过".into(),
                    "Skip".into(),
                    "跳过引导".into(),
                    "直接进入".into(),
                ],
                id_hints: vec!["skip".into(), "btn_skip".into(), "jump".into(), "iv_close".into()],
            },
            enter_button: ButtonMatcher {
                text_patterns: vec![
                    "开启体验".into(),
                    "开始使用".into(),
                    "开始体验".into(),
                    "立即体验".into(),
                    "进入应用".into(),
                    "进入主页".into(),
                    "开始探索".into(),
                    "Get Started".into(),
                ],
                id_hints: vec!["btn_enter".into(), "btn_start".into(), "iv_enter".into()],
            },
            swipe_dwell_ms: 800,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuideOverlayConfig {
    pub enabled: bool,
    pub skip_button: ButtonMatcher,
    pub tour_button_progress_re: String,
    pub skip_progress_tours: bool,
}

impl Default for GuideOverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            skip_button: ButtonMatcher {
                text_patterns: vec![
                    "跳过指引".into(),
                    "跳过引导".into(),
                    "跳过新手".into(),
                    "我知道了".into(),
                    "已知".into(),
                    "已知悉".into(),
                    "不再提示".into(),
                    "不再显示".into(),
                    "知道了".into(),
                    "Got it".into(),
                    "Close".into(),
                ],
                id_hints: vec!["guide_skip".into(), "close_guide".into(), "iv_close".into()],
            },
            tour_button_progress_re: "^(开启功能指引|开启引导|查看指引)\\((\\d+)/(\\d+)\\)$".into(),
            skip_progress_tours: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemPermissionConfig {
    pub enabled: bool,
    pub system_packages: Vec<String>,
    pub title_keywords: Vec<String>,
    pub allow_button_preferred: Vec<String>,
    pub deny_button_patterns: Vec<String>,
    pub max_consecutive_dialogs: u32,
}

impl Default for SystemPermissionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            system_packages: vec![
                "com.android.permissioncontroller".into(),
                "com.android.packageinstaller".into(),
                "com.android.systemui".into(),
                "com.google.android.permissioncontroller".into(),
                "com.google.android.packageinstaller".into(),
            ],
            title_keywords: vec![
                "是否允许".into(),
                "应用想要".into(),
                "要允许".into(),
                "Allow".into(),
            ],
            allow_button_preferred: vec![
                "仅在使用中允许".into(),
                "本次运行允许".into(),
                "使用应用时允许".into(),
                "允许".into(),
                "While using the app".into(),
            ],
            deny_button_patterns: vec![
                "拒绝".into(),
                "不允许".into(),
                "禁止".into(),
                "Deny".into(),
                "Don't allow".into(),
            ],
            max_consecutive_dialogs: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeDetectionConfig {
    pub home_id_hints: Vec<String>,
    pub home_text_tokens: Vec<String>,
    pub home_activity_names: Vec<String>,
    pub require_bottom_nav_as_fallback: bool,
    pub stable_detect: bool,
    pub stable_detect_gap_ms: u64,
    pub max_no_target_streak: u32,
}

impl Default for HomeDetectionConfig {
    fn default() -> Self {
        Self {
            home_id_hints: Vec::new(),
            home_text_tokens: vec!["首页".into(), "推荐".into(), "我的".into()],
            home_activity_names: vec!["MainActivity".into(), "HomeActivity".into()],
            require_bottom_nav_as_fallback: true,
            stable_detect: true,
            stable_detect_gap_ms: default_stable_gap_ms(),
            max_no_target_streak: default_max_no_target_streak(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct H5FallbackConfig {
    pub enabled: bool,
    pub candidate_points: Vec<(f32, f32)>,
}

impl Default for H5FallbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // v1.3：候选点按命中率排序。(0.5, 0.82) 命中晨视频「开启体验」按钮中心 (540,1968)
            candidate_points: vec![
                (0.5, 0.82),
                (0.5, 0.85),
                (0.75, 0.85),
                (0.5, 0.78),
                (0.5, 0.92),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardingConfig {
    pub enabled: bool,
    pub total_timeout_s: u32,
    pub max_rounds: u32,
    pub round_interval_ms: u64,
    pub privacy_dialog: PrivacyDialogConfig,
    pub guide_pages: GuidePagesConfig,
    pub guide_overlay: GuideOverlayConfig,
    pub system_permission: SystemPermissionConfig,
    pub home_detection: HomeDetectionConfig,
    pub h5_fallback: H5FallbackConfig,
}

impl Default for OnboardingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            total_timeout_s: default_total_timeout_s(),
            max_rounds: default_max_rounds(),
            round_interval_ms: default_round_interval_ms(),
            privacy_dialog: PrivacyDialogConfig::default(),
            guide_pages: GuidePagesConfig::default(),
            guide_overlay: GuideOverlayConfig::default(),
            system_permission: SystemPermissionConfig::default(),
            home_detection: HomeDetectionConfig::default(),
            h5_fallback: H5FallbackConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrepareConfig {
    pub runtime_permissions_from_apk: bool,
    pub extra_permissions: Vec<String>,
    pub dismiss_keyguard: bool,
    pub disable_animations: bool,
    pub user_foreground_timeout_s: u32,
}

impl Default for PrepareConfig {
    fn default() -> Self {
        Self {
            runtime_permissions_from_apk: true,
            extra_permissions: Vec::new(),
            dismiss_keyguard: true,
            disable_animations: true,
            user_foreground_timeout_s: 30,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CoverageStep {
    pub name: String,
    pub target: TargetSelector,
    pub action: StepAction,
    #[serde(default)]
    pub expect_events: Vec<String>,
    #[serde(default = "default_settle_ms")]
    pub settle_ms: u64,
}

fn default_settle_ms() -> u64 {
    1500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TargetSelector {
    IdSuffix(String),
    Text(String),
    Ratio(f32, f32),
}

impl Default for TargetSelector {
    fn default() -> Self {
        TargetSelector::Text(String::new())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum StepAction {
    Tap,
    SwipeUp,
    SwipeLeft,
    Back,
    Dwell,
}

impl Default for StepAction {
    fn default() -> Self {
        StepAction::Tap
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoverageConfig {
    pub enabled: bool,
    pub abort_on_target_not_found: bool,
    pub steps: Vec<CoverageStep>,
    pub danger_text_patterns: Vec<String>,
    pub danger_id_hints: Vec<String>,
    pub max_back_before_relaunch: u32,
}

impl Default for CoverageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            abort_on_target_not_found: false,
            steps: Vec::new(),
            danger_text_patterns: vec![
                "退出登录".into(),
                "退出".into(),
                "注销".into(),
                "登出".into(),
                "Log out".into(),
                "Sign out".into(),
                "清除".into(),
                "清空".into(),
                "删除".into(),
                "卸载".into(),
                "Delete".into(),
                "Clear".into(),
                "Remove".into(),
                "设置".into(),
                "Settings".into(),
                "关于".into(),
                "About".into(),
                "重置".into(),
                "Reset".into(),
                "恢复出厂".into(),
            ],
            danger_id_hints: vec![
                "logout".into(),
                "exit".into(),
                "sign_out".into(),
                "unbind".into(),
                "delete_account".into(),
                "clear_data".into(),
                "reset".into(),
            ],
            max_back_before_relaunch: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceConfig {
    pub logcat_event_patterns: Vec<String>,
    pub logcat_event_tag_hints: Vec<String>,
    pub proxy_umeng_host_hints: Vec<String>,
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            logcat_event_patterns: Vec::new(),
            logcat_event_tag_hints: vec![
                "UMLog".into(),
                "MobclickAgent".into(),
                "com.umeng".into(),
                "onEvent".into(),
            ],
            proxy_umeng_host_hints: vec!["umeng.com".into(), "umtrack.com".into(), "umengcloud.com".into()],
        }
    }
}

/// UI 自动化总配置（EngineConfig 挂载）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiAutomationConfig {
    pub enabled: bool,
    pub onboarding: OnboardingConfig,
    pub prepare: PrepareConfig,
    pub coverage: CoverageConfig,
    pub evidence: EvidenceConfig,
    pub flush_dwell_s: u32,
}

impl Default for UiAutomationConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            onboarding: OnboardingConfig::default(),
            prepare: PrepareConfig::default(),
            coverage: CoverageConfig::default(),
            evidence: EvidenceConfig::default(),
            flush_dwell_s: default_flush_dwell_s(),
        }
    }
}
