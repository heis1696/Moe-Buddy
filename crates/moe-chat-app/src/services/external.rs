use super::*;

impl ChatCore {
    pub(crate) fn enable_history(
        &mut self,
        mut store: Box<dyn crate::ChatHistory>,
        retention: moe_agent_runtime::RawRetention,
        now_ms: u64,
    ) -> Result<crate::HistoryRecovery, AppError> {
        self.ensure_active().map_err(AppError::not_executed)?;
        if self.admitted_events != 0 || self.history_store.is_some() {
            return Err(AppError::rejection(
                "history_already_started",
                "会话已开始，不能替换历史存储",
                "history initialization requires a fresh runtime",
            )
            .not_executed());
        }
        self.ingress
            .bind_stable_credential(self.credential, "daily-desktop-owner")
            .map_err(AppError::from_gateway)?;
        let directory = store.open_directory()?;
        if let Some(directory) = &directory {
            self.active_logical_session_id = directory.active_session_id.clone();
            self.active_session_hint = directory.active_storage_hint.clone();
        }
        self.sessions = SessionRuntime::with_retention(
            &self.ingress,
            SessionLimits::new(
                SESSION_FRAGMENTS_BUDGET,
                SESSION_EVENTS_BUDGET,
                256,
                8192,
                SESSION_CHARS_BUDGET,
                2,
            )
            .map_err(AppError::from_gateway)?,
            SessionTiming::default(),
            retention,
            now_ms,
        );
        let request = self.admit("history-access")?;
        let domain = moe_agent_runtime::RawDomainKey::from_request(&self.ingress, &request)
            .map_err(AppError::from_gateway)?;
        let grant = self
            .sessions
            .install_raw_history_grant(&self.ingress, domain)
            .map_err(AppError::from_gateway)?;
        let mut history_requests = Vec::new();
        if let Some(directory) = &directory {
            history_requests.reserve(directory.sessions.len());
            for session in &directory.sessions {
                let (session_request, session_grant) =
                    if session.id == self.active_logical_session_id {
                        (request.clone(), grant.clone())
                    } else {
                        let hint = store.session_storage_hint(&session.id)?;
                        let session_request = self.admit_with_hint("history-recovery", hint)?;
                        let domain = moe_agent_runtime::RawDomainKey::from_request(
                            &self.ingress,
                            &session_request,
                        )
                        .map_err(AppError::from_gateway)?;
                        let session_grant = self
                            .sessions
                            .install_raw_history_grant(&self.ingress, domain)
                            .map_err(AppError::from_gateway)?;
                        (session_request, session_grant)
                    };
                history_requests.push(crate::HistoryRequest {
                    logical_session_id: session.id.clone(),
                    request: session_request,
                    grant: session_grant,
                });
            }
        }
        let events = store.open(
            crate::HistoryAccess {
                runtime: &mut self.sessions,
                ingress: &self.ingress,
                request: &request,
                grant: &grant,
                retention: &retention,
                now_ms,
                logical_session_id: &self.active_logical_session_id,
                status: self.chat_history.status,
                online: self.online && self.transmission_allowed,
                external_request_pending: self.pending_external.is_some(),
                external_unknown: self.external_unknown_evidence,
            },
            &history_requests,
            directory.as_ref().map_or(0, |item| item.sessions_revision),
        )?;
        self.active_logical_session_id = events.active_session_id.clone();
        self.active_session_hint = events.active_storage_hint.clone();
        self.external_unknown_evidence = events.active_external_unknown;
        self.recovered_unsettled_evidence = events.active_recovered_unsettled;
        let status = events
            .sessions
            .iter()
            .find(|session| session.id == events.active_session_id)
            .map_or(ChatStatus::Ready, |session| session.status);
        self.chat_history = ChatSnapshot {
            draft: String::new(),
            messages: events.active_history.clone(),
            status,
            failure: (status == ChatStatus::Unknown).then(|| String::from("send_unknown")),
        };
        let recovery = events;
        self.diagnostics_history_gap = true;
        self.last_storage_receipts = recovery.receipts.clone();
        self.history_store = Some((store, request, grant, retention));
        Ok(recovery)
    }

    pub(crate) fn checkpoint_history(
        &mut self,
        now_ms: u64,
        sessions_revision: u64,
    ) -> Result<(), AppError> {
        if self.history_store.is_none() {
            return Ok(());
        }
        let maintenance = match self.maintain_retention(now_ms, sessions_revision, 0) {
            Ok(report) => report,
            Err(error) => {
                self.mark_storage_unknown();
                return Err(error);
            }
        };
        if maintenance.is_some_and(|report| report.live_checkpointed) {
            return Ok(());
        }
        let (store, request, grant, retention) = self
            .history_store
            .as_mut()
            .expect("history store checked above");
        let result = store.checkpoint(
            crate::HistoryAccess {
                runtime: &mut self.sessions,
                ingress: &self.ingress,
                request,
                grant,
                retention,
                now_ms,
                logical_session_id: &self.active_logical_session_id,
                status: self.chat_history.status,
                online: self.online && self.transmission_allowed,
                external_request_pending: self.pending_external.is_some(),
                external_unknown: self.external_unknown_evidence,
            },
            sessions_revision,
        );
        match result {
            Ok(receipts) => {
                self.last_storage_receipts = receipts;
                Ok(())
            }
            Err(error) => {
                if let Some((store, _, _, _)) = self.history_store.as_ref() {
                    self.last_storage_receipts = store.last_commit_receipts();
                }
                if error.execution_disposition() == crate::ExecutionDisposition::Unknown {
                    self.mark_storage_unknown();
                }
                Err(error)
            }
        }
    }

    pub(crate) fn configure_model(
        &mut self,
        settings: &crate::ModelSettings,
    ) -> Result<(), AppError> {
        let model_version = self.settings_version.checked_add(1).ok_or_else(|| {
            AppError::rejection(
                "settings_full",
                "设置版本已耗尽",
                "settings version overflow",
            )
            .not_executed()
        })?;
        let persona_version = if settings.persona == self.persona_text {
            self.persona_version
        } else {
            self.persona_version.checked_add(1).ok_or_else(|| {
                AppError::rejection(
                    "settings_full",
                    "人设版本已耗尽",
                    "persona version overflow",
                )
                .not_executed()
            })?
        };
        self.configure_model_binding(settings, model_version, persona_version, "daily-chat")
    }

    pub(crate) fn configure_model_binding(
        &mut self,
        settings: &crate::ModelSettings,
        model_version: u64,
        persona_version: u64,
        preset_id: &str,
    ) -> Result<(), AppError> {
        self.preflight_model_binding(settings, model_version, persona_version, preset_id)?;
        self.ensure_active().map_err(AppError::not_executed)?;
        if self.pending_external.is_some() {
            return Err(AppError::rejection(
                "request_pending",
                "请等待当前请求结束再修改设置",
                "pending external request",
            )
            .not_executed());
        }
        if model_version <= self.settings_version
            || model_version > crate::MAX_SAFE_INTEGER
            || persona_version == 0
            || persona_version > crate::MAX_SAFE_INTEGER
            || (settings.persona == self.persona_text && persona_version != self.persona_version)
            || (settings.persona != self.persona_text && persona_version <= self.persona_version)
        {
            return Err(AppError::rejection(
                "stale_buddy_configuration",
                "伙伴配置版本已变化",
                "buddy configuration versions are stale",
            )
            .not_executed());
        }
        if !crate::buddy::valid_persona(&settings.persona) {
            return Err(AppError::rejection(
                "invalid_persona",
                "人设不能为空且最多 4096 字",
                "invalid persona",
            )
            .not_executed());
        }
        let config = PresetConfig::new(
            "我的模型",
            "openai-compatible",
            &settings.endpoint,
            None,
            &settings.model,
            BTreeMap::new(),
        )
        .map_err(|error| AppError::from_gateway(error).not_executed())?;
        let id = PresetId::new(preset_id)
            .map_err(|error| AppError::from_gateway(error).not_executed())?;
        let mut registry =
            PresetRegistry::new(4).map_err(|error| AppError::from_gateway(error).not_executed())?;
        registry
            .create(id.clone(), config)
            .map_err(|error| AppError::from_gateway(error).not_executed())?;
        registry
            .bind(PresetPurpose::Chat, id.clone())
            .map_err(|error| AppError::from_gateway(error).not_executed())?;
        if settings.persona != self.persona_text {
            self.memory.set_identity_view(
                &self.identity,
                persona_version,
                &settings.persona,
                vec![self.persona_source.clone()],
            )?;
        }
        let sources = self.grant_sources();
        let policy = if settings.allow_transmission {
            ReadPolicy::model_transmission(model_version, sources)
        } else {
            ReadPolicy::offline(model_version, sources)
        };
        self.memory
            .set_read_policy(&self.identity, policy)
            .map_err(|error| {
                error.with_execution_disposition(crate::ExecutionDisposition::Unknown)
            })?;
        self.settings_version = model_version;
        self.persona_version = persona_version;
        self.persona_text = settings.persona.clone();
        self.presets = registry;
        self.preset_ids = vec![id];
        self.preset_catalog = vec![PresetSnapshot {
            id: preset_id.to_owned(),
            label: "我的模型".into(),
            provider: "openai-compatible".into(),
            endpoint: settings.endpoint.clone(),
            model: settings.model.clone(),
        }];
        self.online = true;
        self.transmission_allowed = settings.allow_transmission;
        Ok(())
    }

    pub(crate) fn preflight_model_binding(
        &self,
        settings: &crate::ModelSettings,
        model_version: u64,
        persona_version: u64,
        preset_id: &str,
    ) -> Result<(), AppError> {
        self.ensure_active().map_err(AppError::not_executed)?;
        if self.storage_unknown {
            return Err(AppError::rejection(
                "storage_failed",
                "存储结果未确认，当前配置暂不可修改",
                "storage outcome is unresolved",
            )
            .not_executed());
        }
        if self.pending_external.is_some() {
            return Err(AppError::rejection(
                "request_pending",
                "请等待当前请求结束再修改设置",
                "pending external request",
            )
            .not_executed());
        }
        if model_version <= self.settings_version
            || model_version > crate::MAX_SAFE_INTEGER
            || persona_version == 0
            || persona_version > crate::MAX_SAFE_INTEGER
            || (settings.persona == self.persona_text && persona_version != self.persona_version)
            || (settings.persona != self.persona_text && persona_version <= self.persona_version)
        {
            return Err(AppError::rejection(
                "stale_buddy_configuration",
                "伙伴配置版本已变化",
                "buddy configuration versions are stale",
            )
            .not_executed());
        }
        if !crate::buddy::valid_persona(&settings.persona) {
            return Err(AppError::rejection(
                "invalid_persona",
                "人设不能为空且最多 4096 字",
                "invalid persona",
            )
            .not_executed());
        }
        let config = PresetConfig::new(
            "我的模型",
            "openai-compatible",
            &settings.endpoint,
            None,
            &settings.model,
            BTreeMap::new(),
        )
        .map_err(|error| AppError::from_gateway(error).not_executed())?;
        let id = PresetId::new(preset_id)
            .map_err(|error| AppError::from_gateway(error).not_executed())?;
        let mut registry =
            PresetRegistry::new(4).map_err(|error| AppError::from_gateway(error).not_executed())?;
        registry
            .create(id.clone(), config)
            .map_err(|error| AppError::from_gateway(error).not_executed())?;
        registry
            .bind(PresetPurpose::Chat, id)
            .map_err(|error| AppError::from_gateway(error).not_executed())?;
        Ok(())
    }

    pub(super) fn begin_external(&mut self, text: &str, now_ms: u64) -> Result<(), AppError> {
        if !self.transmission_allowed {
            return Err(AppError::rejection(
                "transmission_denied",
                "请在模型设置中明确允许发送内容",
                "model transmission denied",
            )
            .not_executed());
        }
        if self.pending_publish.is_some() {
            return Err(AppError::rejection(
                "request_pending",
                "上一轮已提交，正在等待展示发布结果",
                "a committed turn still awaits its display publication result",
            )
            .not_executed());
        }
        if self.pending_external.is_some() || self.chat_history.status == ChatStatus::Unknown {
            return Err(AppError::rejection(
                "send_unknown",
                "已有请求进行中或结果未知，请勿重发",
                "external admission blocked",
            )
            .not_executed());
        }
        if self.admitted_events + 2 > SESSION_EVENTS_BUDGET
            || self.admitted_chars + text.chars().count() + REPLY_CHARS_BUDGET
                > SESSION_CHARS_BUDGET
        {
            return Err(AppError::rejection(
                "conversation_full",
                "本次会话已满",
                "external conversation capacity",
            )
            .not_executed());
        }
        let mut turn = self.start_turn(text)?;
        if let Err(error) = self.admission_phase(&mut turn, text, now_ms) {
            self.record_progress(&turn);
            if turn.is_registered() {
                return Err(self.finish_registered_failure(&mut turn, error, now_ms));
            }
            self.chat_history.status = ChatStatus::Failed;
            self.chat_history.failure = Some(error.public_code().to_owned());
            return Err(error);
        }
        let prepared = (|| {
            self.prepare_input_phase(&mut turn, true, now_ms)?;
            self.external_request_phase(&mut turn, now_ms)
        })();
        match prepared {
            Ok(input) => {
                self.record_progress(&turn);
                self.push_message(ChatMessage {
                    role: MessageRole::User,
                    text: text.to_owned(),
                });
                self.chat_history.status = ChatStatus::Sending;
                self.chat_history.failure = None;
                turn.user_visible = true;
                self.external_input = Some(input);
                self.pending_external = Some(turn);
                Ok(())
            }
            Err(primary) => {
                self.record_progress(&turn);
                let error = self.finish_registered_failure(&mut turn, primary, now_ms);
                Err(error)
            }
        }
    }
}
