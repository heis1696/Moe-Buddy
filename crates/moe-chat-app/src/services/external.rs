use super::*;

impl ChatCore {
    pub(crate) fn enable_history(
        &mut self,
        mut store: Box<dyn crate::ChatHistory>,
        retention: moe_agent_runtime::RawRetention,
        now_ms: u64,
    ) -> Result<(), AppError> {
        self.ensure_active()?;
        if self.admitted_events != 0 || self.history_store.is_some() {
            return Err(AppError::rejection(
                "history_already_started",
                "会话已开始，不能替换历史存储",
                "history initialization requires a fresh runtime",
            ));
        }
        self.ingress
            .bind_stable_credential(self.credential, "daily-desktop-owner")
            .map_err(AppError::from_gateway)?;
        self.sessions = SessionRuntime::with_retention(
            &self.ingress,
            SessionLimits::new(8, SESSION_EVENTS_BUDGET, 256, 8192, SESSION_CHARS_BUDGET, 2)
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
        let events = store.open(crate::HistoryAccess {
            runtime: &mut self.sessions,
            ingress: &self.ingress,
            request: &request,
            grant: &grant,
            retention: &retention,
            now_ms,
        })?;
        for event in events {
            let role = match event.role() {
                moe_agent_runtime::EventRole::User => MessageRole::User,
                moe_agent_runtime::EventRole::Assistant => MessageRole::Assistant,
                _ => continue,
            };
            if role == MessageRole::User
                && matches!(
                    event.terminal(),
                    None | Some(TerminalState::Unknown | TerminalState::Cancelled)
                )
            {
                self.chat_history.status = ChatStatus::Unknown;
                self.chat_history.failure = Some("send_unknown".into());
            }
            self.chat_history.messages.push(ChatMessage {
                role,
                text: event.body().to_owned(),
            });
        }
        if self.chat_history.messages.len() > 128 {
            let excess = self.chat_history.messages.len() - 128;
            self.chat_history.messages.drain(..excess);
        }
        self.history_store = Some((store, request, grant, retention));
        Ok(())
    }

    pub(crate) fn checkpoint_history(&mut self, now_ms: u64) -> Result<(), AppError> {
        let Some((store, request, grant, retention)) = self.history_store.as_mut() else {
            return Ok(());
        };
        let result = store.checkpoint(crate::HistoryAccess {
            runtime: &mut self.sessions,
            ingress: &self.ingress,
            request,
            grant,
            retention,
            now_ms,
        });
        if result.is_err() {
            self.external_input = None;
            self.chat_history.status = ChatStatus::Unknown;
            self.chat_history.failure = Some("storage_failed".into());
        }
        result
    }

    pub(crate) fn configure_model(
        &mut self,
        settings: &crate::ModelSettings,
    ) -> Result<(), AppError> {
        self.ensure_active()?;
        if self.pending_external.is_some() {
            return Err(AppError::rejection(
                "request_pending",
                "请等待当前请求结束再修改设置",
                "pending external request",
            ));
        }
        if settings.persona.trim().is_empty() || settings.persona.chars().count() > 4096 {
            return Err(AppError::rejection(
                "invalid_persona",
                "人设不能为空且最多 4096 字",
                "invalid persona",
            ));
        }
        let config = PresetConfig::new(
            "我的模型",
            "openai-compatible",
            &settings.endpoint,
            None,
            &settings.model,
            BTreeMap::new(),
        )
        .map_err(AppError::from_gateway)?;
        let id = PresetId::new("daily-chat").map_err(AppError::from_gateway)?;
        let mut registry = PresetRegistry::new(4).map_err(AppError::from_gateway)?;
        registry
            .create(id.clone(), config)
            .map_err(AppError::from_gateway)?;
        registry
            .bind(PresetPurpose::Chat, id.clone())
            .map_err(AppError::from_gateway)?;
        let version = self.settings_version.checked_add(1).ok_or_else(|| {
            AppError::rejection(
                "settings_full",
                "设置版本已耗尽",
                "settings version overflow",
            )
        })?;
        self.memory
            .set_identity_view(
                &self.identity,
                version,
                &settings.persona,
                vec![self.persona_source.clone()],
            )
            .map_err(AppError::from_memory)?;
        let policy = if settings.allow_transmission {
            ReadPolicy::model_transmission(version, self.grant_sources())
        } else {
            ReadPolicy::offline(version, self.grant_sources())
        };
        self.memory
            .set_read_policy(&self.identity, policy)
            .map_err(AppError::from_memory)?;
        self.settings_version = version;
        self.presets = registry;
        self.preset_ids = vec![id];
        self.preset_catalog = vec![PresetSnapshot {
            id: "daily-chat".into(),
            label: "我的模型".into(),
            provider: "openai-compatible".into(),
            endpoint: settings.endpoint.clone(),
            model: settings.model.clone(),
        }];
        self.online = true;
        self.transmission_allowed = settings.allow_transmission;
        Ok(())
    }

    pub(super) fn begin_external(&mut self, text: &str, now_ms: u64) -> Result<(), AppError> {
        if !self.transmission_allowed {
            return Err(AppError::rejection(
                "transmission_denied",
                "请在模型设置中明确允许发送内容",
                "model transmission denied",
            ));
        }
        if self.pending_external.is_some() || self.chat_history.status == ChatStatus::Unknown {
            return Err(AppError::rejection(
                "send_unknown",
                "已有请求进行中或结果未知，请勿重发",
                "external admission blocked",
            ));
        }
        if self.admitted_events + 2 > SESSION_EVENTS_BUDGET
            || self.admitted_chars + text.chars().count() + REPLY_CHARS_BUDGET
                > SESSION_CHARS_BUDGET
        {
            return Err(AppError::rejection(
                "conversation_full",
                "本次会话已满",
                "external conversation capacity",
            ));
        }
        let request = self.admit(text)?;
        let registration = self
            .sessions
            .register_user(
                SessionAccess::new(&self.ingress, &request, now_ms),
                UserEvent::new(UserEventKind::New),
            )
            .map_err(AppError::from_gateway)?;
        self.admitted_events += 2;
        self.admitted_chars += text.chars().count() + REPLY_CHARS_BUDGET;
        let prepared = (|| {
            let access = AgentAccess::new(&self.ingress, &request, &self.presets, now_ms);
            let binding = self
                .agent
                .bind_session(
                    &access,
                    &mut self.sessions,
                    &registration.session,
                    vec![self.persona_source.clone(), self.notes_source.clone()],
                )
                .map_err(AppError::from)?;
            let mut task = self
                .agent
                .prepare_external(
                    &access,
                    AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                    &binding,
                    &registration.interaction,
                    &RetrievalQuery::new(""),
                )
                .map_err(AppError::from)?;
            let input = self
                .agent
                .dispatch_external(
                    &access,
                    AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                    &mut task,
                )
                .map_err(AppError::from)?;
            Ok::<_, AppError>((task, input))
        })();
        let (task, input) = match prepared {
            Ok(value) => value,
            Err(error) => {
                return Err(self.settle_after_failure(
                    &request,
                    &registration,
                    error,
                    None,
                    now_ms,
                ));
            }
        };
        self.pending_external = Some((request, registration, task));
        self.external_input = Some(input);
        self.chat_history.messages.push(ChatMessage {
            role: MessageRole::User,
            text: text.to_owned(),
        });
        self.chat_history.status = ChatStatus::Sending;
        self.chat_history.failure = None;
        Ok(())
    }

    pub(crate) fn complete_external(
        &mut self,
        outcome: ModelOutcome,
        now_ms: u64,
    ) -> Result<(), AppError> {
        self.ensure_active()?;
        let (request, registration, mut task) = self.pending_external.take().ok_or_else(|| {
            AppError::rejection("no_request", "当前没有等待中的请求", "no external request")
        })?;
        self.external_input = None;
        let completed = (|| {
            let access = AgentAccess::new(&self.ingress, &request, &self.presets, now_ms);
            let result = self
                .agent
                .complete_external(
                    &access,
                    AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                    &mut task,
                    outcome,
                )
                .map_err(AppError::from)?;
            self.agent
                .finish(
                    &access,
                    AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                    &mut task,
                    &result,
                )
                .map_err(AppError::from)
        })();
        match completed {
            Ok(delivery) => {
                self.chat_history.status = match delivery.state() {
                    TerminalState::Completed => ChatStatus::Ready,
                    TerminalState::Unknown => ChatStatus::Unknown,
                    _ => ChatStatus::Failed,
                };
                self.chat_history.failure = match delivery.state() {
                    TerminalState::Completed => None,
                    TerminalState::Unknown => Some("send_unknown".into()),
                    _ => Some("send_failed".into()),
                };
                if let Some(text) = delivery.text() {
                    self.chat_history.messages.push(ChatMessage {
                        role: MessageRole::Assistant,
                        text: text.to_owned(),
                    });
                }
                Ok(())
            }
            Err(error) => {
                // Cancellation cannot prove that an already dispatched request did not arrive.
                let error = self.settle_after_failure(&request, &registration, error, None, now_ms);
                self.chat_history.status = ChatStatus::Unknown;
                self.chat_history.failure = Some("send_unknown".into());
                Err(error)
            }
        }
    }
}
