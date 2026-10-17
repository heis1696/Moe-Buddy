use moe_chat_app::{Action, ChatApplication, PanelKind};

#[test]
fn edit_search_delete_and_next_turn_use_authoritative_memory() {
    let mut app = ChatApplication::new(901).unwrap();
    app.dispatch(Action::Open(PanelKind::Memory), 0).unwrap();
    app.dispatch(Action::Remember("喜欢红茶".into()), 1)
        .unwrap();
    app.dispatch(Action::Remember("周末去散步".into()), 2)
        .unwrap();
    app.dispatch(Action::SearchMemories("红茶".into()), 3)
        .unwrap();
    let note = app.snapshot().memories[0].clone();
    assert_eq!(app.snapshot().memories.len(), 1);
    app.dispatch(
        Action::UpdateMemory {
            id: note.id.clone(),
            revision: note.revision,
            text: "更喜欢绿茶".into(),
        },
        4,
    )
    .unwrap();
    assert!(
        app.dispatch(
            Action::UpdateMemory {
                id: note.id.clone(),
                revision: note.revision,
                text: "过期修改".into()
            },
            5
        )
        .is_err()
    );
    app.dispatch(Action::SearchMemories("绿茶".into()), 6)
        .unwrap();
    let updated = app.snapshot().memories[0].clone();
    assert_eq!(updated.revision, note.revision + 1);
    assert_eq!(updated.text, "更喜欢绿茶");
    assert!(
        app.dispatch(
            Action::DeleteMemory {
                id: note.id,
                revision: note.revision
            },
            7
        )
        .is_err()
    );
    app.dispatch(
        Action::DeleteMemory {
            id: updated.id,
            revision: updated.revision,
        },
        8,
    )
    .unwrap();
    app.dispatch(Action::SearchMemories("绿茶".into()), 9)
        .unwrap();
    assert!(app.snapshot().memories.is_empty());
    app.dispatch(Action::Open(PanelKind::Chat), 10).unwrap();
    app.dispatch(Action::SetDraft("记得什么？".into()), 11)
        .unwrap();
    app.dispatch(Action::Send, 12).unwrap();
    let reply = &app.snapshot().chat.messages.last().unwrap().text.clone();
    assert!(reply.contains("散步"));
    assert!(!reply.contains("绿茶"));
}

#[test]
fn memory_commands_recheck_visibility_and_revocation() {
    let mut app = ChatApplication::new(902).unwrap();
    assert!(app.dispatch(Action::SearchMemories("".into()), 0).is_err());
    app.dispatch(Action::Open(PanelKind::Memory), 1).unwrap();
    app.dispatch(Action::Remember("私密记录".into()), 2)
        .unwrap();
    let note = app.snapshot().memories[0].clone();
    app.dispatch(Action::Hide(PanelKind::Memory), 3).unwrap();
    assert!(
        app.dispatch(
            Action::DeleteMemory {
                id: note.id.clone(),
                revision: note.revision
            },
            4
        )
        .is_err()
    );
    app.dispatch(Action::Open(PanelKind::Memory), 5).unwrap();
    app.dispatch(Action::Revoke, 6).unwrap();
    assert!(
        app.dispatch(
            Action::UpdateMemory {
                id: note.id,
                revision: note.revision,
                text: "修改".into()
            },
            7
        )
        .is_err()
    );
    assert!(app.snapshot().memories.is_empty());
}
