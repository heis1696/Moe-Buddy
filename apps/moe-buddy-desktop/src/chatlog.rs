//! 聊天历史的纯布局计算：按注入的换行测量结果排布消息块并管理滚动。
//!
//! 测量函数由宿主注入（Win32 `DrawTextW` 的 `DT_CALCRECT`），本模块
//! 只做确定性数学，可以在无窗口环境回归。

use moe_chat_app::{ChatMessage, MessageRole};

/// 单条消息的布局结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockLayout {
    pub role: MessageRole,
    /// 相对历史内容顶部的偏移。
    pub top: i32,
    /// 气泡整体高度（含角色标签与内边距）。
    pub height: i32,
}

/// 一次完整历史布局。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryLayout {
    pub blocks: Vec<BlockLayout>,
    pub content_height: i32,
}

/// 布局度量（宿主由主题按 DPI 缩放后注入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// 正文一行的高度。
    pub line_height: i32,
    /// 角色标签行高度。
    pub label_height: i32,
    /// 气泡上下内边距。
    pub pad_y: i32,
    /// 相邻消息间距。
    pub gap: i32,
}

/// 依据注入的“给定文本折成几行”测量器排布全部消息。
///
/// 空文本按一行计；返回的内容高度不含末尾间距。
pub fn layout_history(
    messages: &[ChatMessage],
    metrics: Metrics,
    mut measure_lines: impl FnMut(&str) -> i32,
) -> HistoryLayout {
    let mut top = 0;
    let mut blocks = Vec::with_capacity(messages.len());
    for message in messages {
        let lines = measure_lines(&message.text).max(1);
        let height = metrics.label_height + lines * metrics.line_height + 2 * metrics.pad_y;
        blocks.push(BlockLayout {
            role: message.role,
            top,
            height,
        });
        top += height + metrics.gap;
    }
    let content_height = match blocks.last() {
        Some(last) => last.top + last.height,
        None => 0,
    };
    HistoryLayout {
        blocks,
        content_height,
    }
}

/// 把滚动位置夹在 `[0, content_height - viewport]`。
pub fn clamp_scroll(scroll: i32, content_height: i32, viewport: i32) -> i32 {
    let max = (content_height - viewport).max(0);
    scroll.clamp(0, max)
}

/// 用户是否已停留在底部（用于新消息自动跟随）。
pub fn at_bottom(scroll: i32, content_height: i32, viewport: i32) -> bool {
    clamp_scroll(scroll, content_height, viewport) >= (content_height - viewport).max(0)
}

/// 一次布局变化后的滚动位置：原本在底部则继续贴底，否则保持阅读位置。
pub fn scroll_after_content_change(
    previous_scroll: i32,
    previous_content: i32,
    new_content: i32,
    previous_viewport: i32,
    viewport: i32,
) -> i32 {
    if at_bottom(previous_scroll, previous_content, previous_viewport) {
        clamp_scroll(i32::MAX, new_content, viewport)
    } else {
        clamp_scroll(previous_scroll, new_content, viewport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics {
            line_height: 24,
            label_height: 18,
            pad_y: 8,
            gap: 8,
        }
    }

    fn message(role: MessageRole, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            text: text.to_owned(),
        }
    }

    #[test]
    fn empty_history_has_zero_content() {
        let layout = layout_history(&[], metrics(), |_| 3);
        assert!(layout.blocks.is_empty());
        assert_eq!(layout.content_height, 0);
    }

    #[test]
    fn block_offsets_accumulate_and_measure_lines_drive_height() {
        let messages = [
            message(MessageRole::User, "short"),
            message(MessageRole::Assistant, "needs five lines"),
        ];
        let layout = layout_history(
            &messages,
            metrics(),
            |text| {
                if text.len() > 10 { 5 } else { 1 }
            },
        );
        assert_eq!(layout.blocks.len(), 2);
        let first = layout.blocks[0];
        assert_eq!(first.height, 18 + 24 + 16);
        assert_eq!(first.top, 0);
        let second = layout.blocks[1];
        assert_eq!(second.height, 18 + 5 * 24 + 16);
        assert_eq!(second.top, first.height + 8);
        assert_eq!(layout.content_height, second.top + second.height);
    }

    #[test]
    fn measurer_returning_zero_is_clamped_to_one_line() {
        let messages = [message(MessageRole::User, "hello")];
        let layout = layout_history(&messages, metrics(), |_| 0);
        assert_eq!(layout.blocks[0].height, 18 + 24 + 16);
    }

    #[test]
    fn clamp_handles_negative_and_overflow() {
        assert_eq!(clamp_scroll(-10, 300, 200), 0);
        assert_eq!(clamp_scroll(500, 300, 200), 100);
        assert_eq!(clamp_scroll(50, 300, 200), 50);
        assert_eq!(clamp_scroll(50, 100, 200), 0);
    }

    #[test]
    fn at_bottom_tolerates_minor_overflow() {
        assert!(at_bottom(100, 300, 200));
        assert!(!at_bottom(99, 300, 200));
        assert!(at_bottom(0, 100, 200));
    }

    #[test]
    fn content_change_sticks_to_bottom_only_when_already_there() {
        let stuck = scroll_after_content_change(100, 300, 900, 200, 200);
        assert_eq!(stuck, 700);
        let reading = scroll_after_content_change(40, 300, 900, 200, 200);
        assert_eq!(reading, 40);
        let shrink = scroll_after_content_change(40, 300, 120, 200, 200);
        assert_eq!(shrink, 0);
    }

    #[test]
    fn resizing_viewport_preserves_bottom_or_reading_position() {
        assert_eq!(scroll_after_content_change(600, 1000, 1000, 400, 300), 700);
        assert_eq!(scroll_after_content_change(200, 1000, 1000, 400, 300), 200);
        assert_eq!(scroll_after_content_change(600, 1000, 1000, 400, 500), 500);
    }
}
