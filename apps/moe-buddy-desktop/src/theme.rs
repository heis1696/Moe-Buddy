//! 桌面表现主题：纸白画布、苔绿强调与 8px 间距基准。
//!
//! 纯数据模块，不包含任何 Win32 调用；宿主按窗口 DPI 缩放后使用。

/// 纸白画布 `#F7F6F2`。
pub const BG: u32 = rgb(0xF7, 0xF6, 0xF2);
/// 灰绿导航底色。
pub const NAV: u32 = rgb(0xEC, 0xEF, 0xE8);
/// 白面板 `#FFFFFF`。
pub const PANEL: u32 = rgb(0xFF, 0xFF, 0xFF);
/// 墨灰正文。
pub const INK: u32 = rgb(0x26, 0x2F, 0x2B);
/// 辅助正文；在白色和纸白背景上均保持可读对比度。
pub const MUTED: u32 = rgb(0x68, 0x73, 0x6C);
/// 柔和边界。
pub const BORDER: u32 = rgb(0xDC, 0xE2, 0xDA);
/// 苔绿强调。
pub const ACCENT: u32 = rgb(0x38, 0x66, 0x53);
/// 苔绿按压态。
pub const ACCENT_DARK: u32 = rgb(0x29, 0x4E, 0x3F);
/// 浅绿选中背景。
pub const ACCENT_SOFT: u32 = rgb(0xE4, 0xED, 0xE3);
/// 失败/危险文字；形状与文字标签同时给出，不单独依赖颜色。
pub const DANGER: u32 = rgb(0xA8, 0x40, 0x2F);
/// 需要留意的警告文字（结果未知、历史缺口等）。
pub const WARN: u32 = rgb(0x8A, 0x6A, 0x16);
/// 警告浅底。
pub const WARN_SOFT: u32 = rgb(0xFA, 0xF3, 0xE1);
/// 禁用控件填充与文字。
pub const DISABLED_FILL: u32 = rgb(0xED, 0xEF, 0xE9);
pub const DISABLED_TEXT: u32 = rgb(0x8B, 0x95, 0x8C);

/// 中文清晰界面字体。
pub const FONT_UI: &str = "Microsoft YaHei UI";
/// 日志条目使用的系统等宽字体。
pub const FONT_MONO: &str = "Consolas";

/// `COLORREF` 布局（0x00BBGGRR）。
pub const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | (green << 8) | (blue << 16)
}

/// 按窗口 DPI 把 96 DPI 基准值缩放为设备像素。
pub fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// 界面度量：96 DPI 基准的像素值，8px 间距体系；宿主按 DPI 缩放。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// 左侧导航宽度。
    pub nav_w: i32,
    /// 完整输入卡高度（编辑区、提示与发送）。
    pub composer_h: i32,
    /// 窗口外边距与栏间距基数。
    pub margin: i32,
    /// 控件间距基数。
    pub gap: i32,
    /// 顶部伙伴信息栏高度。
    pub header_h: i32,
    /// 底部状态栏高度。
    pub status_h: i32,
    /// 通知条高度（仅有通知时出现）。
    pub notice_h: i32,
    /// 聊天输入框高度。
    pub input_h: i32,
    /// 发送按钮宽度。
    pub send_w: i32,
    /// 右侧边栏宽度。
    pub sidebar_w: i32,
    /// 边栏页签高度。
    pub tab_h: i32,
    /// 边栏面板小标题高度。
    pub panel_title_h: i32,
    /// 边栏详情文本区高度。
    pub detail_h: i32,
    /// 边栏按钮行高度。
    pub action_h: i32,
    /// 边栏记忆输入行高度。
    pub panel_input_h: i32,
    /// 面板圆角（18px 基准）。
    pub radius_panel: i32,
    /// 控件圆角（10px 基准）。
    pub radius_ctrl: i32,
    /// 标题字号。
    pub font_title: i32,
    /// 正文字号。
    pub font_body: i32,
    /// 次要文字字号。
    pub font_small: i32,
    /// 日志等宽字号。
    pub font_mono: i32,
    /// 聊天气泡行高。
    pub bubble_line: i32,
    /// 最小客户区宽度。
    pub min_client_w: i32,
    /// 最小客户区高度。
    pub min_client_h: i32,
    /// 初始客户区宽度。
    pub initial_client_w: i32,
    /// 初始客户区高度。
    pub initial_client_h: i32,
}

impl Metrics {
    pub fn new(dpi: u32) -> Self {
        let s = |value: i32| scale(value, dpi);
        Self {
            nav_w: s(88),
            composer_h: s(128),
            margin: s(24),
            gap: s(8),
            header_h: s(80),
            status_h: s(56),
            notice_h: s(44),
            input_h: s(60),
            send_w: s(88),
            sidebar_w: s(344),
            tab_h: s(64),
            panel_title_h: s(28),
            detail_h: s(64),
            action_h: s(32),
            panel_input_h: s(72),
            radius_panel: s(18),
            radius_ctrl: s(10),
            // Font sizes stay at 96 DPI logical pixels; the native host scales
            // them once when creating the GDI fonts.
            font_title: 20,
            font_body: 16,
            font_small: 13,
            font_mono: 13,
            bubble_line: s(24),
            min_client_w: s(880),
            min_client_h: s(640),
            initial_client_w: s(1120),
            initial_client_h: s(760),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_scale_with_dpi_and_keep_baseline_at_96() {
        let base = Metrics::new(96);
        assert_eq!(base.margin, 24);
        assert_eq!(base.gap, 8);
        assert_eq!(base.font_body, 16);
        let one_and_half = Metrics::new(144);
        assert_eq!(one_and_half.margin, 36);
        assert_eq!(one_and_half.font_body, 16);
        assert_eq!(one_and_half.min_client_w, base.min_client_w * 3 / 2);
    }

    #[test]
    fn scale_rounds_down_and_survives_zero() {
        assert_eq!(scale(10, 96), 10);
        assert_eq!(scale(10, 150), 15);
        assert_eq!(scale(9, 150), 14);
    }
}
