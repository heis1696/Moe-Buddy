//! U2 文字聊天闭环的定向回归：发送、明确记住与召回、预设切换、
//! 日志脱敏与有界查询、真实生命周期计数、隐藏恢复草稿、关闭确认、
//! 不可见面板拒绝、撤权与错误边界。
//!
//! 全部通过公开 API `ChatApplication::new/dispatch/snapshot` 驱动。

use std::error::Error;

use moe_agent_runtime::GatewayError;
use moe_chat_app::{
    Action, AppSnapshot, ChatApplication, ChatStatus, MessageRole, PanelKind, PanelSnapshot,
    PanelVisibility,
};

fn app() -> ChatApplication {
    ChatApplication::new(20_261_014).expect("nonzero process generation")
}

#[test]
fn preset_catalog_is_owned_by_the_registered_view_and_cleared_on_unmount() {
    let mut app = app();
    assert!(app.snapshot().presets.is_empty());
    app.dispatch(Action::Open(PanelKind::Presets), 1).unwrap();
    assert_eq!(app.snapshot().presets.len(), 2);
    app.dispatch(Action::Hide(PanelKind::Presets), 2).unwrap();
    assert!(app.snapshot().presets.is_empty());
    app.dispatch(Action::Open(PanelKind::Presets), 3).unwrap();
    assert_eq!(app.snapshot().presets.len(), 2);
    app.dispatch(Action::Close(PanelKind::Presets), 4).unwrap();
    assert!(app.snapshot().presets.is_empty());
}

#[test]
fn closing_a_view_preserves_the_application_conversation_and_clears_only_the_draft() {
    let mut app = app();
    app.dispatch(Action::Open(PanelKind::Chat), 1).unwrap();
    app.dispatch(Action::SetDraft(String::from("视图独立于会话")), 2)
        .unwrap();
    app.dispatch(Action::Send, 3).unwrap();
    let before = app.snapshot().chat.messages;
    app.dispatch(Action::Close(PanelKind::Chat), 4).unwrap();
    assert!(app.snapshot().chat.messages.is_empty());
    app.dispatch(Action::Open(PanelKind::Chat), 5).unwrap();
    let reopened = app.snapshot();
    assert_eq!(reopened.chat.messages.len(), before.len());
    for (restored, original) in reopened.chat.messages.iter().zip(&before) {
        assert_eq!(restored.role, original.role);
        assert_eq!(restored.text, original.text);
    }
    assert!(reopened.chat.draft.is_empty());
    app.dispatch(Action::Revoke, 6).unwrap();
    assert!(app.snapshot().chat.messages.is_empty());
}

fn panel(snapshot: &AppSnapshot, kind: PanelKind) -> &PanelSnapshot {
    snapshot
        .panels
        .iter()
        .find(|panel| panel.kind == kind)
        .expect("all four panels are always present")
}

fn last_assistant_text(snapshot: &AppSnapshot) -> &str {
    snapshot
        .chat
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::Assistant)
        .map(|message| message.text.as_str())
        .expect("assistant reply recorded")
}

fn open_all(app: &mut ChatApplication, start_ms: u64) {
    for (offset, kind) in [
        PanelKind::Chat,
        PanelKind::Presets,
        PanelKind::Memory,
        PanelKind::Diagnostics,
    ]
    .into_iter()
    .enumerate()
    {
        app.dispatch(Action::Open(kind), start_ms + offset as u64)
            .expect("panel opens");
    }
}

#[test]
fn send_roundtrip_judges_by_actual_delivery() {
    let mut app = app();
    let snapshot = {
        app.dispatch(Action::Open(PanelKind::Chat), 10)
            .expect("open chat");
        app.snapshot()
    };
    assert_eq!(snapshot.mode, "offline");
    assert_eq!(snapshot.companion, "offline-companion");
    assert!(!snapshot.external_transmission_allowed);
    assert!(!snapshot.revoked);
    assert_eq!(snapshot.active_preset, "offline-echo");
    assert!(snapshot.presets.is_empty());

    app.dispatch(Action::SetDraft(String::from("你好，离线伙伴")), 20)
        .expect("draft accepted");
    assert_eq!(app.snapshot().chat.draft, "你好，离线伙伴");
    app.dispatch(Action::Send, 30).expect("send accepted");
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.status, ChatStatus::Ready);
    assert!(snapshot.chat.failure.is_none());
    assert_eq!(snapshot.chat.draft, "");
    assert_eq!(snapshot.chat.messages.len(), 2);
    assert_eq!(snapshot.chat.messages[0].role, MessageRole::User);
    assert_eq!(snapshot.chat.messages[0].text, "你好，离线伙伴");
    assert_eq!(snapshot.chat.messages[1].role, MessageRole::Assistant);
    let reply = &snapshot.chat.messages[1].text;
    assert!(
        reply.contains("离线验收·非真实模型"),
        "reply must be labelled as offline acceptance: {reply}"
    );
    assert!(
        reply.contains("offline-echo"),
        "reply names the preset: {reply}"
    );
    assert!(
        reply.contains("你好，离线伙伴"),
        "reply echoes the turn: {reply}"
    );

    app.dispatch(Action::SetDraft(String::from("再来一句")), 40)
        .expect("draft accepted");
    app.dispatch(Action::Send, 50)
        .expect("second send accepted");
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.messages.len(), 4);
    assert_eq!(snapshot.chat.status, ChatStatus::Ready);
}

#[test]
fn remember_then_recall_next_turn_via_authorized_retrieval() {
    let mut app = app();
    app.dispatch(Action::Open(PanelKind::Chat), 10)
        .expect("open chat");
    app.dispatch(Action::Open(PanelKind::Memory), 11)
        .expect("open memory");
    app.dispatch(Action::Remember(String::from("我最喜欢的酱料是番茄酱")), 20)
        .expect("explicit remember");
    let snapshot = app.snapshot();
    assert!(
        snapshot
            .memories
            .iter()
            .any(|entry| entry.text == "我最喜欢的酱料是番茄酱"),
        "remembered entry is listed after the controlled write"
    );

    let entries_after_remember = app.snapshot().memories.len();
    app.dispatch(Action::SetDraft(String::from("我喜欢的酱料是什么？")), 30)
        .expect("draft accepted");
    app.dispatch(Action::Send, 40).expect("send accepted");
    assert!(
        last_assistant_text(&app.snapshot()).contains("番茄酱"),
        "next turn recalls the remembered fact: {}",
        last_assistant_text(&app.snapshot())
    );
    assert_eq!(app.snapshot().memories.len(), entries_after_remember);
}

#[test]
fn preset_switch_rebinds_registry_and_next_reply() {
    let mut app = app();
    app.dispatch(Action::Open(PanelKind::Chat), 10)
        .expect("open chat");
    app.dispatch(Action::Open(PanelKind::Presets), 11)
        .expect("open presets");
    assert_eq!(app.snapshot().active_preset, "offline-echo");
    app.dispatch(Action::SelectPreset(String::from("offline-reflect")), 20)
        .expect("preset switch");
    assert_eq!(app.snapshot().active_preset, "offline-reflect");

    let error = app
        .dispatch(Action::SelectPreset(String::from("missing")), 30)
        .expect_err("unknown preset rejected");
    assert_eq!(error.public_code(), "unknown_preset");
    assert_eq!(app.snapshot().active_preset, "offline-reflect");

    app.dispatch(Action::SetDraft(String::from("换预设了")), 40)
        .expect("draft accepted");
    app.dispatch(Action::Send, 50).expect("send accepted");
    assert!(
        last_assistant_text(&app.snapshot()).contains("offline-reflect"),
        "next turn resolves the newly bound preset: {}",
        last_assistant_text(&app.snapshot())
    );
}

#[test]
fn diagnostics_are_redacted_and_bounded() {
    let mut app = app();
    open_all(&mut app, 10);
    app.dispatch(Action::SetDraft(String::from("机密用户正文番茄酱")), 20)
        .expect("draft accepted");
    app.dispatch(Action::Send, 30).expect("send accepted");
    app.dispatch(Action::Remember(String::from("私密记忆正文土豆泥")), 40)
        .expect("remember accepted");
    app.dispatch(Action::RefreshDiagnostics, 50)
        .expect("authorized diagnostics refresh");
    let snapshot = app.snapshot();
    assert!(
        !snapshot.diagnostics.is_empty(),
        "business wiring emits events"
    );
    assert!(
        snapshot.diagnostics.len() <= 8,
        "single bounded page per refresh"
    );
    for (index, entry) in snapshot.diagnostics.iter().enumerate() {
        assert_eq!(entry.sequence, (index + 1) as u64);
        assert!(
            entry.run == "unattributed" || entry.run.starts_with("run-"),
            "runs use opaque display aliases"
        );
        for field in [entry.event.as_str(), entry.status.as_str()] {
            assert!(!field.contains("机密"), "no user text in {field}");
            assert!(!field.contains("番茄"), "no user text in {field}");
            assert!(!field.contains("土豆"), "no memory text in {field}");
            assert!(!field.contains("token"), "no credential hints in {field}");
        }
    }
    assert!(
        snapshot.notice.is_none(),
        "no gap is reported before any eviction"
    );

    app.dispatch(Action::RefreshDiagnostics, 60)
        .expect("second bounded query");
    app.dispatch(Action::RefreshDiagnostics, 70)
        .expect("third bounded query");
}

#[test]
fn real_lifecycle_counts_and_hidden_draft_retention() {
    let mut app = app();
    app.dispatch(Action::Open(PanelKind::Chat), 10)
        .expect("open chat");
    let snapshot = app.snapshot();
    let chat = panel(&snapshot, PanelKind::Chat);
    assert_eq!(chat.visibility, PanelVisibility::Visible);
    assert_eq!(chat.mounts, 1);
    assert_eq!(chat.unmounts, 0);
    assert!(chat.renders >= 1, "render callback ran while visible");

    app.dispatch(Action::SetDraft(String::from("隐藏也要保留的草稿")), 20)
        .expect("draft accepted");
    let renders_after_draft = panel(&app.snapshot(), PanelKind::Chat).renders;
    assert!(renders_after_draft >= 2, "every visible drive renders");
    app.dispatch(Action::Hide(PanelKind::Chat), 30)
        .expect("hide chat");
    let snapshot = app.snapshot();
    let chat = panel(&snapshot, PanelKind::Chat);
    assert_eq!(chat.visibility, PanelVisibility::Hidden);
    assert_eq!(chat.mounts, 1);
    assert_eq!(chat.unmounts, 1);
    assert_eq!(snapshot.chat.draft, "");
    let renders_while_hidden = chat.renders;
    assert_eq!(renders_while_hidden, renders_after_draft);

    app.dispatch(Action::Open(PanelKind::Presets), 40)
        .expect("open presets while chat hidden");
    assert_eq!(
        panel(&app.snapshot(), PanelKind::Chat).renders,
        renders_while_hidden
    );

    app.dispatch(Action::Open(PanelKind::Chat), 50)
        .expect("restore chat");
    let snapshot = app.snapshot();
    let chat = panel(&snapshot, PanelKind::Chat);
    assert_eq!(chat.visibility, PanelVisibility::Visible);
    assert_eq!(chat.mounts, 2);
    assert_eq!(chat.renders, renders_while_hidden + 1);
    assert_eq!(snapshot.chat.draft, "隐藏也要保留的草稿");
}

#[test]
fn invisible_and_unopened_panels_reject_input() {
    let mut app = app();
    let error = app
        .dispatch(Action::Send, 10)
        .expect_err("chat panel is not open yet");
    assert_eq!(error.public_code(), "panel_not_open");

    app.dispatch(Action::Open(PanelKind::Chat), 20)
        .expect("open chat");
    app.dispatch(Action::SetDraft(String::from("可见时输入")), 30)
        .expect("draft accepted");
    app.dispatch(Action::Hide(PanelKind::Chat), 40)
        .expect("hide chat");
    let error = app
        .dispatch(Action::SetDraft(String::from("隐藏后输入")), 50)
        .expect_err("hidden panel rejects input");
    assert_eq!(error.public_code(), "panel_not_visible");
    let error = app
        .dispatch(Action::Send, 60)
        .expect_err("hidden panel rejects send");
    assert_eq!(error.public_code(), "panel_not_visible");

    app.dispatch(Action::Open(PanelKind::Chat), 70)
        .expect("restore chat");
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.draft, "可见时输入");
    assert_eq!(snapshot.chat.messages.len(), 0);
}

#[test]
fn close_confirmation_cancel_stale_and_confirm() {
    let mut app = app();
    app.dispatch(Action::Open(PanelKind::Chat), 10)
        .expect("open chat");
    app.dispatch(Action::SetDraft(String::from("重要草稿")), 20)
        .expect("draft accepted");

    app.dispatch(Action::Close(PanelKind::Chat), 30)
        .expect("close with draft asks for confirmation");
    let snapshot = app.snapshot();
    assert!(panel(&snapshot, PanelKind::Chat).confirmation);
    assert_eq!(
        panel(&snapshot, PanelKind::Chat).visibility,
        PanelVisibility::Visible
    );

    app.dispatch(
        Action::ConfirmClose {
            panel: PanelKind::Chat,
            discard: false,
        },
        40,
    )
    .expect("cancel keeps the instance");
    let snapshot = app.snapshot();
    assert_eq!(
        panel(&snapshot, PanelKind::Chat).visibility,
        PanelVisibility::Visible
    );
    assert!(!panel(&snapshot, PanelKind::Chat).confirmation);
    assert_eq!(snapshot.chat.draft, "重要草稿");
    let error = app
        .dispatch(
            Action::ConfirmClose {
                panel: PanelKind::Chat,
                discard: true,
            },
            50,
        )
        .expect_err("cancelled credential is gone");
    assert_eq!(error.public_code(), "no_pending_confirmation");

    app.dispatch(Action::Close(PanelKind::Chat), 60)
        .expect("close asks again");
    app.dispatch(Action::SetDraft(String::from("重要草稿二号")), 70)
        .expect("draft changed after the close request");
    let error = app
        .dispatch(
            Action::ConfirmClose {
                panel: PanelKind::Chat,
                discard: true,
            },
            80,
        )
        .expect_err("stale confirmation rejected");
    assert_eq!(error.public_code(), "confirm_stale");
    let snapshot = app.snapshot();
    assert_eq!(
        panel(&snapshot, PanelKind::Chat).visibility,
        PanelVisibility::Visible
    );
    assert_eq!(snapshot.chat.draft, "重要草稿二号");

    app.dispatch(Action::Close(PanelKind::Chat), 90)
        .expect("close asks once more");
    app.dispatch(
        Action::ConfirmClose {
            panel: PanelKind::Chat,
            discard: true,
        },
        100,
    )
    .expect("confirmed close releases the instance");
    let snapshot = app.snapshot();
    assert_eq!(
        panel(&snapshot, PanelKind::Chat).visibility,
        PanelVisibility::Closed
    );
    assert!(!snapshot.chat.draft.contains("草稿"));

    app.dispatch(Action::Open(PanelKind::Chat), 110)
        .expect("reopen after release");
    let snapshot = app.snapshot();
    assert_eq!(panel(&snapshot, PanelKind::Chat).mounts, 1);
    assert_eq!(snapshot.chat.draft, "");
}

#[test]
fn revoke_clears_panels_and_blocks_everything() {
    let mut app = app();
    open_all(&mut app, 10);
    app.dispatch(Action::SetDraft(String::from("将被清理的草稿")), 20)
        .expect("draft accepted");
    app.dispatch(Action::Send, 30).expect("send accepted");
    app.dispatch(Action::Remember(String::from("将被清理的记忆")), 40)
        .expect("remember accepted");
    app.dispatch(Action::RefreshDiagnostics, 50)
        .expect("diagnostics refresh");
    assert!(!app.snapshot().diagnostics.is_empty());

    app.dispatch(Action::Revoke, 60).expect("revoke succeeds");
    let snapshot = app.snapshot();
    assert!(snapshot.revoked);
    assert!(
        snapshot
            .panels
            .iter()
            .all(|panel| panel.visibility == PanelVisibility::Closed),
        "revoke releases every panel"
    );
    assert!(snapshot.memories.is_empty(), "memory display cache cleared");
    assert!(snapshot.diagnostics.is_empty(), "diagnostics cache cleared");
    assert!(snapshot.chat.messages.is_empty(), "chat cache cleared");
    assert!(!snapshot.chat.draft.contains("清理"), "draft cache cleared");

    for (action, now_ms) in [
        (Action::Open(PanelKind::Chat), 70),
        (Action::Send, 80),
        (Action::Remember(String::from("再记一条")), 90),
        (Action::RefreshDiagnostics, 100),
        (Action::Revoke, 110),
    ] {
        let error = app
            .dispatch(action, now_ms)
            .expect_err("revoked authorization blocks every action");
        assert_eq!(error.public_code(), "revoked");
    }
    assert!(
        app.snapshot().revoked,
        "snapshot stays readable after revoke"
    );
}

#[test]
fn input_and_error_boundaries_are_rejected() {
    let error = match ChatApplication::new(0) {
        Ok(_) => panic!("zero process generation must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.public_code(), "invalid_process_generation");

    let mut app = app();
    let error = app.dispatch(Action::Send, 10).expect_err("chat not open");
    assert_eq!(error.public_code(), "panel_not_open");
    app.dispatch(Action::Open(PanelKind::Chat), 20)
        .expect("open chat");

    let error = app.dispatch(Action::Send, 30).expect_err("blank draft");
    assert_eq!(error.public_code(), "empty_input");

    let oversized_chars = "长".repeat(4_500);
    let error = app
        .dispatch(Action::SetDraft(oversized_chars), 40)
        .expect_err("over byte budget");
    assert_eq!(error.public_code(), "input_too_large");
    let error = app
        .dispatch(Action::SetDraft("a".repeat(8_001)), 50)
        .expect_err("ascii over byte budget");
    assert_eq!(error.public_code(), "input_too_large");

    app.dispatch(Action::SetDraft(String::from("   ")), 60)
        .expect("blank draft stored");
    let error = app.dispatch(Action::Send, 70).expect_err("blank send");
    assert_eq!(error.public_code(), "empty_input");

    app.dispatch(Action::Open(PanelKind::Memory), 80)
        .expect("open memory");
    let error = app
        .dispatch(Action::Remember(String::from("  ")), 90)
        .expect_err("blank remember");
    assert_eq!(error.public_code(), "empty_input");
    let error = app
        .dispatch(Action::Remember("a".repeat(4_097)), 100)
        .expect_err("over char budget for memory text");
    assert_eq!(error.public_code(), "input_too_large");

    app.dispatch(Action::Open(PanelKind::Presets), 110)
        .expect("open presets");
    let error = app
        .dispatch(Action::SelectPreset(String::from("nope")), 120)
        .expect_err("unknown preset");
    assert_eq!(error.public_code(), "unknown_preset");
    let error = app
        .dispatch(Action::SelectPreset(String::from("  ")), 130)
        .expect_err("blank preset id");
    assert_eq!(error.public_code(), "empty_input");
}

#[test]
fn clock_regression_fails_without_automatic_retry() {
    let mut app = app();
    app.dispatch(Action::Open(PanelKind::Chat), 1_000)
        .expect("open chat");
    app.dispatch(Action::SetDraft(String::from("第一轮")), 1_100)
        .expect("draft accepted");
    app.dispatch(Action::Send, 1_200).expect("first send");
    app.dispatch(Action::SetDraft(String::from("第二轮")), 1_300)
        .expect("draft accepted");

    let error = app
        .dispatch(Action::Send, 900)
        .expect_err("time regression rejected");
    assert_eq!(error.public_code(), "gateway_rejected");
    let mut source = Error::source(&error).map(|source| source as &dyn Error);
    let mut found_gateway_error = false;
    while let Some(current) = source {
        if let Some(gateway) = current.downcast_ref::<GatewayError>() {
            assert_eq!(*gateway, GatewayError::ClockWentBackwards);
            found_gateway_error = true;
        }
        source = current.source();
    }
    assert!(found_gateway_error, "original error chain is preserved");

    app.dispatch(Action::Open(PanelKind::Presets), 1_400)
        .expect("unrelated drive still works");
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.status, ChatStatus::Failed);
    assert_eq!(snapshot.chat.failure.as_deref(), Some("gateway_rejected"));
    assert_eq!(snapshot.chat.messages.len(), 2);

    app.dispatch(Action::SetDraft(String::from("重试一轮")), 1_500)
        .expect("draft accepted");
    app.dispatch(Action::Send, 1_600)
        .expect("explicit retry with fresh time succeeds");
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.status, ChatStatus::Ready);
    assert_eq!(snapshot.chat.messages.len(), 4);
}
