//! UiDump：uiautomator dump XML 解析与查询（设计文档 §12.3 参考实现）
//! 关键修正（§11）：
//! - 带 parent 索引（§11.3：向上找可点击祖先，应对 Compose/ConstraintLayout）
//! - exec-out 读出的字节再做 \r\n 清洗（双保险，§11.7）
//! - 统一 owned 类型（§11.11：UiNode<'a> 与 UiNodeOwned 并存矛盾）

use roxmltree::Document;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bounds {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Bounds {
    /// 解析 "[x1,y1][x2,y2]"，失败返回全零（调用方按面积 0 自然忽略）
    pub fn parse(s: &str) -> Self {
        let nums: Vec<i32> = s
            .split(|c: char| !c.is_ascii_digit() && c != '-')
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() >= 4 {
            Bounds {
                x1: nums[0],
                y1: nums[1],
                x2: nums[2],
                y2: nums[3],
            }
        } else {
            Bounds::default()
        }
    }

    pub fn center(&self) -> (i32, i32) {
        ((self.x1 + self.x2) / 2, (self.y1 + self.y2) / 2)
    }
    pub fn width(&self) -> i32 {
        self.x2 - self.x1
    }
    pub fn height(&self) -> i32 {
        self.y2 - self.y1
    }
    pub fn area(&self) -> i64 {
        (self.width() as i64).max(0) * (self.height() as i64).max(0)
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x1 && x < self.x2 && y >= self.y1 && y < self.y2
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiNodeOwned {
    pub text: String,
    pub resource_id: String,
    pub class: String,
    pub package: String,
    pub content_desc: String,
    pub clickable: bool,
    pub scrollable: bool,
    pub enabled: bool,
    pub bounds: Bounds,
    pub depth: u32,
    /// §11.3：nodes 数组下标，用于向上找可点击祖先
    pub parent: Option<usize>,
}

impl UiNodeOwned {
    /// class 名是否为按钮类（§11.11 补定义）
    pub fn is_button_class(&self) -> bool {
        let c = self.class.as_str();
        c.ends_with("Button")
            || c.ends_with("TextView")
            || c.ends_with("ImageView")
            || c.ends_with("CheckBox")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiDump {
    /// 顶层 package（取面积最大的非空 package 节点，根 FrameLayout 常为空）
    pub top_package: String,
    pub nodes: Vec<UiNodeOwned>,
    /// 原始 XML（失败落盘用，§15.7）
    pub raw_xml: String,
}

impl UiDump {
    pub fn parse(xml: &str) -> Result<Self, String> {
        // §11.7 双保险：exec-out 理论上无 \r，但清洗成本极低
        let cleaned = xml.replace("\r\n", "\n");
        let doc = Document::parse(&cleaned).map_err(|e| e.to_string())?;

        let mut nodes: Vec<UiNodeOwned> = Vec::new();
        // 显式栈维护 (roxmltree NodeId, 在 nodes 中的下标)
        let mut stack: Vec<(roxmltree::NodeId, usize)> = Vec::new();

        for n in doc.descendants().filter(|n| n.has_tag_name("node")) {
            // 回退栈到当前节点的父级
            while let Some(&(nid, _)) = stack.last() {
                let is_ancestor = n.ancestors().any(|a| a.id() == nid);
                if is_ancestor {
                    break;
                }
                stack.pop();
            }
            let parent = stack.last().map(|&(_, idx)| idx);
            let depth = stack.len() as u32;

            let g = |k: &str| n.attribute(k).unwrap_or("").to_string();
            let b = |k: &str| n.attribute(k) == Some("true");

            nodes.push(UiNodeOwned {
                text: g("text"),
                resource_id: g("resource-id"),
                class: g("class"),
                package: g("package"),
                content_desc: g("content-desc"),
                clickable: b("clickable"),
                scrollable: b("scrollable"),
                enabled: n.attribute("enabled") != Some("false"),
                bounds: Bounds::parse(n.attribute("bounds").unwrap_or("")),
                depth,
                parent,
            });
            stack.push((n.id(), nodes.len() - 1));
        }

        let top_package = nodes
            .iter()
            .filter(|n| !n.package.is_empty())
            .max_by_key(|n| n.bounds.area())
            .map(|n| n.package.clone())
            .unwrap_or_default();

        Ok(UiDump {
            top_package,
            nodes,
            raw_xml: cleaned,
        })
    }

    /// §11.3：向上找最近可点击祖先；找不到返回自身
    pub fn resolve_tap_target<'a>(&'a self, node: &'a UiNodeOwned) -> &'a UiNodeOwned {
        if node.clickable {
            return node;
        }
        let mut cur = node.parent;
        while let Some(i) = cur {
            if self.nodes[i].clickable {
                return &self.nodes[i];
            }
            cur = self.nodes[i].parent;
        }
        node
    }

    /// 按 text 包含匹配（大小写敏感；调用方自行 trim）
    pub fn find_by_text(&self, pat: &str) -> Vec<&UiNodeOwned> {
        self.nodes.iter().filter(|n| n.text.contains(pat)).collect()
    }

    /// 精确文本匹配（trim 后全等）
    pub fn find_by_text_exact(&self, pat: &str) -> Vec<&UiNodeOwned> {
        self.nodes
            .iter()
            .filter(|n| n.text.trim() == pat)
            .collect()
    }

    /// resource-id 后缀匹配（"btn_agree" 匹配 "com.xxx.app:id/btn_agree"）
    pub fn find_by_id_suffix(&self, sfx: &str) -> Vec<&UiNodeOwned> {
        self.nodes
            .iter()
            .filter(|n| n.resource_id.rsplit('/').next() == Some(sfx))
            .collect()
    }

    pub fn clickable_nodes(&self) -> Vec<&UiNodeOwned> {
        self.nodes.iter().filter(|n| n.clickable).collect()
    }

    pub fn contains_package(&self, pkg: &str) -> bool {
        self.nodes.iter().any(|n| n.package == pkg)
    }

    /// 是否存在底部导航（启发式：可点击节点 ≥3 个且中心 y 都在屏幕下方 1/4）
    pub fn has_bottom_nav(&self, screen_h: i32) -> bool {
        let zone_y = screen_h * 3 / 4;
        let count = self
            .nodes
            .iter()
            .filter(|n| n.clickable)
            .filter(|n| n.bounds.center().1 > zone_y)
            .filter(|n| n.bounds.height() > 0 && n.bounds.height() < screen_h / 6)
            .count();
        count >= 3
    }

    /// 节点总数（loading 页节点极少，反面信号用）
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bounds_parse() {
        let b = Bounds::parse("[80,1700][540,1820]");
        assert_eq!(b.x1, 80);
        assert_eq!(b.y2, 1820);
        assert_eq!(b.center(), (310, 1760));
        assert!(b.contains(100, 1750));
        assert!(!b.contains(0, 0));
    }

    #[test]
    fn test_parse_parent_index() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<hierarchy rotation="0">
  <node text="" resource-id="" class="android.widget.FrameLayout" package="com.xxx"
        clickable="true" enabled="true" bounds="[0,0][1080,2400]">
    <node text="同意" resource-id="com.xxx:id/tv_agree" class="android.widget.TextView"
          package="com.xxx" clickable="false" enabled="true" bounds="[540,1700][1000,1820]"/>
  </node>
</hierarchy>"#;
        let dump = UiDump::parse(xml).unwrap();
        assert_eq!(dump.nodes.len(), 2);
        // text 节点 clickable=false，其父 clickable=true
        let text_node = dump.find_by_text("同意")[0];
        assert!(!text_node.clickable);
        let target = dump.resolve_tap_target(text_node);
        assert!(target.clickable);
        assert_eq!(target.bounds.center(), (540, 1200));
        assert_eq!(dump.top_package, "com.xxx");
    }

    #[test]
    fn test_crlf_cleaning() {
        let xml = "<hierarchy>\r\n  <node text=\"a\" package=\"p\" bounds=\"[0,0][1,1]\"/>\r\n</hierarchy>";
        let dump = UiDump::parse(xml).unwrap();
        assert_eq!(dump.nodes.len(), 1);
        assert_eq!(dump.nodes[0].text, "a");
    }
}
