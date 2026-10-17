//! 桌面窗口的全部自绘：伙伴信息栏、通知条、聊天气泡、空态占位、
//! 状态条与自绘按钮/列表行。颜色与间距全部来自 `theme`。

use super::*;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush,
    DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_TOP,
    DT_VCENTER, DT_WORDBREAK, DeleteDC, DrawFocusRect, DrawTextW, Ellipse, EndPaint, FillRect,
    FrameRect, HBITMAP, LineTo, MoveToEx, PAINTSTRUCT, PS_SOLID, RoundRect, SRCCOPY, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, ODT_BUTTON, ODT_LISTBOX,
};

pub(super) unsafe fn paint_main(state: &DesktopState, window: HWND) {
    let mut paint_struct = PAINTSTRUCT::default();
    let hdc = BeginPaint(window, &mut paint_struct);
    let mut client = RECT::default();
    GetClientRect(window, &mut client);
    fill(hdc, &client, state.brushes.bg);
    if let Some(plan) = state.layout.as_ref() {
        paint_navigation(state, hdc, plan);
        paint_header(state, hdc, &plan.header);
        if let Some(notice_rect) = plan.notice {
            paint_notice(
                state,
                hdc,
                &notice_rect,
                state.view.notice.as_deref().unwrap_or(""),
            );
        }
        paint_sidebar(state, hdc, plan);
        paint_chat_chrome(state, hdc, plan);
        paint_status(state, hdc, plan);
    }
    EndPaint(window, &paint_struct);
}

unsafe fn paint_header(state: &DesktopState, hdc: HDC, header: &Rect) {
    let s = |v| theme::scale(v, state.dpi);
    let name_x = header.x + state.metrics.margin;
    let name_rect = RECT {
        left: name_x,
        top: header.y + s(18),
        right: name_x + s(164),
        bottom: header.y + s(46),
    };
    text(
        hdc,
        state.view.header.name,
        name_rect,
        DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX,
        theme::INK,
        state.fonts.title,
    );
    text(
        hdc,
        "一段对话，一点陪伴",
        RECT {
            top: name_rect.bottom,
            bottom: header.bottom(),
            ..name_rect
        },
        DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX,
        theme::MUTED,
        state.fonts.small,
    );
    let badge_color = if state.view.header.revoked {
        theme::DANGER
    } else {
        theme::ACCENT
    };
    let badge_rect = RECT {
        left: (header.right() - s(214)).max(name_rect.right + s(8)),
        top: header.y + s(24),
        right: header.right() - state.metrics.margin,
        bottom: header.y + s(54),
    };
    rounded(hdc, &badge_rect, theme::ACCENT_SOFT, None, s(15));
    text(
        hdc,
        &state.view.header.offline_text,
        inset_win(&badge_rect, s(6)),
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        badge_color,
        state.fonts.small,
    );
}

unsafe fn paint_navigation(state: &DesktopState, hdc: HDC, plan: &layout::ClientLayout) {
    fill(hdc, &to_win(&plan.navigation), state.brushes.nav);
    let s = |v| theme::scale(v, state.dpi);
    let mark = RECT {
        left: s(24),
        top: s(24),
        right: s(64),
        bottom: s(64),
    };
    rounded(hdc, &mark, theme::ACCENT, None, s(14));
    flower(hdc, inset_win(&mark, s(8)), theme::PANEL, theme::ACCENT);
    text(
        hdc,
        "MOE\nBUDDY",
        RECT {
            left: 0,
            top: plan.navigation.bottom() - s(64),
            right: plan.navigation.right(),
            bottom: plan.navigation.bottom() - s(16),
        },
        DT_CENTER | DT_TOP | DT_NOPREFIX,
        theme::MUTED,
        state.fonts.small,
    );
}

/// Four geometric petals; no downloaded font, bitmap or character asset.
unsafe fn flower(hdc: HDC, rect: RECT, color: u32, center: u32) {
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let petal = RECT {
            left: rect.left + x * w / 2,
            top: rect.top + y * h / 2,
            right: rect.left + (x + 1) * w / 2,
            bottom: rect.top + (y + 1) * h / 2,
        };
        stroke_ellipse(hdc, &petal, color, color);
    }
    let dot = inset_win(&rect, w * 2 / 5);
    stroke_ellipse(hdc, &dot, center, center);
}

unsafe fn paint_notice(state: &DesktopState, hdc: HDC, notice_rect: &Rect, notice: &str) {
    let body = RECT {
        left: notice_rect.x,
        top: notice_rect.y,
        right: notice_rect.right(),
        bottom: notice_rect.bottom() - 1,
    };
    fill(hdc, &body, state.brushes.warn_soft);
    let edge = RECT {
        left: notice_rect.x,
        top: notice_rect.bottom() - 1,
        right: notice_rect.right(),
        bottom: notice_rect.bottom(),
    };
    fill(hdc, &edge, state.brushes.border);
    let glyph_rect = RECT {
        left: notice_rect.x + state.metrics.margin,
        right: notice_rect.x + state.metrics.margin + state.small_line * 2,
        ..body
    };
    text(
        hdc,
        "▲",
        glyph_rect,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        theme::WARN,
        state.fonts.small,
    );
    let text_rect = RECT {
        left: glyph_rect.right + state.metrics.gap,
        right: notice_rect.right() - state.metrics.margin,
        ..body
    };
    text(
        hdc,
        notice,
        text_rect,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        theme::WARN,
        state.fonts.small,
    );
}

unsafe fn paint_sidebar(state: &DesktopState, hdc: HDC, plan: &layout::ClientLayout) {
    if state.view.active_tab.is_none() {
        return;
    }
    let sidebar = to_win(&plan.sidebar);
    fill(hdc, &sidebar, state.brushes.panel);
    fill(
        hdc,
        &RECT {
            right: sidebar.left + 1,
            ..sidebar
        },
        state.brushes.border,
    );

    let Some(tab) = state.view.active_tab else {
        return;
    };
    let index = SideTab::ALL
        .iter()
        .position(|item| *item == tab)
        .unwrap_or(0);
    let panel_layout = &state.panels[index];
    if tab == SideTab::Memory {
        let label = RECT {
            left: panel_layout.title.x,
            top: panel_layout.search.y,
            right: panel_layout.search.x - state.metrics.gap,
            bottom: panel_layout.search.bottom(),
        };
        text(
            hdc,
            "搜索",
            label,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            theme::MUTED,
            state.fonts.small,
        );
        rounded(
            hdc,
            &inset_rect(&panel_layout.search, -theme::scale(4, state.dpi)),
            theme::PANEL,
            Some(if state.focus_ring == ID_MEM_SEARCH {
                theme::ACCENT
            } else {
                theme::BORDER
            }),
            state.metrics.radius_ctrl,
        );
        let label = RECT {
            left: panel_layout.input.x,
            top: panel_layout.input.y - state.small_line - state.metrics.gap,
            right: panel_layout.input.right(),
            bottom: panel_layout.input.y - state.metrics.gap,
        };
        let caption = if state.view.editing_memory.is_some() {
            "正在编辑 · 保存后替换原记忆"
        } else {
            "新记忆"
        };
        text(
            hdc,
            caption,
            label,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            theme::MUTED,
            state.fonts.small,
        );
    }
    let title = presenter::panel_title(tab);
    text(
        hdc,
        title,
        to_win(&panel_layout.title),
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        theme::INK,
        state.fonts.title,
    );
    if tab == SideTab::Memory {
        text(
            hdc,
            "最多 100 条",
            to_win(&panel_layout.title),
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            theme::MUTED,
            state.fonts.small,
        );
    }

    if tab == SideTab::Presets && state.view.presets.visible {
        let rects = super::model_settings::rects(panel_layout.list, state.dpi);
        for (index, label) in [
            "完整端点（含 /chat/completions）",
            "模型名称",
            "API Key（仅本次运行）",
            "伙伴人设",
        ]
        .iter()
        .enumerate()
        {
            let mut label_rect = rects[index];
            label_rect.y -= theme::scale(26, state.dpi);
            label_rect.h = theme::scale(20, state.dpi);
            text(
                hdc,
                label,
                to_win(&label_rect),
                DT_LEFT | DT_SINGLELINE | DT_NOPREFIX,
                theme::MUTED,
                state.fonts.small,
            );
            rounded(
                hdc,
                &inset_rect(&rects[index], -theme::scale(4, state.dpi)),
                theme::PANEL,
                Some(if state.focus_ring == 260 + index as i32 {
                    theme::ACCENT
                } else {
                    theme::BORDER
                }),
                state.metrics.radius_ctrl,
            );
        }
        let mut location = rects[5];
        location.y = location.bottom() + theme::scale(8, state.dpi);
        location.h = theme::scale(72, state.dpi);
        text(
            hdc,
            "保存：%LOCALAPPDATA%\\MoeBuddy\\daily\n私密记忆持续保存；聊天保留 30 天、回收 7 天。重启需重新确认发送许可。",
            to_win(&location),
            DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
            theme::MUTED,
            state.fonts.small,
        );
        return;
    }
    let model = match tab {
        SideTab::Memory => &state.view.memory,
        SideTab::Presets => &state.view.presets,
        SideTab::Logs => &state.view.logs,
    };
    let list_id = match tab {
        SideTab::Memory => ID_MEM_LIST,
        SideTab::Presets => ID_PRESET_LIST,
        SideTab::Logs => ID_LOG_LIST,
    };
    let focus_brush = if state.focus_ring == list_id {
        state.brushes.accent
    } else {
        state.brushes.border
    };
    frame(hdc, &inset_rect(&panel_layout.list, -1), focus_brush);

    if !model.visible {
        let hint = inset_rect(&panel_layout.list, state.metrics.margin * 2);
        text(
            hdc,
            model.closed_text,
            hint,
            DT_CENTER | DT_VCENTER | DT_WORDBREAK | DT_NOPREFIX,
            theme::MUTED,
            state.fonts.small,
        );
    } else if model.rows.is_empty() {
        let hint = inset_rect(&panel_layout.list, state.metrics.margin * 2);
        text(
            hdc,
            model.empty_text,
            hint,
            DT_CENTER | DT_VCENTER | DT_WORDBREAK | DT_NOPREFIX,
            theme::MUTED,
            state.fonts.small,
        );
    }

    let detail_color = if tab == SideTab::Memory && state.view.mem_selected.is_some() {
        theme::INK
    } else {
        theme::MUTED
    };
    let detail = match tab {
        SideTab::Memory => &state.view.mem_detail,
        SideTab::Presets => &state.view.preset_detail,
        SideTab::Logs => &state.view.log_detail,
    };
    text(
        hdc,
        detail,
        to_win(&panel_layout.detail),
        DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX,
        detail_color,
        state.fonts.small,
    );
}

unsafe fn paint_chat_chrome(state: &DesktopState, hdc: HDC, plan: &layout::ClientLayout) {
    let input_focus = state.focus_ring == ID_INPUT;
    rounded(
        hdc,
        &to_win(&plan.composer),
        theme::PANEL,
        Some(if input_focus {
            theme::ACCENT
        } else {
            theme::BORDER
        }),
        state.metrics.radius_panel,
    );
    let memory_active = state.view.active_tab == Some(SideTab::Memory) && state.view.memory.visible;
    if let (true, Some(panel_layout)) = (
        memory_active && !state.view.header.revoked,
        state.panels.first(),
    ) {
        let focus = state.focus_ring == ID_MEM_INPUT;
        rounded(
            hdc,
            &inset_rect(&panel_layout.input, -theme::scale(4, state.dpi)),
            theme::PANEL,
            Some(if focus { theme::ACCENT } else { theme::BORDER }),
            state.metrics.radius_ctrl,
        );
    }
    text(
        hdc,
        presenter::input_hint_text(),
        to_win(&plan.hint),
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        theme::MUTED,
        state.fonts.small,
    );
}

unsafe fn paint_status(state: &DesktopState, hdc: HDC, plan: &layout::ClientLayout) {
    fill(hdc, &to_win(&plan.status), state.brushes.bg);
    let (label, severity) = match &state.view.error {
        Some((text, severity)) => (text.clone(), *severity),
        None => (state.view.status.text.clone(), state.view.status.severity),
    };
    let color = match severity {
        Severity::Info => theme::MUTED,
        Severity::Busy => theme::ACCENT,
        Severity::Warn => theme::WARN,
        Severity::Error => theme::DANGER,
    };
    let glyph = match severity {
        Severity::Warn | Severity::Error => "▲",
        _ => "●",
    };
    let glyph_rect = RECT {
        left: plan.status_text.x,
        right: plan.status_text.x + state.small_line,
        top: plan.status_text.y + theme::scale(8, state.dpi),
        bottom: plan.status_text.y + theme::scale(8, state.dpi) + state.small_line,
    };
    text(
        hdc,
        glyph,
        glyph_rect,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        color,
        state.fonts.small,
    );
    let label_rect = RECT {
        left: glyph_rect.right + state.metrics.gap,
        right: plan.status_text.right(),
        top: plan.status_text.y + theme::scale(8, state.dpi),
        bottom: plan.status_text.bottom() - theme::scale(4, state.dpi),
    };
    text(
        hdc,
        &label,
        label_rect,
        DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX,
        color,
        state.fonts.small,
    );
}

pub(super) unsafe fn paint_canvas(state: &DesktopState, window: HWND) {
    let mut paint_struct = PAINTSTRUCT::default();
    let hdc = BeginPaint(window, &mut paint_struct);
    let mut client = RECT::default();
    GetClientRect(window, &mut client);
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let memory_dc = CreateCompatibleDC(hdc);
    let bitmap = CreateCompatibleBitmap(hdc, width, height);
    let old_bitmap = SelectObject(memory_dc, bitmap as _);
    fill(memory_dc, &client, state.brushes.bg);

    let empty = state
        .canvas_view
        .layout
        .as_ref()
        .is_none_or(|history| history.blocks.is_empty());
    if empty {
        paint_empty_chat(state, memory_dc, width, height);
    } else {
        paint_bubbles(state, memory_dc, width, height);
    }

    BitBlt(hdc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
    SelectObject(memory_dc, old_bitmap);
    DeleteObject(bitmap as HBITMAP);
    DeleteDC(memory_dc);
    EndPaint(window, &paint_struct);
}

unsafe fn paint_empty_chat(state: &DesktopState, hdc: HDC, width: i32, height: i32) {
    let s = |v| theme::scale(v, state.dpi);
    let compact = height < s(300);
    let left = ((width - s(460)) / 2).max(s(24));
    let top = ((height - s(if compact { 170 } else { 270 })) / 2).max(s(12));
    let size = s(if compact { 36 } else { 64 });
    flower(
        hdc,
        RECT {
            left,
            top,
            right: left + size,
            bottom: top + size,
        },
        theme::ACCENT,
        theme::BG,
    );
    let heading = RECT {
        left,
        top: top + size + s(24),
        right: width - s(16),
        bottom: top + size + s(24 + if compact { 38 } else { 84 }),
    };
    text(
        hdc,
        if compact {
            "今天，慢慢聊。"
        } else {
            "留一点时间，\n给今天的自己。"
        },
        heading,
        DT_LEFT | DT_TOP | DT_NOPREFIX,
        theme::INK,
        if compact {
            state.fonts.title
        } else {
            state.fonts.hero
        },
    );
    text(
        hdc,
        presenter::chat_empty_text(),
        RECT {
            top: heading.bottom + s(16),
            bottom: height,
            ..heading
        },
        DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX,
        theme::MUTED,
        state.fonts.small,
    );
}

unsafe fn paint_bubbles(state: &DesktopState, hdc: HDC, width: i32, height: i32) {
    let Some(history) = state.canvas_view.layout.as_ref() else {
        return;
    };
    let margin = state.metrics.margin;
    let pad_x = state.metrics.gap * 2;
    let pad_y = state.metrics.gap;
    let inner = bubble_inner_width(state, width);
    let radius = state.metrics.radius_panel;
    for (block, message) in history.blocks.iter().zip(&state.canvas_view.messages) {
        let top = block.top - state.canvas_view.scroll;
        let bottom = top + block.height;
        if bottom < 0 || top > height {
            continue;
        }
        let user = message.role == moe_chat_app::MessageRole::User;
        let label_rect = RECT {
            left: margin,
            top,
            right: width - margin,
            bottom: top + state.small_line,
        };
        text(
            hdc,
            presenter::role_label(message.role),
            label_rect,
            (if user { DT_RIGHT } else { DT_LEFT }) | DT_TOP | DT_SINGLELINE | DT_NOPREFIX,
            theme::MUTED,
            state.fonts.small,
        );
        let (_, text_width) = measure_extent(state, hdc, &message.text, inner);
        let bubble_w = (text_width + 2 * pad_x).clamp(state.metrics.gap * 6, inner + 2 * pad_x);
        let bubble_h = block.height - state.small_line;
        let bubble_x = if user {
            width - margin - bubble_w
        } else {
            margin
        };
        let bubble_y = top + state.small_line;
        let bubble = RECT {
            left: bubble_x,
            top: bubble_y,
            right: bubble_x + bubble_w,
            bottom: bubble_y + bubble_h,
        };
        if user {
            rounded(hdc, &bubble, theme::ACCENT, None, radius);
            text(
                hdc,
                &message.text,
                RECT {
                    left: bubble.left + pad_x,
                    top: bubble.top + pad_y,
                    right: bubble.left + pad_x + text_width,
                    bottom: bubble.bottom - pad_y,
                },
                DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX,
                theme::PANEL,
                state.fonts.body,
            );
        } else {
            rounded(hdc, &bubble, theme::PANEL, None, radius);
            text(
                hdc,
                &message.text,
                RECT {
                    left: bubble.left + pad_x,
                    top: bubble.top + pad_y,
                    right: bubble.left + pad_x + text_width,
                    bottom: bubble.bottom - pad_y,
                },
                DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX,
                theme::INK,
                state.fonts.body,
            );
        }
    }
}

pub(super) unsafe fn draw_item(state: &DesktopState, draw: &DRAWITEMSTRUCT) {
    if draw.CtlType == ODT_BUTTON {
        draw_button(state, draw);
    } else if draw.CtlType == ODT_LISTBOX {
        draw_list_row(state, draw);
    }
}

unsafe fn draw_button(state: &DesktopState, draw: &DRAWITEMSTRUCT) {
    let id = draw.CtlID as i32;
    let rect = draw.rcItem;
    let hdc = draw.hDC;
    let disabled = draw.itemState & ODS_DISABLED != 0;
    let pressed = draw.itemState & ODS_SELECTED != 0;
    let focused = draw.itemState & ODS_FOCUS != 0;
    // Do not query child HWNDs while borrowing DesktopState in WM_DRAWITEM.
    let label = match id {
        ID_CHAT_TAB => "聊天",
        ID_SEND => "发送  ↗",
        ID_MEM_REMEMBER => "保存记忆",
        ID_MEM_REFRESH => "新建 / 刷新",
        ID_MEM_EDIT => "编辑选中",
        ID_MEM_DELETE => "删除选中",
        ID_PRESET_APPLY => "切换预设",
        ID_LOG_REFRESH => "刷新",
        ID_REVOKE => "撤销授权",
        252 => "应用模型设置",
        _ => TAB_IDS
            .iter()
            .position(|tab| *tab == id)
            .map_or("", |index| presenter::tab_title(SideTab::ALL[index])),
    };

    fill(
        hdc,
        &rect,
        if id == ID_CHAT_TAB || TAB_IDS.contains(&id) {
            state.brushes.nav
        } else if id == ID_REVOKE {
            state.brushes.bg
        } else {
            state.brushes.panel
        },
    );

    if id == ID_SEND || id == 252 {
        let face = if disabled {
            theme::DISABLED_FILL
        } else if pressed {
            theme::ACCENT_DARK
        } else {
            theme::ACCENT
        };
        rounded(hdc, &rect, face, None, state.metrics.radius_ctrl);
        text(
            hdc,
            label,
            rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            if disabled {
                theme::DISABLED_TEXT
            } else {
                theme::PANEL
            },
            state.fonts.body_bold,
        );
    } else if id == ID_CHAT_TAB || TAB_IDS.contains(&id) {
        let active = if id == ID_CHAT_TAB {
            state.view.active_tab.is_none()
        } else {
            TAB_IDS
                .iter()
                .position(|tab| *tab == id)
                .is_some_and(|index| state.view.active_tab == Some(SideTab::ALL[index]))
        };
        if active {
            rounded(hdc, &rect, theme::PANEL, None, state.metrics.radius_ctrl);
        } else if pressed {
            rounded(
                hdc,
                &rect,
                theme::ACCENT_SOFT,
                None,
                state.metrics.radius_ctrl,
            );
        }
        let color = if disabled {
            theme::DISABLED_TEXT
        } else if active {
            theme::ACCENT
        } else {
            theme::MUTED
        };
        let size = theme::scale(22, state.dpi);
        let x = (rect.left + rect.right - size) / 2;
        let y = rect.top + theme::scale(10, state.dpi);
        navigation_icon(
            hdc,
            id,
            x,
            y,
            size,
            color,
            if active {
                theme::PANEL
            } else if pressed {
                theme::ACCENT_SOFT
            } else {
                theme::NAV
            },
        );
        text(
            hdc,
            label,
            RECT {
                top: y + size + theme::scale(5, state.dpi),
                bottom: rect.bottom - theme::scale(5, state.dpi),
                ..rect
            },
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            if disabled {
                theme::DISABLED_TEXT
            } else if active {
                theme::ACCENT_DARK
            } else {
                theme::MUTED
            },
            state.fonts.small,
        );
    } else if id == ID_REVOKE {
        text(
            hdc,
            label,
            rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            if disabled {
                theme::DISABLED_TEXT
            } else {
                theme::MUTED
            },
            state.fonts.small,
        );
    } else {
        rounded(
            hdc,
            &rect,
            if disabled {
                theme::DISABLED_FILL
            } else {
                theme::PANEL
            },
            Some(theme::BORDER),
            state.metrics.radius_ctrl,
        );
        text(
            hdc,
            label,
            rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            if disabled {
                theme::DISABLED_TEXT
            } else {
                theme::INK
            },
            state.fonts.small,
        );
    }
    if focused && !disabled {
        DrawFocusRect(hdc, &inset_win(&rect, 3));
    }
}

unsafe fn navigation_icon(hdc: HDC, id: i32, x: i32, y: i32, size: i32, ink: u32, face: u32) {
    let p = |v| v * size / 24;
    let pen = CreatePen(PS_SOLID, (size / 12).max(1), ink);
    let old = SelectObject(hdc, pen as _);
    let line = |points: &[(i32, i32)]| {
        MoveToEx(
            hdc,
            x + p(points[0].0),
            y + p(points[0].1),
            std::ptr::null_mut(),
        );
        for &(a, b) in &points[1..] {
            LineTo(hdc, x + p(a), y + p(b));
        }
    };
    match id {
        ID_CHAT_TAB => {
            rounded(
                hdc,
                &RECT {
                    left: x + p(2),
                    top: y + p(2),
                    right: x + p(22),
                    bottom: y + p(18),
                },
                face,
                Some(ink),
                p(4),
            );
            line(&[(6, 18), (6, 22), (11, 18)]);
            line(&[(7, 8), (17, 8)]);
            line(&[(7, 12), (13, 12)]);
        }
        ID_TAB_BASE => {
            line(&[
                (12, 5),
                (8, 3),
                (2, 3),
                (2, 20),
                (8, 20),
                (12, 22),
                (16, 20),
                (22, 20),
                (22, 3),
                (16, 3),
                (12, 5),
                (12, 22),
            ]);
        }
        id if id == ID_TAB_BASE + 1 => {
            for (row, knob) in [(5, 8), (12, 16), (19, 10)] {
                line(&[(2, row), (22, row)]);
                stroke_ellipse(
                    hdc,
                    &RECT {
                        left: x + p(knob - 3),
                        top: y + p(row - 3),
                        right: x + p(knob + 3),
                        bottom: y + p(row + 3),
                    },
                    face,
                    ink,
                );
            }
        }
        _ => {
            line(&[(3, 2), (3, 21), (23, 21)]);
            line(&[(7, 16), (11, 10), (15, 13), (21, 5)]);
        }
    }
    SelectObject(hdc, old);
    DeleteObject(pen as _);
}

unsafe fn draw_list_row(state: &DesktopState, draw: &DRAWITEMSTRUCT) {
    let id = draw.CtlID as i32;
    let hdc = draw.hDC;
    let rect = draw.rcItem;
    let selected = draw.itemState & ODS_SELECTED != 0;
    let disabled = draw.itemState & ODS_DISABLED != 0;
    let focused = draw.itemState & ODS_FOCUS != 0;
    fill(
        hdc,
        &rect,
        if selected {
            state.brushes.accent_soft
        } else {
            state.brushes.panel
        },
    );
    let (rows, index) = match id {
        ID_MEM_LIST => (&state.view.memory.rows, draw.itemID as usize),
        ID_PRESET_LIST => (&state.view.presets.rows, draw.itemID as usize),
        ID_LOG_LIST => (&state.view.logs.rows, draw.itemID as usize),
        _ => return,
    };
    let Some(row) = rows.get(index) else {
        return;
    };
    let ink = if disabled {
        theme::DISABLED_TEXT
    } else {
        theme::INK
    };
    let muted = if disabled {
        theme::DISABLED_TEXT
    } else {
        theme::MUTED
    };
    let accent = if disabled {
        theme::DISABLED_TEXT
    } else {
        theme::ACCENT_DARK
    };
    let pad = state.metrics.gap / 2;
    match id {
        ID_PRESET_LIST => {
            let primary = RECT {
                left: rect.left + pad * 2,
                top: rect.top + pad,
                right: rect.right - pad * 2,
                bottom: rect.top + pad + state.small_line,
            };
            text(
                hdc,
                &row.primary,
                primary,
                DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                if row.active { accent } else { ink },
                state.fonts.small,
            );
            if let Some(secondary) = &row.secondary {
                let secondary_rect = RECT {
                    left: rect.left + pad * 2,
                    top: primary.bottom,
                    right: rect.right - pad * 2,
                    bottom: rect.bottom - pad,
                };
                text(
                    hdc,
                    secondary,
                    secondary_rect,
                    DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                    muted,
                    state.fonts.small,
                );
            }
        }
        ID_LOG_LIST => {
            let primary = RECT {
                left: rect.left + pad * 2,
                right: rect.right - pad * 2,
                top: rect.top + pad,
                bottom: rect.top + pad + state.small_line,
            };
            text(
                hdc,
                &row.primary,
                primary,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                ink,
                state.fonts.mono,
            );
            if let Some(secondary) = &row.secondary {
                let secondary_rect = RECT {
                    left: rect.left + pad * 2,
                    right: rect.right - pad * 2,
                    top: primary.bottom,
                    ..rect
                };
                text(
                    hdc,
                    secondary,
                    secondary_rect,
                    DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                    muted,
                    state.fonts.mono,
                );
            }
        }
        _ => {
            let primary = RECT {
                left: rect.left + pad * 2,
                right: rect.right - pad * 2,
                ..rect
            };
            text(
                hdc,
                &row.primary,
                primary,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                ink,
                state.fonts.body,
            );
        }
    }
    if selected && focused && !disabled {
        DrawFocusRect(hdc, &inset_win(&rect, 2));
    }
}

pub(super) fn bubble_inner_width(state: &DesktopState, canvas_w: i32) -> i32 {
    let bubble_max = (canvas_w - 2 * state.metrics.margin).max(theme::scale(200, state.dpi));
    let capped = bubble_max.min(theme::scale(520, state.dpi));
    (capped - 4 * state.metrics.gap).max(theme::scale(120, state.dpi))
}

pub(super) unsafe fn measure_lines(state: &DesktopState, value: &str, width: i32) -> i32 {
    if value.is_empty() {
        return 1;
    }
    let hdc = GetDC(state.canvas);
    let (height, _) = measure_extent(state, hdc, value, width);
    ReleaseDC(state.canvas, hdc);
    let line = state.body_line.max(1);
    ((height + line - 1) / line).max(1)
}

/// 以正文字体按给定宽度计算折行结果；返回（像素高度、最宽行像素宽）。
unsafe fn measure_extent(state: &DesktopState, hdc: HDC, value: &str, width: i32) -> (i32, i32) {
    let encoded = super::wide(value);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width.max(1),
        bottom: 0,
    };
    let old = SelectObject(hdc, state.fonts.body as _);
    let height = DrawTextW(
        hdc,
        encoded.as_ptr(),
        -1,
        &mut rect,
        DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
    );
    SelectObject(hdc, old);
    (height, (rect.right - rect.left).max(1))
}

fn to_win(rect: &Rect) -> RECT {
    RECT {
        left: rect.x,
        top: rect.y,
        right: rect.right(),
        bottom: rect.bottom(),
    }
}

fn inset_rect(rect: &Rect, delta: i32) -> RECT {
    RECT {
        left: rect.x + delta,
        top: rect.y + delta,
        right: rect.right() - delta,
        bottom: rect.bottom() - delta,
    }
}

fn inset_win(rect: &RECT, delta: i32) -> RECT {
    RECT {
        left: rect.left + delta,
        top: rect.top + delta,
        right: rect.right - delta,
        bottom: rect.bottom - delta,
    }
}

unsafe fn fill(hdc: HDC, rect: &RECT, brush_handle: HBRUSH) {
    FillRect(hdc, rect, brush_handle);
}

unsafe fn frame(hdc: HDC, rect: &RECT, brush_handle: HBRUSH) {
    FrameRect(hdc, rect, brush_handle);
}

unsafe fn text(hdc: HDC, value: &str, rect: RECT, flags: u32, color: u32, face: HFONT) {
    let encoded = super::wide(value);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, color);
    let old = SelectObject(hdc, face as _);
    let mut target = rect;
    DrawTextW(hdc, encoded.as_ptr(), -1, &mut target, flags);
    SelectObject(hdc, old);
}

/// 圆角填充（可选描边）；无描边时用同色画笔避免残留上次画笔颜色。
unsafe fn rounded(hdc: HDC, rect: &RECT, face: u32, border: Option<u32>, radius: i32) {
    let brush_handle = CreateSolidBrush(face);
    let pen = CreatePen(PS_SOLID, 1, border.unwrap_or(face));
    let old_brush = SelectObject(hdc, brush_handle as _);
    let old_pen = SelectObject(hdc, pen as _);
    RoundRect(
        hdc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius * 2,
        radius * 2,
    );
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    DeleteObject(brush_handle as _);
    DeleteObject(pen as _);
}

unsafe fn stroke_ellipse(hdc: HDC, rect: &RECT, face: u32, border: u32) {
    let brush_handle = CreateSolidBrush(face);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_brush = SelectObject(hdc, brush_handle as _);
    let old_pen = SelectObject(hdc, pen as _);
    Ellipse(hdc, rect.left, rect.top, rect.right, rect.bottom);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    DeleteObject(brush_handle as _);
    DeleteObject(pen as _);
}
