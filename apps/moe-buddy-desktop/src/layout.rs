//! 客户区布局：把窗口尺寸与主题度量换算为各区域矩形。
//!
//! 纯几何计算，不包含 Win32；缩放/最小尺寸的行为在此集中回归。

use crate::theme::Metrics;

/// 平面矩形（设备像素）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn zero() -> Self {
        Self::default()
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// 主窗口一次布局的全部区域。
#[derive(Clone, Copy, Debug)]
pub struct ClientLayout {
    pub navigation: Rect,
    pub chat_tab: Rect,
    pub composer: Rect,
    pub header: Rect,
    pub notice: Option<Rect>,
    pub canvas: Rect,
    pub input: Rect,
    pub send: Rect,
    pub hint: Rect,
    pub status: Rect,
    pub status_text: Rect,
    pub revoke: Rect,
    pub sidebar: Rect,
    pub tabs: [Rect; 3],
    pub panel: Rect,
}

/// 边栏面板内部布局。
#[derive(Clone, Debug)]
pub struct PanelLayout {
    pub title: Rect,
    pub search: Rect,
    pub list: Rect,
    pub detail: Rect,
    pub input: Rect,
    pub actions: Vec<Rect>,
}

/// 计算主客户区布局；`show_notice` 决定是否保留通知条。
#[cfg(test)]
pub fn compute(client_w: i32, client_h: i32, show_notice: bool, m: &Metrics) -> ClientLayout {
    compute_with_drawer(client_w, client_h, show_notice, true, m)
}

/// A hidden management drawer gives its space back to the conversation.
pub fn compute_with_drawer(
    client_w: i32,
    client_h: i32,
    show_notice: bool,
    drawer: bool,
    m: &Metrics,
) -> ClientLayout {
    let width = client_w.max(m.min_client_w);
    let height = client_h.max(m.min_client_h);

    let navigation = Rect {
        x: 0,
        y: 0,
        w: m.nav_w,
        h: height,
    };
    let main_right = if drawer { width - m.sidebar_w } else { width };
    let header = Rect {
        x: m.nav_w,
        y: 0,
        w: main_right - m.nav_w,
        h: m.header_h,
    };
    let status = Rect {
        x: m.nav_w,
        y: height - m.status_h,
        w: width - m.nav_w,
        h: m.status_h,
    };
    let notice = if show_notice {
        Some(Rect {
            x: m.nav_w,
            y: status.y - m.notice_h,
            w: width - m.nav_w,
            h: m.notice_h,
        })
    } else {
        None
    };
    let middle_bottom = notice.map_or(status.y, |rect| rect.y) - m.gap * 2;
    let middle_top = header.bottom() + m.gap;

    let sidebar = Rect {
        x: width - m.sidebar_w,
        y: 0,
        w: m.sidebar_w,
        h: notice.map_or(status.y, |rect| rect.y),
    };
    let available = main_right - m.nav_w - 2 * m.margin;
    let chat_w = available.min(m.gap * 96);
    let chat_x = m.nav_w + m.margin + (available - chat_w) / 2;
    let composer = Rect {
        x: chat_x,
        y: middle_bottom - m.composer_h,
        w: chat_w,
        h: m.composer_h,
    };
    let inset = m.gap * 2;
    let send = Rect {
        x: composer.right() - inset - m.send_w,
        y: composer.bottom() - inset - m.action_h,
        w: m.send_w,
        h: m.action_h,
    };
    let hint = Rect {
        x: composer.x + inset,
        y: send.y,
        w: send.x - composer.x - inset - m.gap,
        h: send.h,
    };
    let input = Rect {
        x: composer.x + inset,
        y: composer.y + inset,
        w: composer.w - inset * 2,
        h: m.input_h,
    };
    let canvas = Rect {
        x: chat_x,
        y: middle_top,
        w: chat_w,
        h: composer.y - m.gap * 2 - middle_top,
    };

    let chat_tab = Rect {
        x: m.gap,
        y: m.header_h + m.margin,
        w: m.nav_w - 2 * m.gap,
        h: m.tab_h,
    };
    let mut tabs = [Rect::zero(); 3];
    for (index, tab) in tabs.iter_mut().enumerate() {
        *tab = Rect {
            x: chat_tab.x,
            y: chat_tab.y + (index as i32 + 1) * (m.tab_h + m.gap),
            w: chat_tab.w,
            h: m.tab_h,
        };
    }
    let panel = Rect {
        x: sidebar.x + m.margin,
        y: sidebar.y + m.margin,
        w: sidebar.w - m.margin * 2,
        h: sidebar.h - m.margin * 2,
    };

    let revoke_w = m.send_w;
    let revoke = Rect {
        x: width - m.margin - revoke_w,
        y: status.y + (m.status_h - m.action_h) / 2,
        w: revoke_w,
        h: m.action_h,
    };
    let status_text = Rect {
        x: m.nav_w + m.margin,
        y: status.y,
        w: revoke.x - m.gap - m.nav_w - m.margin,
        h: status.h,
    };

    ClientLayout {
        navigation,
        chat_tab,
        composer,
        header,
        notice,
        canvas,
        input,
        send,
        hint,
        status,
        status_text,
        revoke,
        sidebar,
        tabs,
        panel,
    }
}

/// 边栏面板内部布局：标题、弹性列表、可选输入行、可选按钮行与底部详情。
pub fn panel_children(panel: Rect, m: &Metrics, actions: usize, with_input: bool) -> PanelLayout {
    let title = Rect {
        x: panel.x,
        y: panel.y,
        w: panel.w,
        h: m.panel_title_h,
    };
    let detail = Rect {
        x: panel.x,
        y: panel.bottom() - m.detail_h,
        w: panel.w,
        h: m.detail_h,
    };
    let action_rows = if actions > 2 { 2 } else { 1 };
    let actions_y = detail.y - m.gap - action_rows * m.action_h - (action_rows - 1) * m.gap;
    let actions: Vec<Rect> = match actions {
        0 => Vec::new(),
        1 => vec![Rect {
            x: panel.x,
            y: actions_y,
            w: panel.w,
            h: m.action_h,
        }],
        count => {
            let half = (panel.w - m.gap) / 2;
            (0..count)
                .map(|i| Rect {
                    x: panel.x + (i % 2) as i32 * (half + m.gap),
                    y: actions_y + (i / 2) as i32 * (m.action_h + m.gap),
                    w: half,
                    h: m.action_h,
                })
                .collect()
        }
    };
    let input = if with_input {
        Rect {
            x: panel.x,
            y: actions_y - m.gap - m.panel_input_h,
            w: panel.w,
            h: m.panel_input_h,
        }
    } else {
        Rect::zero()
    };
    let search = if with_input {
        Rect {
            x: panel.x + m.gap * 6,
            y: title.bottom() + m.gap,
            w: panel.w - m.gap * 6,
            h: m.action_h,
        }
    } else {
        Rect::zero()
    };
    let list_top = if with_input {
        search.bottom() + m.gap
    } else {
        title.bottom() + m.gap
    };
    let list_bottom = if with_input {
        input.y - m.gap * 3
    } else if !actions.is_empty() {
        actions[0].y
    } else {
        detail.y
    };
    let list = Rect {
        x: panel.x,
        y: list_top,
        w: panel.w,
        h: (list_bottom - m.gap - list_top).max(m.gap * 4),
    };
    PanelLayout {
        title,
        search,
        list,
        detail,
        input,
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::new(96)
    }

    fn overlapping(a: Rect, b: Rect) -> bool {
        let gap = 8;
        !(a.right() + gap <= b.x
            || b.right() + gap <= a.x
            || a.bottom() + gap <= b.y
            || b.bottom() + gap <= a.y)
    }

    #[test]
    fn chat_column_stacks_without_overlap_at_minimum_size() {
        let m = metrics();
        let layout = compute(m.min_client_w, m.min_client_h, false, &m);
        assert!(layout.canvas.w >= 400, "chat column stays readable");
        assert!(layout.canvas.h >= 240);
        assert_eq!(layout.canvas.bottom() + m.gap * 2, layout.composer.y);
        assert!(layout.input.bottom() <= layout.hint.y);
        assert!(layout.input.x > layout.composer.x);
        assert!(layout.send.bottom() < layout.composer.bottom());
        assert!(layout.send.right() <= layout.canvas.right() + 1);
        assert!(!overlapping(layout.canvas, layout.input));
        assert!(layout.notice.is_none());
    }

    #[test]
    fn notice_sits_between_chat_and_status_when_present() {
        let m = metrics();
        let layout = compute(m.min_client_w, m.min_client_h, true, &m);
        let notice = layout.notice.expect("notice rect reserved");
        assert_eq!(notice.bottom(), layout.status.y);
        assert_eq!(layout.composer.bottom() + m.gap * 2, notice.y);
    }

    #[test]
    fn sidebar_and_status_bar_span_the_client() {
        let m = metrics();
        let layout = compute(1024, 704, false, &m);
        assert_eq!(layout.header.x, layout.navigation.right());
        assert_eq!(layout.header.right(), layout.sidebar.x);
        assert_eq!(layout.sidebar.right(), 1024);
        assert_eq!(layout.status.bottom(), 704);
        assert!(layout.revoke.right() + m.margin <= 1024);
        assert!(layout.status_text.right() + m.gap <= layout.revoke.x);
        assert_eq!(layout.tabs.len(), 3);
        for pair in layout.tabs.windows(2) {
            assert_eq!(pair[0].bottom() + m.gap, pair[1].y);
            assert_eq!(pair[0].h, m.tab_h);
        }
        assert!(layout.panel.x > layout.navigation.right());
        assert!(layout.panel.h >= m.detail_h + m.action_h + m.panel_input_h + m.gap * 4);
    }

    #[test]
    fn small_windows_are_clamped_to_minimum() {
        let m = metrics();
        let layout = compute(200, 200, false, &m);
        assert_eq!(layout.status.bottom(), m.min_client_h);
        assert_eq!(layout.sidebar.right(), m.min_client_w);
    }

    #[test]
    fn navigation_composer_and_drawer_fit_at_all_supported_scales() {
        for dpi in [96, 120, 144, 192] {
            let m = Metrics::new(dpi);
            for drawer in [false, true] {
                for notice in [false, true] {
                    let l = compute_with_drawer(m.min_client_w, m.min_client_h, notice, drawer, &m);
                    assert!(l.canvas.h >= crate::theme::scale(200, dpi));
                    assert!(l.canvas.w >= crate::theme::scale(400, dpi));
                    assert!(l.canvas.x >= l.navigation.right() + m.margin);
                    assert!(l.input.right() < l.composer.right());
                    assert!(l.input.bottom() <= l.send.y);
                    assert!(l.hint.right() + m.gap <= l.send.x);
                    assert!(l.composer.bottom() < l.status.y);
                    assert!(l.tabs[2].bottom() < l.navigation.bottom() - m.margin);
                    if drawer {
                        assert!(l.composer.right() + m.margin <= l.sidebar.x);
                    } else {
                        assert_eq!(l.canvas.x - m.nav_w, m.min_client_w - l.canvas.right());
                    }
                }
            }
        }
    }

    #[test]
    fn panel_children_place_rows_in_declared_order() {
        let m = metrics();
        let panel = Rect {
            x: 0,
            y: 0,
            w: 304,
            h: 400,
        };
        let memory = panel_children(panel, &m, 2, true);
        assert_eq!(memory.actions.len(), 2);
        assert!(memory.list.bottom() + m.gap <= memory.input.y);
        assert_eq!(memory.input.bottom() + m.gap, memory.actions[0].y);
        assert_eq!(memory.actions[0].bottom(), memory.actions[1].bottom());
        assert_eq!(memory.actions[1].right(), panel.right());
        assert_eq!(memory.detail.bottom(), panel.bottom());
        assert!(memory.list.h > 0);

        let presets = panel_children(panel, &m, 1, false);
        assert_eq!(presets.actions.len(), 1);
        assert_eq!(presets.actions[0].w, panel.w);
        assert_eq!(presets.input, Rect::zero());
        assert_eq!(presets.list.bottom() + m.gap, presets.actions[0].y);

        let plain = panel_children(panel, &m, 0, false);
        assert!(plain.actions.is_empty());
        assert_eq!(plain.input, Rect::zero());
        assert_eq!(plain.list.bottom() + m.gap, plain.detail.y);
    }
}
