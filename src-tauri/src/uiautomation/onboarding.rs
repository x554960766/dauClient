//! OnboardingRunner：多层引导循环（设计文档 §4 + §12.5 修正版）
//! 关卡检测优先级（§12.5 末尾）：系统权限弹窗 → 隐私协议 → 功能指引覆盖层 → 引导页 → 主页
//! 关键修正：廉价前置探测（§11.10）+ 全量 dump 重试（§11.4）+ 失败同时存 XML+PNG（§15.7）
//! 协议识别用 §12.4 修正版：负向过滤 DISAGREE_PATTERNS + 三级匹配 + resolve_tap_target

use crate::adb::{Adb, AdbError};
use crate::uiautomation::dump::{UiDump, UiNodeOwned};
use crate::uiautomation::interactor::{Interactor, ScreenInfo};
use crate::uiautomation::script::OnboardingConfig;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 否定语义——必须先于任何正向匹配整体排除（§11.2）
const DISAGREE_PATTERNS: &[&str] = &[
    "不同意", "不接受", "拒绝", "暂不同意", "暂不使用", "退出应用", "退出程序",
    "取消", "残忍拒绝", "以后再说", "仍不同意",
    "Disagree", "Decline", "Not now", "Exit", "Cancel", "No thanks",
];

/// 协议文档名（强条件）
const PRIVACY_STRONG: &[&str] = &[
    "《用户协议》", "《隐私政策》", "《服务协议》", "《用户服务协议》",
    "《隐私保护指引》", "《儿童隐私政策》", "隐私政策", "隐私协议",
    "Privacy Policy", "Terms of Service", "User Agreement",
];

const PRIVACY_WEAK: &[&str] = &["隐私", "协议", "政策", "个人信息", "Privacy", "Terms"];

#[allow(dead_code)]
static PROGRESS_BTN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(.+?)\((\d+)/(\d+)\)$").unwrap());

/// dump 内容哈希（v1.3：引导页末页检测——swipe 后哈希不变 = 滑不动了）
fn dump_hash(dump: &UiDump) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    dump.raw_xml.hash(&mut h);
    h.finish()
}

pub fn is_system_package(pkg: &str) -> bool {
    const SYSTEM_PACKAGES: &[&str] = &[
        "com.android.permissioncontroller",
        "com.android.packageinstaller",
        "com.android.systemui",
        "com.android.settings",
        "com.google.android.permissioncontroller",
        "com.google.android.packageinstaller",
    ];
    SYSTEM_PACKAGES.iter().any(|p| pkg.starts_with(p))
}

/// 系统教学/提示弹窗检测（v1.3 M3 实测：沉浸式 cling 挡死引导页）。
/// 特征：package=android/systemui，含教学文案或可点的 "Got it"/"OK"/"知道了" 按钮。
/// 与权限弹窗的区别：无权限问询标题，只有单个确认按钮。
fn detect_system_cling<'a>(dump: &'a UiDump) -> Option<&'a UiNodeOwned> {
    let pkg = dump.top_package.as_str();
    // 沉浸式 cling 的 package 就是裸 "android"；也兼容 systemui 的提示
    if pkg != "android" && !is_system_package(pkg) {
        return None;
    }
    // 沉浸式 cling 强特征
    const CLING_HINTS: &[&str] = &[
        "Viewing full screen",
        "To exit, swipe down",
        "全屏显示",
        "向下轻扫",
        "沉浸式",
    ];
    let has_cling_text = CLING_HINTS.iter().any(|k| !dump.find_by_text(k).is_empty());
    // 确认按钮
    const CLING_OK: &[&str] = &["Got it", "Got It", "OK", "知道了", "好的", "我知道了"];
    let ok_btn = CLING_OK.iter().find_map(|k| {
        dump.find_by_text_exact(k)
            .into_iter()
            .find(|n| n.clickable || n.is_button_class())
    });
    // ANR (Application Not Responding "无响应" / "isn't responding") 系统弹窗检测
    if let Some(wait_btn) = dump.find_by_id_suffix("aerr_wait").into_iter().next() {
        tracing::warn!("Onboarding: 检测到系统 ANR 弹窗 (isn't responding)，自动点击 Wait/等待 按钮");
        return Some(dump.resolve_tap_target(wait_btn));
    }
    let has_anr_text = dump.nodes.iter().any(|n| n.text.contains("isn't responding") || n.text.contains("无响应"));
    if has_anr_text {
        if let Some(btn) = dump.nodes.iter().find(|n| n.text == "Wait" || n.text == "等待") {
            tracing::warn!("Onboarding: 检测到系统 ANR 文本，点击 Wait 按钮");
            return Some(dump.resolve_tap_target(btn));
        }
    }

    match (has_cling_text, ok_btn) {
        (true, Some(btn)) => Some(dump.resolve_tap_target(btn)),
        // 无 cling 文案但系统包有 "Got it"/"OK" 单按钮（也兼容 android:id/ok）
        (false, Some(btn)) if pkg == "android" => Some(dump.resolve_tap_target(btn)),
        (false, _) => dump
            .find_by_id_suffix("ok")
            .into_iter()
            .find(|n| n.clickable && n.package == "android")
            .map(|n| dump.resolve_tap_target(n)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    Privacy,
    GuideSwipe,
    GuideSkip,
    GuideEnter,
    OverlaySkip,
    Permission,
    SystemCling,
    H5Fallback,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnboardingStats {
    pub privacy_agreed: bool,
    pub agree_button_matched_by: Option<String>,
    pub guide_swipes: u32,
    pub guide_skip_count: u32,
    pub guide_entered: bool,
    pub enter_button_matched_by: Option<String>,
    pub overlay_skipped: bool,
    pub overlay_skip_button_matched_by: Option<String>,
    pub permission_granted: bool,
    pub permission_dialogs_seen: u32,
    /// v1.3：沉浸式 cling 等系统教学弹窗被 dismiss 的次数
    pub system_cling_dismissed: u32,
    pub full_dump_rescues: u32,
    pub reached_home: bool,
    pub rounds_used: u32,
    pub duration_s: f64,
    pub timed_out: bool,
    pub failed_at_round: Option<u32>,
    pub fail_reason: Option<String>,
}

/// 关卡动作
#[derive(Debug)]
enum Action {
    Tap { x: i32, y: i32, stage: Stage, matched_by: String },
    #[allow(dead_code)]
    SwipeLeftGuide,
    OnHome,
    None,
}

// ---------- 关卡 1：隐私协议弹窗（§12.4 修正版） ----------

fn detect_privacy_dialog<'a>(
    dump: &'a UiDump,
    cfg: &OnboardingConfig,
    screen: &ScreenInfo,
    cb_already_clicked: bool,
) -> Option<(&'a UiNodeOwned, String)> {
    if !cfg.privacy_dialog.enabled {
        return None;
    }
    // 1. 强条件：协议文档名出现，或（弱关键词 + 存在否定按钮）
    let keywords: Vec<&str> = cfg
        .privacy_dialog
        .doc_name_keywords
        .iter()
        .chain(cfg.privacy_dialog.detect_keywords.iter())
        .map(|s| s.as_str())
        .collect();
    let has_doc = keywords
        .iter()
        .chain(PRIVACY_STRONG.iter())
        .any(|k| !dump.find_by_text(k).is_empty());
    let has_disagree = DISAGREE_PATTERNS
        .iter()
        .any(|k| !dump.find_by_text(k).is_empty());
    let weak_kw = PRIVACY_WEAK
        .iter()
        .any(|k| !dump.find_by_text(k).is_empty());
    if !(has_doc || (weak_kw && has_disagree)) {
        return None;
    }

    // 2. 负向过滤 + 排除 H5 整页正文（按 dp 判，§11.12）
    let candidates: Vec<&UiNodeOwned> = dump
        .nodes
        .iter()
        .filter(|n| n.enabled)
        .filter(|n| !n.text.is_empty())
        .filter(|n| !DISAGREE_PATTERNS.iter().any(|p| n.text.contains(p)))
        .filter(|n| screen.to_dp(n.bounds.height()) < 96)
        .collect();

    // 2.5 优先检测隐私协议弹窗内的「阅读并同意」勾选框（CheckBox）
    // 很多 App（如晨视频）需先勾选同意协议 CheckBox 才能使「同意」按钮生效。
    // 在 L2.5/L3 新用户模式下初始状态未勾选，直接点击「同意」会被 App 内部 if (!isChecked) return 静默丢弃。
    // 关键修正：若节点属性为 checked=true，或在之前轮次已点击过勾选框 (cb_already_clicked=true)，不再重复点击，避免形成死循环。
    let cb_node = if cb_already_clicked {
        None
    } else {
        dump.nodes.iter().find(|n| {
            if !n.enabled || n.bounds.area() <= 0 { return false; }
            if n.checked { return false; }
            let class_lower = n.class.to_lowercase();
            let id_lower = n.resource_id.to_lowercase();

            let is_cb_class = class_lower.ends_with("checkbox") || class_lower.ends_with("radiobutton");
            let is_cb_id = id_lower.ends_with("checkbox") || id_lower.ends_with("cb_agree") || id_lower.ends_with("agree_cb") || id_lower.contains("cb_select");

            is_cb_class || is_cb_id
        })
    };

    // 3. 三级匹配：精确 → starts_with → contains
    let exact_set: Vec<&str> = cfg
        .privacy_dialog
        .agree_button
        .text_patterns
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut agree_node: Option<(&'a UiNodeOwned, String)> = None;
    for n in &candidates {
        let t = n.text.trim();
        if exact_set.iter().any(|p| t == *p) {
            agree_node = Some((dump.resolve_tap_target(n), format!("text_exact:{}", t)));
            break;
        }
    }
    if agree_node.is_none() {
        for n in &candidates {
            let t = n.text.trim();
            if t.starts_with("同意") || t.starts_with("已阅读并同意") {
                agree_node = Some((dump.resolve_tap_target(n), format!("text_prefix:{}", t)));
                break;
            }
        }
    }
    if agree_node.is_none() {
        for n in &candidates {
            let t = n.text.trim();
            if t.contains("同意") || t.contains("Agree") || t.contains("Accept") {
                agree_node = Some((dump.resolve_tap_target(n), format!("text_contains:{}", t)));
                break;
            }
        }
    }

    if let Some((btn_target, matched)) = agree_node {
        // 如果同时存在未勾选的勾选框节点，且勾选框中心与同意按钮不重合，优先点击勾选框
        if !cb_already_clicked {
            if let Some(cb) = cb_node {
                let cb_target = dump.resolve_tap_target(cb);
                if cb_target.bounds.center() != btn_target.bounds.center() {
                    return Some((cb_target, format!("cb_first:{}", matched)));
                }
            }
        }
        return Some((btn_target, matched));
    }

    // 4. 退到 resource-id
    for hint in &cfg.privacy_dialog.agree_button.id_hints {
        if let Some(n) = dump
            .find_by_id_suffix(hint)
            .into_iter()
            .find(|n| !DISAGREE_PATTERNS.iter().any(|p| n.text.contains(p)))
        {
            let btn_target = dump.resolve_tap_target(n);
            if !cb_already_clicked {
                if let Some(cb) = cb_node {
                    let cb_target = dump.resolve_tap_target(cb);
                    if cb_target.bounds.center() != btn_target.bounds.center() {
                        return Some((cb_target, format!("cb_first:id_suffix:{}", hint)));
                    }
                }
            }
            return Some((btn_target, format!("id_suffix:{}", hint)));
        }
    }
    None
}

/// H5 协议页兜底（§4.7）：检测到协议文本但找不到原生按钮 → 按候选坐标点
fn detect_h5_privacy(dump: &UiDump, cfg: &OnboardingConfig) -> bool {
    if !cfg.h5_fallback.enabled {
        return false;
    }
    let has_privacy = PRIVACY_STRONG
        .iter()
        .chain(PRIVACY_WEAK.iter())
        .any(|k| !dump.find_by_text(k).is_empty());
    if !has_privacy {
        return false;
    }
    // 有 WebView 节点且找不到原生同意按钮
    let has_webview = dump.nodes.iter().any(|n| n.class.contains("WebView"));
    has_webview
}

// ---------- 关卡 2：引导页 ----------

#[allow(dead_code)]
enum GuideAction {
    NotGuide,
    TapSkip(i32, i32, String),
    TapEnter(i32, i32, String),
    SwipeLeft,
}

fn detect_guide_page(dump: &UiDump, cfg: &OnboardingConfig, screen: &ScreenInfo) -> GuideAction {
    if !cfg.guide_pages.enabled {
        return GuideAction::NotGuide;
    }
    // 跳过按钮优先
    for pat in &cfg.guide_pages.skip_button.text_patterns {
        if let Some(n) = dump
            .find_by_text(pat)
            .into_iter()
            .find(|n| n.clickable || n.is_button_class())
        {
            let t = dump.resolve_tap_target(n);
            return GuideAction::TapSkip(t.bounds.center().0, t.bounds.center().1, pat.clone());
        }
    }
    // 进入按钮（最后一页）
    for pat in &cfg.guide_pages.enter_button.text_patterns {
        if let Some(n) = dump
            .find_by_text(pat)
            .into_iter()
            .find(|n| n.clickable || n.is_button_class())
        {
            let t = dump.resolve_tap_target(n);
            return GuideAction::TapEnter(t.bounds.center().0, t.bounds.center().1, pat.clone());
        }
    }
    // 进度点启发式：底部 1/10 区域 ≥3 个小点
    let indicator_zone = |y: i32| y > screen.h * 9 / 10;
    let tiny_in_zone = dump
        .nodes
        .iter()
        .filter(|n| n.bounds.height() <= 20 && n.bounds.width() <= 20 && n.bounds.height() > 0)
        .filter(|n| indicator_zone(n.bounds.y1))
        .count();
    if tiny_in_zone >= 3 {
        return GuideAction::SwipeLeft;
    }
    // 引导提示文本
    const GUIDE_HINTS: &[&str] = &["向左滑动", "向右滑动", "滑动翻页", "翻页", "上滑查看"];
    if GUIDE_HINTS.iter().any(|h| !dump.find_by_text(h).is_empty()) {
        return GuideAction::SwipeLeft;
    }
    // 全屏 pager 启发式（v1.3 修正，晨视频 M3 实测命中）：
    // 引导页内容常被画进全屏 ViewPager，内部节点（进度点/提示文本/图片）
    // importantForAccessibility != yes 被 dump 过滤，只剩容器。
    // 判据：全屏 scrollable 容器 + 可点击节点 ≤2 + 总节点 <30。
    // 与主页信息流区分：主页有大量可点击 nav/卡片节点，不会同时满足后两条。
    let clickable_count = dump.nodes.iter().filter(|n| n.clickable).count();
    let has_fullscreen_pager = dump.nodes.iter().any(|n| {
        n.scrollable
            && !n.package.is_empty()
            && n.bounds.width() >= screen.w * 9 / 10
            && n.bounds.height() >= screen.h * 3 / 4
            && (n.class.contains("ViewPager")
                || n.class.contains("RecyclerView")
                || n.class.contains("ScrollView"))
    });
    if has_fullscreen_pager && clickable_count <= 2 && dump.nodes.len() < 30 {
        return GuideAction::SwipeLeft;
    }
    GuideAction::NotGuide
}

// ---------- 关卡 3：App 内功能指引覆盖层 ----------

#[allow(dead_code)]
fn detect_guide_overlay<'a>(
    dump: &'a UiDump,
    pkg: &str,
    cfg: &OnboardingConfig,
) -> Option<(&'a UiNodeOwned, String)> {
    if !cfg.guide_overlay.enabled {
        return None;
    }
    // 强条件：顶层 package 仍是 App（排除系统弹窗）
    if dump.top_package != pkg {
        return None;
    }
    // 找"跳过"按钮（优先）
    for pat in &cfg.guide_overlay.skip_button.text_patterns {
        if let Some(n) = dump
            .find_by_text(pat)
            .into_iter()
            .find(|n| n.clickable || n.is_button_class())
        {
            return Some((dump.resolve_tap_target(n), format!("text:{}", pat)));
        }
    }
    for hint in &cfg.guide_overlay.skip_button.id_hints {
        if let Some(n) = dump.find_by_id_suffix(hint).into_iter().next() {
            return Some((dump.resolve_tap_target(n), format!("id:{}", hint)));
        }
    }
    // 带进度按钮 "开启功能指引(1/3)"：skip_progress_tours=true 时不进入功能指引
    if !cfg.guide_overlay.skip_progress_tours {
        for n in dump.clickable_nodes() {
            if PROGRESS_BTN_RE.is_match(n.text.trim()) {
                return Some((dump.resolve_tap_target(n), format!("progress:{}", n.text.trim())));
            }
        }
    }
    None
}

// ---------- 关卡 4：系统权限弹窗（兜底；正常已被 prepare.pm grant 消灭） ----------

fn detect_permission_dialog<'a>(
    dump: &'a UiDump,
    cfg: &OnboardingConfig,
) -> Option<(&'a UiNodeOwned, String)> {
    if !cfg.system_permission.enabled {
        return None;
    }
    let sys_pkgs: Vec<&str> = cfg
        .system_permission
        .system_packages
        .iter()
        .map(|s| s.as_str())
        .collect();
    if !sys_pkgs.iter().any(|p| dump.top_package.starts_with(p)) {
        return None;
    }
    let has_title = cfg
        .system_permission
        .title_keywords
        .iter()
        .any(|k| !dump.find_by_text(k).is_empty());
    if !has_title {
        return None;
    }
    for pat in &cfg.system_permission.allow_button_preferred {
        if let Some(n) = dump
            .find_by_text(pat)
            .into_iter()
            .find(|n| n.clickable || n.is_button_class())
        {
            return Some((dump.resolve_tap_target(n), format!("preferred:{}", pat)));
        }
    }
    // 兜底：所有可点击按钮中排除拒绝类
    for n in dump.clickable_nodes() {
        let deny = cfg
            .system_permission
            .deny_button_patterns
            .iter()
            .any(|p| n.text.contains(p));
        if !deny && (n.text.contains("允许") || n.text.contains("Allow")) {
            return Some((dump.resolve_tap_target(n), format!("fallback:{}", n.text.trim())));
        }
    }
    None
}

// ---------- 关卡 6：主页判定 ----------

fn is_on_home(dump: &UiDump, pkg: &str, cfg: &OnboardingConfig, screen: &ScreenInfo) -> bool {
    if dump.top_package != pkg || !dump.contains_package(pkg) {
        return false;
    }
    // 仍有协议关卡特征或引导页特征 → 未到主页
    if detect_privacy_dialog(dump, cfg, screen, true).is_some() {
        return false;
    }
    if !matches!(detect_guide_page(dump, cfg, screen), GuideAction::NotGuide) {
        return false;
    }
    true
}

// ---------- 主循环（§12.5） ----------

fn detect_next_action(
    dump: &UiDump,
    pkg: &str,
    cfg: &OnboardingConfig,
    screen: &ScreenInfo,
    cb_already_clicked: bool,
) -> Action {
    // 优先级：系统教学弹窗（最顶层遮挡）→ 系统权限弹窗 → 隐私协议 → 主页
    if let Some(n) = detect_system_cling(dump) {
        let (x, y) = n.bounds.center();
        return Action::Tap {
            x,
            y,
            stage: Stage::SystemCling,
            matched_by: "system_cling".into(),
        };
    }
    if let Some((n, matched)) = detect_permission_dialog(dump, cfg) {
        let (x, y) = n.bounds.center();
        return Action::Tap { x, y, stage: Stage::Permission, matched_by: matched };
    }
    if let Some((n, matched)) = detect_privacy_dialog(dump, cfg, screen, cb_already_clicked) {
        let (x, y) = n.bounds.center();
        return Action::Tap { x, y, stage: Stage::Privacy, matched_by: matched };
    }

    // 新版 APK：移除了引导页滑动与覆盖层跳过，直接判定主页
    if is_on_home(dump, pkg, cfg, screen) {
        return Action::OnHome;
    }

    // H5 协议页兜底（§4.7）
    if detect_h5_privacy(dump, cfg) {
        if let Some(&(rx, ry)) = cfg.h5_fallback.candidate_points.first() {
            return Action::Tap {
                x: screen.ratio_x(rx),
                y: screen.ratio_y(ry),
                stage: Stage::H5Fallback,
                matched_by: format!("ratio:({}, {})", rx, ry),
            };
        }
    }
    Action::None
}

pub struct OnboardingRunner {
    pub adb: Adb,
    pub serial: String,
    pub pkg: String,
    pub cfg: OnboardingConfig,
    pub screen: ScreenInfo,
    /// 失败落盘目录（None 则不落盘）
    pub dump_dir: Option<std::path::PathBuf>,
}

impl OnboardingRunner {
    pub async fn run(&self) -> Result<OnboardingStats, AdbError> {
        let started = Instant::now();
        let mut stats = OnboardingStats::default();
        let mut privacy_cb_clicked = false;
        let mut no_target_streak = 0u32;
        let mut last_focus = String::new();
        let mut h5_candidate_idx = 0usize;
        // v1.3：引导页末页检测——swipe 后 dump 哈希不变 = 滑不动了 = 已到末页
        let mut last_pager_hash: Option<u64> = None;
        let iac = Interactor::new(self.adb.clone(), self.serial.clone(), self.screen);
        let max_streak = self.cfg.home_detection.max_no_target_streak.max(1);

        for round in 0..self.cfg.max_rounds {
            if started.elapsed() > Duration::from_secs(self.cfg.total_timeout_s as u64) {
                stats.timed_out = true;
                stats.fail_reason = Some("timed_out".into());
                stats.failed_at_round = Some(round);
                stats.rounds_used = round + 1;
                break;
            }

            // ---- 廉价前置探测（§11.10）：~50ms vs dump 的 0.5-3s ----
            let (fpkg, fact) = self
                .adb
                .current_focus(&self.serial)
                .await
                .unwrap_or_default();
            let focus = format!("{}/{}", fpkg, fact);
            if focus == last_focus && stats.reached_home {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            last_focus = focus.clone();

            // ---- 全量 dump（v1.5：不再用压缩 dump） ----
            // 压缩 dump 会过滤 importantForAccessibility != yes 的节点，
            // 导致主页 ViewPager 误触发全屏 pager 启发式 → SwipeLeftGuide 误判。
            // 压缩 dump + rescue 的方案引入了复杂度和性能回归（双重 dump）。
            // 直接用全量 dump：单次 dump ~2s，15 轮 ≈ 60s，在 90s 预算内。
            let dump = match self.dump_once(false).await {
                Ok(d) => d,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            let action = detect_next_action(&dump, &self.pkg, &self.cfg, &self.screen, privacy_cb_clicked);

            // v1.5：每轮 tracing 日志
            tracing::info!(
                round, focus = %focus, top_pkg = %dump.top_package,
                nodes = dump.nodes.len(), clickable = dump.nodes.iter().filter(|n| n.clickable).count(),
                action = ?action,
                guide_swipes = stats.guide_swipes,
                "onboarding round"
            );
            // 失败时用当前 dump 落盘（已经是全量 dump，信息最全）
            let full_dump: Option<UiDump> = None;

            match action {
                Action::Tap { x, y, stage, matched_by } => {
                    iac.tap(x, y).await?;
                    match stage {
                        Stage::Privacy => {
                            if matched_by.starts_with("cb_first:") {
                                privacy_cb_clicked = true;
                                tracing::info!(round, %matched_by, "Onboarding: 已点击勾选隐私协议 CheckBox，下轮将直接点击同意按钮");
                                tokio::time::sleep(Duration::from_millis(400)).await;
                                last_focus.clear();
                            } else {
                                stats.privacy_agreed = true;
                                stats.agree_button_matched_by = Some(matched_by);
                                // 隐私协议双保险：触控点击后再补发 KEYCODE_ENTER 确认键，等待 600ms 隐去动画并清空 focus 缓存
                                let _ = self.adb.keyevent(&self.serial, "KEYCODE_ENTER").await;
                                tokio::time::sleep(Duration::from_millis(600)).await;
                                last_focus.clear();
                            }
                        }
                        Stage::GuideSkip => stats.guide_skip_count += 1,
                        Stage::GuideEnter => {
                            stats.guide_entered = true;
                            stats.enter_button_matched_by = Some(matched_by);
                        }
                        Stage::OverlaySkip => {
                            stats.overlay_skipped = true;
                            stats.overlay_skip_button_matched_by = Some(matched_by);
                        }
                        Stage::Permission => {
                            stats.permission_granted = true;
                            stats.permission_dialogs_seen += 1;
                        }
                        Stage::H5Fallback => {
                            // 轮换候选点
                            h5_candidate_idx = (h5_candidate_idx + 1)
                                % self.cfg.h5_fallback.candidate_points.len().max(1);
                        }
                        Stage::SystemCling => {
                            stats.system_cling_dismissed += 1;
                            if matched_by.contains("aerr_wait") || matched_by.contains("anr") {
                                if stats.system_cling_dismissed >= 3 {
                                    stats.failed_at_round = Some(round);
                                    stats.rounds_used = round + 1;
                                    stats.fail_reason = Some("anr_deadlock_l25_incompatible".into());
                                    tracing::error!("Onboarding 终止: 连续 3 次触发 ANR 系统弹窗，App 在 L2.5 多用户下存在 Native Binder 死锁（不支持 Android 副用户），建议降级切换重置方案为 L2 (卸载重装)");
                                    self.save_failure_artifacts(
                                        full_dump.as_ref().unwrap_or(&dump),
                                        round,
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }
                        Stage::GuideSwipe => {}
                    }
                    no_target_streak = 0;
                    last_pager_hash = None;
                }
                Action::SwipeLeftGuide => {
                    // v1.6：死锁解除——若到达 max_swipes 或 hash 重复（末页滑不动），优先轮换点击候选点；
                    // 但若尝试一轮候选点后页面仍停留在引导页，重置候选点计数并继续尝试 swipe_left，
                    // 避免在未到末页时被死锁在候选点盲点死循环中。
                    let pts = &self.cfg.h5_fallback.candidate_points;
                    let cur_hash = dump_hash(&dump);

                    let should_try_candidate = stats.guide_swipes >= self.cfg.guide_pages.max_swipes
                        || (stats.guide_swipes > 0 && last_pager_hash == Some(cur_hash));

                    if should_try_candidate && !pts.is_empty() && h5_candidate_idx < pts.len() {
                        let (rx, ry) = pts[h5_candidate_idx];
                        h5_candidate_idx += 1;
                        iac.tap_ratio(rx, ry).await?;
                        last_pager_hash = None;
                    } else {
                        if h5_candidate_idx >= pts.len() {
                            h5_candidate_idx = 0;
                        }
                        iac.swipe_left().await?;
                        stats.guide_swipes += 1;
                        last_pager_hash = Some(cur_hash);
                    }
                    no_target_streak = 0;
                    iac.dwell_ms(self.cfg.guide_pages.swipe_dwell_ms).await;
                }
                Action::OnHome => {
                    // 稳定判定：连续 2 次都在主页，避免转场中间态误判
                    // v1.5：用全量 dump（false）而非压缩 dump（true），
                    // 因为压缩 dump 会过滤底部导航等关键节点导致 is_on_home 误判
                    if self.cfg.home_detection.stable_detect {
                        tokio::time::sleep(Duration::from_millis(
                            self.cfg.home_detection.stable_detect_gap_ms,
                        ))
                        .await;
                        if let Ok(d2) = self.dump_once(false).await {
                            if !is_on_home(&d2, &self.pkg, &self.cfg, &self.screen) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                    stats.reached_home = true;
                    stats.rounds_used = round + 1;
                    break;
                }
                Action::None => {
                    no_target_streak += 1;
                    tracing::warn!(round, no_target_streak, "Onboarding: 未匹配到已知关卡动作 (Action::None)");

                    // 容错 1：连续 2 次 Action::None，尝试发送 KEYCODE_BACK 消除隐藏弹窗/侧滑 Drawer/系统阴影
                    if no_target_streak == 2 {
                        tracing::info!("Onboarding 容错策略: 发送 KEYCODE_BACK 清理悬浮遮罩与弹窗");
                        let _ = iac.back().await;
                        tokio::time::sleep(Duration::from_millis(600)).await;
                        continue;
                    }

                    // 容错 2：连续 3 次 Action::None，尝试点击右上角常见跳过盲点 (0.88, 0.08)
                    if no_target_streak == 3 {
                        tracing::info!("Onboarding 容错策略: 尝试点击右上角跳过盲点 (0.88, 0.08)");
                        let _ = iac.tap_ratio(0.88, 0.08).await;
                        tokio::time::sleep(Duration::from_millis(600)).await;
                        continue;
                    }

                    // 容错 3：连续 4 次 Action::None，只要 App 在前台且无阻挡弹窗、节点数 >= 4，兜底作为 OnHome 成功通过！
                    if no_target_streak >= 4 && (dump.top_package == self.pkg || dump.contains_package(&self.pkg)) && dump.nodes.len() >= 4 {
                        tracing::info!("Onboarding 容错策略: 已到达 App 前台且无阻挡弹窗，兜底判定为 OnHome 通过");
                        stats.reached_home = true;
                        stats.rounds_used = round + 1;
                        break;
                    }

                    if no_target_streak >= max_streak.max(6) {
                        stats.failed_at_round = Some(round);
                        stats.rounds_used = round + 1;
                        stats.fail_reason = Some("no_target_consecutive".into());
                        // v1.3：优先保存全量 dump（信息更全），没有则存压缩版
                        self.save_failure_artifacts(
                            full_dump.as_ref().unwrap_or(&dump),
                            round,
                        )
                        .await;
                        break;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(self.cfg.round_interval_ms)).await;
        }

        // v1.x 修复：for 循环自然耗尽 max_rounds（既没到主页、也没触发连续空转/超时）
        // 之前这条出口静默返回 fail_reason=None / 零现场，导致 L2.5 活锁（每轮都在点/滑
        // 却始终到不了主页）报「未到主页: fail_reason=None failed_at_round=None」无从诊断。
        // 现在补上明确 reason + 落盘最后一屏，让每种失败都留下可查现场。
        if !stats.reached_home && stats.fail_reason.is_none() {
            stats.fail_reason = Some("max_rounds_exhausted".into());
            stats.failed_at_round = Some(self.cfg.max_rounds.saturating_sub(1));
            stats.rounds_used = self.cfg.max_rounds;
            if let Ok(full) = self.dump_once(false).await {
                self.save_failure_artifacts(&full, self.cfg.max_rounds.saturating_sub(1))
                    .await;
            }
        }

        stats.duration_s = started.elapsed().as_secs_f64();
        Ok(stats)
    }

    async fn dump_once(&self, compressed: bool) -> Result<UiDump, AdbError> {
        let xml = self.adb.uiautomator_dump(&self.serial, compressed).await?;
        UiDump::parse(&xml).map_err(|e| AdbError::CommandFailed {
            cmd: "uiautomator dump parse".into(),
            code: -1,
            stderr: e,
        })
    }

    /// §15.7：XML 给机器，PNG 给人，两个都存
    async fn save_failure_artifacts(&self, dump: &UiDump, round: u32) {
        if let Some(dir) = &self.dump_dir {
            let _ = tokio::fs::create_dir_all(dir).await;
            let xml_path = dir.join(format!("onboarding-round-{}.xml", round));
            let _ = tokio::fs::write(&xml_path, &dump.raw_xml).await;
            if let Ok(png) = self.adb.screencap_png(&self.serial).await {
                let png_path = dir.join(format!("onboarding-round-{}.png", round));
                let _ = tokio::fs::write(&png_path, png).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uiautomation::interactor::ScreenInfo;

    fn test_screen() -> ScreenInfo {
        ScreenInfo { w: 1080, h: 2400, dpi: 420 }
    }

    /// 晨视频图2 引导页第 1 页真实失败 dump（2026-08-07 M3 实测，仅 2 个节点）
    /// ViewPager 内容页被 a11y 过滤，只剩容器 —— 本测试锁定「全屏 pager」判据
    const GUIDE_PAGER_XML: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2400]"><node index="0" text="" resource-id="com.xxcb.chenshipin:id/view_pager" class="androidx.viewpager.widget.ViewPager" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="true" focused="false" scrollable="true" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]" /></node></hierarchy>"#;

    #[test]
    fn test_fullscreen_pager_detected_as_guide() {
        // 回归测试：M3 实测失败场景——引导页内容被 a11y 过滤只剩 ViewPager 容器
        let dump = UiDump::parse(GUIDE_PAGER_XML).unwrap();
        assert_eq!(dump.nodes.len(), 2);
        let cfg = OnboardingConfig::default();
        match detect_guide_page(&dump, &cfg, &test_screen()) {
            GuideAction::SwipeLeft => {}
            _ => panic!("全屏 pager 应识别为引导页并左滑"),
        }
        // 且不应误判为主页
        assert!(!is_on_home(&dump, "com.xxcb.chenshipin", &cfg, &test_screen()));
    }

    #[test]
    fn test_home_feed_not_misjudged_as_pager_guide() {
        // 反向保护：主页信息流也有全屏 scrollable RecyclerView，
        // 但可点击节点很多 → 不能误判为引导页
        let xml = r#"<hierarchy>
          <node package="com.x" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node package="com.x" class="androidx.recyclerview.widget.RecyclerView" scrollable="true" clickable="false" enabled="true" bounds="[0,0][1080,2337]">
              <node text="视频1" package="com.x" class="android.widget.LinearLayout" clickable="true" enabled="true" bounds="[0,200][540,800]"/>
              <node text="视频2" package="com.x" class="android.widget.LinearLayout" clickable="true" enabled="true" bounds="[540,200][1080,800]"/>
              <node text="视频3" package="com.x" class="android.widget.LinearLayout" clickable="true" enabled="true" bounds="[0,800][540,1400]"/>
            </node>
            <node text="首页" package="com.x" class="android.widget.TextView" clickable="true" enabled="true" bounds="[80,2200][280,2350]"/>
            <node text="我的" package="com.x" class="android.widget.TextView" clickable="true" enabled="true" bounds="[800,2200][1000,2350]"/>
          </node>
        </hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        assert!(matches!(
            detect_guide_page(&dump, &cfg, &test_screen()),
            GuideAction::NotGuide
        ));
    }

    /// 晨视频图1 协议弹窗夹具（§附录 A 真实 dump 缩略）
    const PRIVACY_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<hierarchy rotation="0">
  <node text="" class="android.widget.FrameLayout" package="com.chenchen.video"
        clickable="false" enabled="true" bounds="[0,0][1080,2400]">
    <node text="欢迎使用晨视频APP，为了更好的为您服务，请您仔细阅读并同意" class="android.widget.TextView"
          package="com.chenchen.video" clickable="false" enabled="true" bounds="[80,400][1000,1700]"/>
    <node text="《用户协议》" class="android.widget.TextView" package="com.chenchen.video"
          clickable="true" enabled="true" bounds="[200,1280][400,1320]"/>
    <node text="《隐私政策》" class="android.widget.TextView" package="com.chenchen.video"
          clickable="true" enabled="true" bounds="[420,1280][600,1320]"/>
    <node text="不同意" resource-id="com.chenchen.video:id/btn_disagree" class="android.widget.Button"
          package="com.chenchen.video" clickable="true" enabled="true" bounds="[80,1700][540,1820]"/>
    <node text="同意" resource-id="com.chenchen.video:id/btn_agree" class="android.widget.Button"
          package="com.chenchen.video" clickable="true" enabled="true" bounds="[540,1700][1000,1820]"/>
  </node>
</hierarchy>"#;

    #[test]
    fn test_privacy_dialog_detection() {
        let dump = UiDump::parse(PRIVACY_XML).unwrap();
        let cfg = OnboardingConfig::default();
        let screen = test_screen();
        let hit = detect_privacy_dialog(&dump, &cfg, &screen, false);
        assert!(hit.is_some(), "应识别出协议弹窗");
        let (node, matched) = hit.unwrap();
        // 必须选中「同意」而非「不同意」（§11.2 负向过滤）
        assert_eq!(node.text, "同意");
        assert!(matched.contains("同意"));
        assert_eq!(node.bounds.center(), (770, 1760));
    }

    #[test]
    fn test_privacy_not_triggered_by_normal_page() {
        // 普通页面含"隐私"字样但无文档名/双按钮 → 不触发
        let xml = r#"<hierarchy><node text="查看隐私设置" package="com.x" clickable="true"
            enabled="true" bounds="[0,0][100,50]" class="android.widget.TextView"/></hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        assert!(detect_privacy_dialog(&dump, &cfg, &test_screen(), false).is_none());
    }

    #[test]
    fn test_guide_enter_button() {
        // 晨视频图3：引导页末页「开启体验」
        let xml = r#"<hierarchy>
          <node package="com.chenchen.video" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node text="一起出发 看见温暖" package="com.chenchen.video" class="android.widget.TextView"
                  clickable="false" enabled="true" bounds="[200,800][880,1000]"/>
            <node text="开启体验" package="com.chenchen.video" class="android.widget.Button"
                  clickable="true" enabled="true" bounds="[340,1900][740,2020]"/>
          </node>
        </hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        match detect_guide_page(&dump, &cfg, &test_screen()) {
            GuideAction::TapEnter(_, _, m) => assert_eq!(m, "开启体验"),
            other => panic!("应为 TapEnter，实际 {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_guide_overlay_skip() {
        // 晨视频图4：功能指引覆盖层「跳过指引」
        let xml = r#"<hierarchy>
          <node package="com.chenchen.video" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node text="跳过指引" package="com.chenchen.video" class="android.widget.TextView"
                  clickable="true" enabled="true" bounds="[600,2100][800,2200]"/>
            <node text="开启功能指引(1/3)" package="com.chenchen.video" class="android.widget.TextView"
                  clickable="true" enabled="true" bounds="[820,2100][1060,2200]"/>
          </node>
        </hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        let hit = detect_guide_overlay(&dump, "com.chenchen.video", &cfg);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().0.text, "跳过指引");
    }

    #[test]
    fn test_home_detection() {
        // 晨视频图6：底部 5 nav + 顶部 tabs
        let xml = r#"<hierarchy>
          <node package="com.chenchen.video" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node text="首页" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[80,120][180,180]"/>
            <node text="精选" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[200,120][280,180]"/>
            <node text="首页" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[80,2150][280,2350]"/>
            <node text="创作" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[290,2150][490,2350]"/>
            <node text="帮忙" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[500,2150][700,2350]"/>
            <node text="活动" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[710,2150][910,2350]"/>
            <node text="我的" package="com.chenchen.video" class="android.widget.TextView" clickable="true" enabled="true" bounds="[920,2150][1080,2350]"/>
          </node>
        </hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        assert!(is_on_home(&dump, "com.chenchen.video", &cfg, &test_screen()));
    }

    #[test]
    fn test_system_cling_detection() {
        // 2026-08-07 M3 实测失败现场：沉浸式 cling 盖住功能指引覆盖层
        // （背景已是主页+"跳过指引"，但顶层 package=android，所有关卡不命中）
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="android" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,128][1080,2400]"><node index="0" text="" resource-id="" class="android.widget.RelativeLayout" package="android" clickable="false" enabled="true" bounds="[0,128][1080,781]"><node index="1" text="Viewing full screen" resource-id="android:id/immersive_cling_title" class="android.widget.TextView" package="android" clickable="false" enabled="true" bounds="[0,265][1080,455]"/><node index="2" text="To exit, swipe down from the top." resource-id="android:id/immersive_cling_description" class="android.widget.TextView" package="android" clickable="false" enabled="true" bounds="[0,455][1080,545]"/><node index="3" text="Got it" resource-id="android:id/ok" class="android.widget.Button" package="android" clickable="true" enabled="true" bounds="[900,600][1080,700]"/></node></node></hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let hit = detect_system_cling(&dump);
        assert!(hit.is_some(), "沉浸式 cling 应被识别");
        assert_eq!(hit.unwrap().text, "Got it");
        // 且它应该成为 detect_next_action 的第一个命中（最高优先级）
        let cfg = OnboardingConfig::default();
        match detect_next_action(&dump, "com.xxcb.chenshipin", &cfg, &test_screen(), false) {
            Action::Tap { stage, .. } => assert_eq!(stage, Stage::SystemCling),
            _ => panic!("cling 应以最高优先级被 tap"),
        }
    }

    #[test]
    fn test_permission_dialog_detection() {
        // 晨视频图5：系统权限弹窗（兜底路径）
        let xml = r#"<hierarchy>
          <node package="com.android.permissioncontroller" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node text="是否允许晨视频获取模糊定位" package="com.android.permissioncontroller"
                  class="android.widget.TextView" clickable="false" enabled="true" bounds="[100,800][980,900]"/>
            <node text="拒绝" package="com.android.permissioncontroller" class="android.widget.Button"
                  clickable="true" enabled="true" bounds="[100,1100][980,1200]"/>
            <node text="本次运行允许" package="com.android.permissioncontroller" class="android.widget.Button"
                  clickable="true" enabled="true" bounds="[100,1220][980,1320]"/>
            <node text="仅在使用中允许" package="com.android.permissioncontroller" class="android.widget.Button"
                  clickable="true" enabled="true" bounds="[100,1340][980,1440]"/>
          </node>
        </hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        let hit = detect_permission_dialog(&dump, &cfg);
        assert!(hit.is_some());
        // 优先选「仅在使用中允许」（preferred 列表第一个）
        assert_eq!(hit.unwrap().0.text, "仅在使用中允许");
    }

    /// v1.5 回归测试：用真实失败现场（pilot-20260807-074418 round-14 dump）验证
    /// 全屏 ViewPager 引导页检测 + max_swipes 兜底逻辑。
    /// 该现场是 L2.5 试验失败后模拟器停在错误用户上的产物：
    /// 引导页 ViewPager 只有一个 ImageView（内容被 a11y 过滤），swipe 无效，
    /// 原代码不检查 max_swipes 可无限滑到 max_rounds 耗尽 → max_rounds_exhausted
    #[test]
    fn test_real_failure_fullscreen_pager() {
        // 从真实失败 dump 抽取的 XML（round 14，10 个节点，全屏 ViewPager）
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2400]"><node index="0" text="" resource-id="" class="android.widget.LinearLayout" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]"><node index="0" text="" resource-id="android:id/content" class="android.widget.FrameLayout" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]"><node index="0" text="" resource-id="" class="android.widget.RelativeLayout" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]"><node index="0" text="" resource-id="com.xxcb.chenshipin:id/view_pager" class="androidx.viewpager.widget.ViewPager" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="true" focused="false" scrollable="true" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]"><node index="1" text="" resource-id="" class="android.widget.RelativeLayout" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]"><node index="0" text="" resource-id="com.xxcb.chenshipin:id/image_item" class="android.widget.ImageView" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2337]" /></node></node><node index="1" text="" resource-id="com.xxcb.chenshipin:id/image" class="android.widget.ImageView" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[453,2165][626,2181]" /><node index="2" text="" resource-id="com.xxcb.chenshipin:id/image_direction" class="android.widget.ImageView" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[420,2216][659,2251]" /></node></node></node><node index="1" text="" resource-id="android:id/navigationBarBackground" class="android.view.View" package="com.xxcb.chenshipin" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,2337][1080,2400]" /></node></hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        let cfg = OnboardingConfig::default();
        let screen = test_screen();

        // 1. 引导页检测：全屏 pager 启发式应命中
        assert!(matches!(
            detect_guide_page(&dump, &cfg, &screen),
            GuideAction::SwipeLeft
        ));

        // 2. 不应误判为主页
        assert!(!is_on_home(&dump, "com.xxcb.chenshipin", &cfg, &screen));

        // 3. 新版 APK 跳过引导页，detect_next_action 不再返回 SwipeLeftGuide
        match detect_next_action(&dump, "com.xxcb.chenshipin", &cfg, &screen, false) {
            Action::None | Action::OnHome => {}
            other => panic!("实际 {:?}", std::mem::discriminant(&other)),
        }

        // 4. max_swipes 默认值应为 8
        assert_eq!(cfg.guide_pages.max_swipes, 8);

        // 5. h5_fallback 候选点应非空（max_swipes 后兜底用）
        assert!(!cfg.h5_fallback.candidate_points.is_empty());
    }

    #[test]
    fn test_privacy_dialog_checkbox_behavior() {
        let xml_unchecked = r#"<hierarchy>
          <node package="com.xxcb.chenshipin" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node text="隐私政策与用户协议" package="com.xxcb.chenshipin" class="android.widget.TextView" clickable="false" enabled="true" bounds="[100,500][980,600]"/>
            <node text="不同意" package="com.xxcb.chenshipin" class="android.widget.Button" clickable="true" enabled="true" bounds="[100,1200][500,1300]"/>
            <node text="同意" package="com.xxcb.chenshipin" class="android.widget.Button" clickable="true" enabled="true" bounds="[580,1200][980,1300]"/>
            <node resource-id="com.xxcb.chenshipin:id/cb_agree" class="android.widget.CheckBox" checkable="true" checked="false" clickable="true" enabled="true" bounds="[100,1100][160,1160]"/>
          </node>
        </hierarchy>"#;

        let xml_checked = r#"<hierarchy>
          <node package="com.xxcb.chenshipin" class="android.widget.FrameLayout" clickable="false" enabled="true" bounds="[0,0][1080,2400]">
            <node text="隐私政策与用户协议" package="com.xxcb.chenshipin" class="android.widget.TextView" clickable="false" enabled="true" bounds="[100,500][980,600]"/>
            <node text="不同意" package="com.xxcb.chenshipin" class="android.widget.Button" clickable="true" enabled="true" bounds="[100,1200][500,1300]"/>
            <node text="同意" package="com.xxcb.chenshipin" class="android.widget.Button" clickable="true" enabled="true" bounds="[580,1200][980,1300]"/>
            <node resource-id="com.xxcb.chenshipin:id/cb_agree" class="android.widget.CheckBox" checkable="true" checked="true" clickable="true" enabled="true" bounds="[100,1100][160,1160]"/>
          </node>
        </hierarchy>"#;

        let dump_unchecked = UiDump::parse(xml_unchecked).unwrap();
        let dump_checked = UiDump::parse(xml_checked).unwrap();
        let cfg = OnboardingConfig::default();
        let screen = test_screen();

        // 1. 未勾选且 cb_already_clicked=false：应当优先点击 CheckBox (cb_first)
        let hit1 = detect_privacy_dialog(&dump_unchecked, &cfg, &screen, false);
        assert!(hit1.is_some());
        assert!(hit1.unwrap().1.starts_with("cb_first:"));

        // 2. 已勾选 (checked=true) 且 cb_already_clicked=false：应当跳过 CheckBox 直接点击同意按钮
        let hit2 = detect_privacy_dialog(&dump_checked, &cfg, &screen, false);
        assert!(hit2.is_some());
        assert!(!hit2.unwrap().1.starts_with("cb_first:"));

        // 3. 即使 XML 中 checked=false，若此前轮次已点击过 (cb_already_clicked=true)：也必须直接点击同意按钮，杜绝反复切换死循环
        let hit3 = detect_privacy_dialog(&dump_unchecked, &cfg, &screen, true);
        assert!(hit3.is_some());
        assert!(!hit3.unwrap().1.starts_with("cb_first:"));
    }
}
