//! LocalMemory is the authoritative journal; no alternate note snapshot format.
use moe_chat_app::{AppError, ChatMemory};
use moe_memory_core::*;
use moe_memory_local::{LocalMemory, OperationId};

pub struct DiskMemory(pub LocalMemory);

pub struct DiskHistory {
    pub root: std::path::PathBuf,
    pub store: Option<moe_raw_local::RawLocalStore>,
}

impl moe_chat_app::ChatHistory for DiskHistory {
    fn open(
        &mut self,
        access: moe_chat_app::HistoryAccess<'_>,
    ) -> Result<Vec<moe_agent_runtime::StagedEvent>, AppError> {
        use moe_agent_runtime::*;
        use moe_raw_local::*;
        let domain = RawDomainKey::from_request(access.ingress, access.request)
            .map_err(AppError::from_storage)?;
        let mode = if self.root.exists() {
            OpenMode::OpenExisting
        } else {
            OpenMode::CreateNew
        };
        let mut store = RawLocalStore::open(
            &self.root,
            mode,
            RawLocalAccess::new(
                access.runtime,
                access.ingress,
                access.request,
                access.grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?,
        )
        .map_err(AppError::from_storage)?
        .store;
        let mut events = Vec::new();
        if mode == OpenMode::OpenExisting {
            let revision = store.revision();
            let recovery = store
                .restore(
                    RawLocalAccess::new(
                        access.runtime,
                        access.ingress,
                        access.request,
                        access.grant,
                        domain,
                        access.now_ms,
                    )
                    .map_err(AppError::from_storage)?,
                    access.retention,
                    revision,
                )
                .map_err(AppError::from_storage)?;
            for fragment in recovery.import.fragments {
                if fragment.zone != RetentionZone::Normal {
                    continue;
                }
                let capability = access
                    .runtime
                    .install_management_capability(
                        access.ingress,
                        access.request.identity().clone(),
                        access.request.isolation().clone(),
                        vec![fragment.session.clone()],
                        ManagementPermissions::default().with_view(),
                    )
                    .map_err(AppError::from_storage)?;
                let snapshot = access
                    .runtime
                    .inspect_fragment(
                        SessionAccess::new(access.ingress, access.request, access.now_ms),
                        &capability,
                        &fragment.session,
                    )
                    .map_err(AppError::from_storage)?;
                events.extend(snapshot.events);
            }
        }
        events.sort_by_key(|event| event.received_ms());
        self.store = Some(store);
        Ok(events)
    }

    fn checkpoint(&mut self, access: moe_chat_app::HistoryAccess<'_>) -> Result<(), AppError> {
        let store = self.store.as_mut().ok_or_else(|| {
            AppError::from_storage(std::io::Error::other("history store not opened"))
        })?;
        let revision = store.revision();
        let domain = moe_agent_runtime::RawDomainKey::from_request(access.ingress, access.request)
            .map_err(AppError::from_storage)?;
        store
            .checkpoint(
                moe_raw_local::RawLocalAccess::new(
                    access.runtime,
                    access.ingress,
                    access.request,
                    access.grant,
                    domain,
                    access.now_ms,
                )
                .map_err(AppError::from_storage)?,
                revision,
            )
            .map_err(AppError::from_storage)?;
        Ok(())
    }
}

pub fn root() -> Result<std::path::PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|path| {
            std::path::PathBuf::from(path)
                .join("MoeBuddy")
                .join("daily")
        })
        .ok_or_else(|| "无法确定当前用户的本地保存目录".into())
}

pub fn load_settings() -> Result<Option<moe_chat_app::ModelSettings>, String> {
    if root()?.join("settings.pending").exists() {
        return Err("检测到未完成的设置保存，已保留现场；请核对 settings.pending".into());
    }
    let path = root()?.join("settings.json");
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "无法读取模型设置")?;
    if metadata.len() > 32_768 {
        return Err("模型设置文件超过上限".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "无法读取模型设置")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "模型设置已损坏，文件已保留")?;
    let field = |name| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "模型设置格式错误".to_owned())
    };
    Ok(Some(moe_chat_app::ModelSettings {
        endpoint: field("endpoint")?,
        model: field("model")?,
        persona: field("persona")?,
        allow_transmission: false,
    }))
}

pub fn save_settings(settings: &moe_chat_app::ModelSettings) -> Result<(), String> {
    use std::io::Write;
    let root = root()?;
    let pending = root.join("settings.pending");
    let bytes = serde_json::json!({"version":1,"endpoint":settings.endpoint,"model":settings.model,"persona":settings.persona}).to_string();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|_| "设置未保存：临时文件存在或目录不可写")?;
    file.write_all(bytes.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "设置保存结果未知，保留现场")?;
    drop(file);
    std::fs::rename(pending, root.join("settings.json")).map_err(|_| "设置未发布，保留临时文件")?;
    Ok(())
}

pub fn open_application(
    generation: u64,
    now: u64,
) -> Result<moe_chat_app::ChatApplication, String> {
    let root = root()?;
    let mut app = open_at(&root, generation, now)?;
    if let Some(settings) = load_settings()? {
        app.configure_model(&settings)
            .map_err(|error| error.public_message().to_owned())?;
    }
    Ok(app)
}

fn open_at(
    root: &std::path::Path,
    generation: u64,
    now: u64,
) -> Result<moe_chat_app::ChatApplication, String> {
    std::fs::create_dir_all(root).map_err(|_| "无法创建本地保存目录")?;
    let path = root.join("memory.v1");
    let memory = if path.exists() {
        LocalMemory::open(&path)
    } else {
        LocalMemory::create(&path)
    }
    .map_err(|_| "私密记忆无法打开，原存档已保留；请检查文件与占用")?;
    let mut app =
        moe_chat_app::ChatApplication::with_memory(generation, Box::new(DiskMemory(memory)))
            .map_err(|error| error.public_message().to_owned())?;
    let retention =
        moe_agent_runtime::RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536)
            .map_err(|_| "无法建立聊天保存策略")?;
    app.enable_history(
        Box::new(DiskHistory {
            root: root.join("chat"),
            store: None,
        }),
        retention,
        now,
    )
    .map_err(|error| error.public_message().to_owned())?;
    Ok(app)
}

impl DiskMemory {
    fn operation() -> Result<OperationId, AppError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(AppError::from_storage)?;
        OperationId::new(format!("daily-{:032x}", u128::from_le_bytes(bytes)))
            .map_err(AppError::from_storage)
    }
}
impl MemoryContextAccess for DiskMemory {
    type Error = AppError;
    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<MemoryTask, AppError> {
        self.0
            .begin_memory_task(identity, session, purpose, query, now)
            .map_err(AppError::from_storage)
    }
    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget: usize,
        now: u64,
    ) -> Result<RequestContext, AppError> {
        self.0
            .assemble_context(task, budget, now)
            .map_err(AppError::from_storage)
    }
    fn context_view<'a>(
        &mut self,
        context: &'a RequestContext,
        now: u64,
    ) -> Result<&'a ContextView, AppError> {
        self.0
            .context_view(context, now)
            .map_err(AppError::from_storage)
    }
}
impl ChatMemory for DiskMemory {
    fn register_source(
        &mut self,
        scope: MemoryScope,
        name: &str,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<SourceRef, AppError> {
        self.0
            .register_source(
                OperationId::new(format!("daily-source-{name}")).map_err(AppError::from_storage)?,
                scope,
                name,
                revision,
                epistemic,
            )
            .map(|receipt| receipt.value().clone())
            .map_err(AppError::from_storage)
    }
    fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: &str,
        sources: Vec<SourceRef>,
    ) -> Result<(), AppError> {
        self.0
            .set_identity_view(identity, version, text, sources)
            .map_err(AppError::from_storage)
    }
    fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), AppError> {
        self.0
            .set_read_policy(identity, policy)
            .map_err(AppError::from_storage)
    }
    fn create(
        &mut self,
        identity: &PetIdentity,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .create(Self::operation()?, draft, now)
            .map(|receipt| receipt.value().clone())
            .map_err(AppError::from_storage)
    }
    fn update(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .update(Self::operation()?, id, revision, draft, now)
            .map(|receipt| receipt.value().clone())
            .map_err(AppError::from_storage)
    }
    fn delete(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .delete(Self::operation()?, id, revision, now)
            .map(|receipt| receipt.value().clone())
            .map_err(AppError::from_storage)
    }
    fn retrieve(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<RetrievalResult, AppError> {
        self.0
            .for_session(identity, session, ReadPurpose::OfflineReasoning)
            .retrieve(query, now)
            .map_err(AppError::from_storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moe_chat_app::{Action, ChatStatus, ModelSettings, PanelKind};
    fn directory() -> std::path::PathBuf {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).unwrap();
        std::env::temp_dir().join(format!(
            "buddy-synthetic-{:032x}",
            u128::from_le_bytes(bytes)
        ))
    }
    #[test]
    fn journal_and_raw_history_reopen_without_replaying_requests() {
        let root = directory();
        {
            let mut app = open_at(&root, 51, 1000).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 1000).unwrap();
            app.dispatch(Action::Open(PanelKind::Memory), 1000).unwrap();
            app.dispatch(Action::Remember("synthetic blue tea".into()), 1001)
                .unwrap();
            app.dispatch(Action::SetDraft("synthetic hello".into()), 1002)
                .unwrap();
            app.dispatch(Action::Send, 1003).unwrap();
        }
        {
            let mut app = open_at(&root, 52, 2000).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 2000).unwrap();
            app.dispatch(Action::Open(PanelKind::Memory), 2000).unwrap();
            app.dispatch(Action::RefreshMemories, 2000).unwrap();
            let snapshot = app.snapshot();
            assert_eq!(snapshot.chat.messages.len(), 2);
            assert_eq!(snapshot.memories.len(), 1);
            let note = &snapshot.memories[0];
            app.dispatch(
                Action::UpdateMemory {
                    id: note.id.clone(),
                    revision: note.revision,
                    text: "synthetic green tea".into(),
                },
                2001,
            )
            .unwrap();
            app.configure_model(&ModelSettings {
                endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
                model: "synthetic".into(),
                persona: "synthetic persona".into(),
                allow_transmission: true,
            })
            .unwrap();
            app.dispatch(Action::SetDraft("pending synthetic request".into()), 2002)
                .unwrap();
            app.dispatch(Action::Send, 2003).unwrap();
            let input = app.take_external_request().unwrap();
            assert!(input.memory().iter().any(|text| text.contains("green tea")));
        }
        {
            let mut app = open_at(&root, 53, 3000).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 3000).unwrap();
            assert!(app.take_external_request().is_none());
            assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
            app.dispatch(Action::SetDraft("must not replay".into()), 3001)
                .unwrap();
            assert!(app.dispatch(Action::Send, 3002).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
