use super::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{BM_GETCHECK, KillTimer, SetTimer};

pub(super) fn rects(area: Rect, dpi: u32) -> [Rect; 6] {
    // The notice strip may reduce the minimum-height drawer. Keep the explicit
    // transmission permission, Apply button and storage note inside the panel.
    let compact = area.h < theme::scale(380, dpi);
    let offsets = if compact {
        [60, 116, 172, 228, 292, 332]
    } else {
        [68, 128, 188, 248, 320, 368]
    };
    let heights = if compact {
        [26, 26, 26, 48, 32, 32]
    } else {
        [30, 30, 30, 56, 36, 36]
    };
    std::array::from_fn(|index| Rect {
        x: area.x,
        y: area.y + theme::scale(offsets[index], dpi),
        w: area.w,
        h: theme::scale(heights[index], dpi),
    })
}

pub(super) unsafe fn labels(controls: Controls, window: HWND, captured: Option<u64>) {
    for (index, label) in [(4, "允许发送人设、私密记忆与本轮会话"), (5, "应用模型设置")]
    {
        SetWindowTextW(controls.model_fields[index], wide(label).as_ptr());
        if !instance_valid(window, captured) {
            return;
        }
    }
}

pub(super) unsafe fn save(pointer: *mut DesktopState) {
    let (main, controls) = {
        let state = &*pointer;
        (state.main, state.controls)
    };
    let captured = window_instance(main);
    let mut values = Vec::new();
    for field in controls.model_fields.iter().take(4) {
        let Some(value) = control_text_if_alive(main, *field) else {
            return;
        };
        values.push(value);
    }
    let allowed = SendMessageW(controls.model_fields[4], BM_GETCHECK, 0, 0) == 1;
    if !instance_valid(main, captured) {
        return;
    }
    {
        let state = &mut *pointer;
        let settings = moe_chat_app::ModelSettings {
            endpoint: values[0].trim().into(),
            model: values[1].trim().into(),
            persona: values[3].clone(),
            allow_transmission: allowed,
        };
        match state.app.configure_model(&settings) {
            Ok(()) => {
                state.model_key = values[2].clone();
                state.view.error = Some(match crate::storage::save_settings(&settings) {
                    Ok(()) => (
                        "设置已保存；API Key 与发送许可不落盘，重启需重新输入并确认".into(),
                        Severity::Info,
                    ),
                    Err(error) => (error, Severity::Error),
                });
            }
            Err(error) => set_error(state, &error),
        }
        state.revision = state.revision.wrapping_add(1);
    }
    refresh(pointer);
}

pub(super) unsafe fn start(pointer: *mut DesktopState) {
    let (main, waiting) = {
        let state = &mut *pointer;
        let Some(request) = state.app.take_external_request() else {
            return;
        };
        let (input, ticket) = request.into_parts();
        match crate::transport::start(input, state.model_key.clone()) {
            Ok(worker) => {
                state.model_worker = Some(worker);
                state.model_ticket = Some(ticket);
            }
            Err(_) => {
                let now = elapsed_ms(state);
                if let Err(error) = state.app.complete_external(
                    ticket,
                    moe_agent_runtime::ModelOutcome::Failed(
                        moe_agent_runtime::ModelFailure::Unavailable,
                    ),
                    now,
                ) {
                    set_error(state, &error);
                }
            }
        }
        (state.main, state.model_worker.is_some())
    };
    if waiting {
        SetTimer(main, 9, 100, None);
    }
}

pub(super) unsafe fn poll(pointer: *mut DesktopState) {
    let main = {
        let state = &mut *pointer;
        let Some(worker) = state.model_worker.take() else {
            let main = state.main;
            KillTimer(main, 9);
            return;
        };
        let reply = match worker.try_recv() {
            Ok(reply) => reply,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                state.model_worker = Some(worker);
                return;
            }
            Err(_) => crate::transport::Reply {
                outcome: moe_agent_runtime::ModelOutcome::Unknown,
                message: "请求线程已中断；结果未知，不会重发",
            },
        };
        let Some(ticket) = state.model_ticket.take() else {
            return;
        };
        let now = elapsed_ms(state);
        match state.app.complete_external(ticket, reply.outcome, now) {
            Ok(()) => {
                let severity = presenter::status(&state.app.snapshot()).severity;
                state.view.error = Some((reply.message.into(), severity));
            }
            Err(error) => set_error(state, &error),
        }
        state.revision = state.revision.wrapping_add(1);
        state.main
    };
    KillTimer(main, 9);
    refresh(pointer);
}
