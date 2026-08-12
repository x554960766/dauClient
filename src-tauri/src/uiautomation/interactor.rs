//! Interactor：input 命令封装（v1.2：无抖动，确定性剧本）
//! 设计文档 §3.2 修正版：删除 jitter/rng（CoverageWalker 用元素中心，不抖动）

use crate::adb::{Adb, AdbError};
use crate::uiautomation::dump::UiNodeOwned;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct ScreenInfo {
    pub w: i32,
    pub h: i32,
    pub dpi: i32,
}

impl ScreenInfo {
    /// 像素转 dp（§11.12：阈值判定统一走 dp，不用写死像素）
    pub fn to_dp(&self, px: i32) -> i32 {
        px * 160 / self.dpi.max(1)
    }
    pub fn ratio_x(&self, r: f32) -> i32 {
        (self.w as f32 * r) as i32
    }
    pub fn ratio_y(&self, r: f32) -> i32 {
        (self.h as f32 * r) as i32
    }
}

#[derive(Debug, Clone)]
pub struct Interactor {
    adb: Adb,
    serial: String,
    pub screen: ScreenInfo,
}

impl Interactor {
    pub fn new(adb: Adb, serial: String, screen: ScreenInfo) -> Self {
        Self { adb, serial, screen }
    }

    pub async fn fetch_screen(adb: &Adb, serial: &str) -> Result<ScreenInfo, AdbError> {
        let (w, h) = adb.screen_size(serial).await?;
        let dpi = adb.density_dpi(serial).await?;
        Ok(ScreenInfo { w, h, dpi })
    }

    /// 点击节点中心（无抖动，§5 重写）
    pub async fn tap_node(&self, node: &UiNodeOwned) -> Result<(), AdbError> {
        let (cx, cy) = node.bounds.center();
        self.tap(cx, cy).await
    }

    pub async fn tap(&self, x: i32, y: i32) -> Result<(), AdbError> {
        self.adb.input_tap(&self.serial, x, y).await
    }

    /// 屏幕比例坐标点击（H5 协议页兜底 / CoverageWalker Ratio 选择器用）
    pub async fn tap_ratio(&self, rx: f32, ry: f32) -> Result<(), AdbError> {
        self.tap(self.screen.ratio_x(rx), self.screen.ratio_y(ry)).await
    }

    pub async fn swipe(
        &self,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        duration_ms: u32,
    ) -> Result<(), AdbError> {
        self.adb
            .input_swipe(&self.serial, x1, y1, x2, y2, duration_ms)
            .await
    }

    /// 左滑（引导页翻页：优化滑动轨迹与时长，避开全屏卡片中心图层）
    pub async fn swipe_left(&self) -> Result<(), AdbError> {
        let (w, h) = (self.screen.w, self.screen.h);
        self.swipe(w * 85 / 100, h * 58 / 100, w * 15 / 100, h * 58 / 100, 350).await
    }

    /// 上滑（列表浏览）
    pub async fn swipe_up(&self) -> Result<(), AdbError> {
        let (w, h) = (self.screen.w, self.screen.h);
        self.swipe(w / 2, h * 4 / 5, w / 2, h / 5, 350).await
    }

    pub async fn back(&self) -> Result<(), AdbError> {
        self.adb.keyevent(&self.serial, "KEYCODE_BACK").await
    }

    pub async fn home(&self) -> Result<(), AdbError> {
        self.adb.keyevent(&self.serial, "KEYCODE_HOME").await
    }

    /// 固定停留（确定性剧本）
    pub async fn dwell_ms(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}
