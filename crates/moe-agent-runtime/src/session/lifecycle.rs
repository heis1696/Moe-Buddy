//! G3：会话原文生命周期与本机回收区。
//!
//! 仅使用标准库与既有 G1 暂存结构；全部时间由可信宿主注入单调毫秒值，
//! 溢出与容量检查先行，失败不发布半更新。本模块不是磁盘格式、GUI 或
//! 远程 API；管理能力由受信宿主在本机主人校验后显式安装。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use moe_memory_core::PetIdentity;

use super::{SessionRuntime, StagedEvent};
use crate::{
    GatewayError, HostEventId, RequestIsolation, SessionAccess, SessionId, SessionLimits,
    SessionTiming, TrustedIngress,
};

const MAX_MANAGEMENT_CAPABILITIES: usize = 64;

/// 宿主必填的原文留存配置：两段留存时长与回收区三项容量全部为正值，
/// 没有内置 24 小时或 7 天默认值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawRetention {
    pub(super) normal_ms: u64,
    pub(super) recycle_ms: u64,
    pub(super) recycle_fragments: usize,
    pub(super) recycle_events: usize,
    pub(super) recycle_chars: usize,
}

impl RawRetention {
    pub fn new(
        normal_ms: u64,
        recycle_ms: u64,
        recycle_fragments: usize,
        recycle_events: usize,
        recycle_chars: usize,
    ) -> Result<Self, GatewayError> {
        if normal_ms == 0
            || recycle_ms == 0
            || recycle_fragments == 0
            || recycle_events == 0
            || recycle_chars == 0
        {
            return Err(GatewayError::InvalidInput(
                "retention durations and recycle capacities must be positive",
            ));
        }
        Ok(Self {
            normal_ms,
            recycle_ms,
            recycle_fragments,
            recycle_events,
            recycle_chars,
        })
    }

    pub fn normal_ms(&self) -> u64 {
        self.normal_ms
    }

    pub fn recycle_ms(&self) -> u64 {
        self.recycle_ms
    }

    pub fn recycle_fragments(&self) -> usize {
        self.recycle_fragments
    }

    pub fn recycle_events(&self) -> usize {
        self.recycle_events
    }

    pub fn recycle_chars(&self) -> usize {
        self.recycle_chars
    }
}

/// 片段原文所在区域；清理后的片段不再存在于任何区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionZone {
    Normal,
    Recycle,
}

pub(super) struct FragmentLifecycle {
    pub(super) zone: RetentionZone,
    pub(super) created_at_ms: u64,
    pub(super) normal_deadline_ms: u64,
    pub(super) hard_deadline_ms: u64,
    pub(super) final_deadline_ms: Option<u64>,
    pub(super) version: u64,
}

impl FragmentLifecycle {
    /// 生效最终截止：已固定的取 `min(固定最终截止, 硬截止)`；尚未固定的
    /// 按（收紧后的）原正常截止加当前回收时长推算并封顶硬截止——与快照
    /// 导入/净化共用同一有效生命周期算法，自然回收不按维护时刻重给窗口。
    /// 期限运算溢出即失败封闭，不用饱和值悄悄放宽期限。
    pub(super) fn logical_final_deadline(&self, recycle_ms: u64) -> Result<u64, GatewayError> {
        logical_final_deadline(
            self.final_deadline_ms,
            self.normal_deadline_ms,
            self.hard_deadline_ms,
            recycle_ms,
        )
    }

    /// 最终到期统一使用 `now >= deadline`：正常区取已固定的最终截止或按
    /// 原正常截止推算的有效最终截止，尚未固定时封顶硬截止；不依赖定时
    /// 维护即对管理读取生效。
    pub(super) fn final_expired(&self, now_ms: u64, recycle_ms: u64) -> Result<bool, GatewayError> {
        self.logical_final_deadline(recycle_ms)
            .map(|deadline| now_ms >= deadline)
    }
}

/// 快照与在线片段共用的有效最终截止推算；`fixed_final` 为 `None` 时按
/// 有效正常截止加当前回收时长推算并封顶有效硬截止，加法溢出即失败。
pub(super) fn logical_final_deadline(
    fixed_final: Option<u64>,
    effective_normal: u64,
    effective_hard: u64,
    recycle_ms: u64,
) -> Result<u64, GatewayError> {
    if let Some(final_deadline) = fixed_final {
        return Ok(final_deadline.min(effective_hard));
    }
    effective_normal
        .checked_add(recycle_ms)
        .map(|candidate| candidate.min(effective_hard))
        .ok_or(GatewayError::IdentifierExhausted)
}

pub(super) struct CleanedFragment {
    pub(super) isolation: RequestIsolation,
    pub(super) host_ids: Vec<HostEventId>,
    /// 已清理片段的稳定片段键撤销栅栏：即使片段事件没有宿主事件 ID，
    /// 后续导入携带同键"存活"片段的旧快照也会被首次导入裁决拒绝，
    /// 不能凭无 HostEventId 的旧档复活已撤销正文。
    pub(super) raw_keys: Vec<u64>,
    pub(super) highest_request: u64,
    pub(super) highest_source: Option<u64>,
}

pub(super) struct CapabilityRecord {
    pub(super) ingress_run: u64,
    pub(super) generation: u64,
    pub(super) identity: PetIdentity,
    pub(super) isolation: RequestIsolation,
    pub(super) fragments: Vec<SessionId>,
    pub(super) permissions: ManagementPermissions,
    pub(super) revoked: bool,
}

/// 独立本机管理权限集合，默认无任何权限；查看、软删除、恢复、清空分别裁决。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManagementPermissions {
    view: bool,
    soft_delete: bool,
    restore: bool,
    purge: bool,
}

impl ManagementPermissions {
    #[must_use]
    pub fn with_view(mut self) -> Self {
        self.view = true;
        self
    }

    #[must_use]
    pub fn with_soft_delete(mut self) -> Self {
        self.soft_delete = true;
        self
    }

    #[must_use]
    pub fn with_restore(mut self) -> Self {
        self.restore = true;
        self
    }

    #[must_use]
    pub fn with_purge(mut self) -> Self {
        self.purge = true;
        self
    }

    pub fn can_view(&self) -> bool {
        self.view
    }

    pub fn can_soft_delete(&self) -> bool {
        self.soft_delete
    }

    pub fn can_restore(&self) -> bool {
        self.restore
    }

    pub fn can_purge(&self) -> bool {
        self.purge
    }
}

/// 私有构造的管理能力句柄；只在本运行实例内有效，Debug 不含任何范围或身份。
#[derive(Clone)]
pub struct ManagementCapability {
    pub(super) owner: Arc<()>,
    pub(super) index: usize,
}

impl fmt::Debug for ManagementCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagementCapability(REDACTED)")
    }
}

/// 管理范围内的片段摘要；不含正文。`logically_expired` 表示正常区
/// 到期待转移，`final_expired` 表示已到最终截止、等待受信维护或清空。
#[derive(Clone, Debug)]
pub struct ManagedFragmentSummary {
    pub session: SessionId,
    pub isolation: RequestIsolation,
    pub zone: RetentionZone,
    pub lifecycle_version: u64,
    pub created_at_ms: u64,
    pub normal_deadline_ms: u64,
    pub final_deadline_ms: Option<u64>,
    pub processed_through: usize,
    pub event_count: usize,
    pub body_chars: usize,
    pub logically_expired: bool,
    pub final_expired: bool,
}

/// 查看操作返回的只读快照；正文经 `StagedEvent` 访问器仅供受信宿主读取，
/// Debug 输出保持脱敏。
#[derive(Clone, Debug)]
pub struct ManagedFragmentSnapshot {
    pub session: SessionId,
    pub isolation: RequestIsolation,
    pub zone: RetentionZone,
    pub lifecycle_version: u64,
    pub created_at_ms: u64,
    pub normal_deadline_ms: u64,
    pub final_deadline_ms: Option<u64>,
    pub processed_through: usize,
    pub logically_expired: bool,
    pub events: Vec<StagedEvent>,
}

/// 软删除或恢复后的生命周期回执。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleReceipt {
    pub session: SessionId,
    pub zone: RetentionZone,
    pub lifecycle_version: u64,
    pub normal_deadline_ms: u64,
    pub final_deadline_ms: Option<u64>,
}

/// 单片段清空回执。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurgeReceipt {
    pub session: SessionId,
    pub released_events: usize,
    pub released_chars: usize,
}

/// 回收区清空报告。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecycleEmptyReport {
    pub purged: usize,
    pub released_events: usize,
    pub released_chars: usize,
}

/// 维护报告：多片段处理没有全局事务，单项转换原子；`pending_transfer`
/// 表示回收区满额时的显式背压，`authorization_removed` 表示原片段
/// 授权代际落后于当前权威配置的撤权清理。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub transferred: usize,
    pub removed: usize,
    pub pending_transfer: usize,
    pub authorization_removed: usize,
    pub released_events: usize,
    pub released_chars: usize,
}

impl SessionRuntime {
    /// 显式启用原文留存的构造入口；未使用本入口时所有生命周期与
    /// 管理 API 返回 [`GatewayError::RetentionNotConfigured`]，
    /// 其余行为与 [`SessionRuntime::new`] 完全一致。
    pub fn with_retention(
        ingress: &TrustedIngress,
        limits: SessionLimits,
        timing: SessionTiming,
        retention: RawRetention,
        now_ms: u64,
    ) -> Self {
        let mut runtime = Self::new(ingress, limits, timing, now_ms);
        runtime.retention = Some(retention);
        runtime
    }

    pub fn retention(&self) -> Option<&RawRetention> {
        self.retention.as_ref()
    }

    fn management_index(&self, capability: &ManagementCapability) -> Result<usize, GatewayError> {
        if !Arc::ptr_eq(&self.owner, &capability.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        self.capabilities
            .get(capability.index)
            .ok_or(GatewayError::StaleHandle)?;
        Ok(capability.index)
    }

    /// 管理操作的公共授权链：留存已启用、当前权威入口配置有效且未撤权、
    /// 同一运行实例与协调器、能力在册且未撤销、未跨配置代际、身份与
    /// 隔离域精确匹配，最后按权限分别裁决。任何失败不改变状态。
    fn authorize(
        &self,
        access: &SessionAccess<'_>,
        capability: &ManagementCapability,
        permission: fn(&ManagementPermissions) -> bool,
    ) -> Result<usize, GatewayError> {
        if self.retention.is_none() {
            return Err(GatewayError::RetentionNotConfigured);
        }
        access.request.validate(access.ingress)?;
        if access.ingress.run_instance() != self.ingress_run {
            return Err(GatewayError::ForeignRequest);
        }
        if access.now_ms < self.last_now_ms {
            return Err(GatewayError::ClockWentBackwards);
        }
        let index = self.management_index(capability)?;
        let record = &self.capabilities[index];
        if record.ingress_run != access.ingress.run_instance() {
            return Err(GatewayError::ForeignRequest);
        }
        if record.revoked {
            return Err(GatewayError::Revoked);
        }
        if record.generation != access.request.generation() {
            return Err(GatewayError::VersionConflict);
        }
        if record.identity != *access.request.identity() {
            return Err(GatewayError::ForeignRequest);
        }
        if record.isolation != *access.request.isolation() {
            return Err(GatewayError::ForeignRequest);
        }
        if !permission(&record.permissions) {
            return Err(GatewayError::Unauthorized);
        }
        Ok(index)
    }

    fn managed_session_index(
        &self,
        record: &CapabilityRecord,
        session: &SessionId,
    ) -> Result<usize, GatewayError> {
        if !Arc::ptr_eq(&self.owner, &session.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        if !record.fragments.iter().any(|scoped| scoped == session) {
            return Err(GatewayError::ForeignRequest);
        }
        let index = self
            .sessions
            .iter()
            .position(|candidate| &candidate.id == session)
            .ok_or(GatewayError::StaleHandle)?;
        if self.sessions[index].isolation != record.isolation {
            return Err(GatewayError::ForeignRequest);
        }
        if self.sessions[index].authorization_generation != record.generation {
            return Err(GatewayError::Revoked);
        }
        Ok(index)
    }

    fn lifecycle(&self, index: usize) -> &FragmentLifecycle {
        self.sessions[index]
            .lifecycle
            .as_ref()
            .expect("retention-configured fragments carry lifecycle state")
    }

    fn next_lifecycle_generation(&self) -> Result<u64, GatewayError> {
        self.lifecycle_generation
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)
    }

    pub(super) fn recycle_fragment_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| session.zone() == RetentionZone::Recycle)
            .count()
    }

    fn lifecycle_receipt(&self, index: usize) -> LifecycleReceipt {
        let session = &self.sessions[index];
        let lifecycle = session.lifecycle.as_ref().expect("retention configured");
        LifecycleReceipt {
            session: session.id.clone(),
            zone: lifecycle.zone,
            lifecycle_version: lifecycle.version,
            normal_deadline_ms: lifecycle.normal_deadline_ms,
            final_deadline_ms: lifecycle.final_deadline_ms,
        }
    }

    /// 正常区 → 回收区。逻辑回收时间由调用方给出：自然到期取原正常截止，
    /// 显式软删除取删除时刻；首次进入回收区把最终截止固定为
    /// min(既有硬截止, 逻辑回收时间 + 回收时长)，此后不再重算。
    /// 全部检查通过后才发布转换；未终结交互强制置为 Cancelled，
    /// 在途批次释放，版本与修订推进使旧票据与旧任务失效。
    fn transfer_to_recycle(
        &mut self,
        index: usize,
        logical_recycle_ms: u64,
    ) -> Result<u64, GatewayError> {
        let retention = self.retention.as_ref().expect("retention configured");
        let candidate_final = logical_recycle_ms
            .checked_add(retention.recycle_ms)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let session = &self.sessions[index];
        let lifecycle = session.lifecycle.as_ref().expect("retention configured");
        let final_deadline_ms = lifecycle
            .final_deadline_ms
            .unwrap_or(candidate_final.min(lifecycle.hard_deadline_ms));
        let version = lifecycle
            .version
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let revision = session
            .revision
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let generation = self.next_lifecycle_generation()?;
        let events = session.events.len();
        let chars = session.char_count;
        if self.recycle_fragment_count() + 1 > retention.recycle_fragments
            || self
                .recycle_events
                .checked_add(events)
                .is_none_or(|total| total > retention.recycle_events)
            || self
                .recycle_chars
                .checked_add(chars)
                .is_none_or(|total| total > retention.recycle_chars)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        let session = &mut self.sessions[index];
        let lifecycle = session.lifecycle.as_mut().expect("retention configured");
        lifecycle.zone = RetentionZone::Recycle;
        lifecycle.final_deadline_ms = Some(final_deadline_ms);
        lifecycle.version = version;
        session.revision = revision;
        session.active_until_ms = None;
        session.batch = None;
        for event in &mut session.events {
            if event.interaction.is_some() && event.staged.terminal.is_none() {
                event.staged.terminal = Some(crate::TerminalState::Cancelled);
            }
        }
        self.events_total -= events;
        self.chars_total -= chars;
        self.recycle_events += events;
        self.recycle_chars += chars;
        self.lifecycle_generation = generation;
        Ok(final_deadline_ms)
    }

    /// 清理正文并释放容量，保留有界请求序号、隔离域顺序水位、宿主事件
    /// 栅栏与稳定片段键栅栏。栅栏超出累计预算时仍完成清理，但永久拒绝
    /// 新用户/关联事件接纳，且账本不再增长。单调 ID 不回退、不复用；
    /// 不保存正文墓碑或承诺跨重启防重放。
    pub(super) fn cleanup_fragment(&mut self, index: usize) -> Result<(), GatewayError> {
        let generation = self.next_lifecycle_generation()?;
        let session = &self.sessions[index];
        let events = session.events.len();
        let chars = session.char_count;
        let recycled = session.zone() == RetentionZone::Recycle;
        let isolation = session.isolation.clone();
        let host_ids: Vec<HostEventId> = session
            .events
            .iter()
            .filter_map(|event| event.host_id.clone())
            .collect();
        let raw_keys: Vec<u64> = session.raw_key.into_iter().collect();
        let request_sequences = session
            .events
            .iter()
            .filter_map(|event| event.interaction.as_ref())
            .map(|interaction| interaction.request_sequence)
            .collect();
        let highest_source = session
            .events
            .iter()
            .filter(|event| event.interaction.is_some())
            .filter_map(|event| event.staged.source_order)
            .max();
        self.sessions.remove(index);
        if recycled {
            self.recycle_events -= events;
            self.recycle_chars -= chars;
        } else {
            self.events_total -= events;
            self.chars_total -= chars;
        }
        self.lifecycle_generation = generation;
        self.record_cleaned_tombstone(
            &isolation,
            host_ids,
            raw_keys,
            request_sequences,
            highest_source,
        );
        Ok(())
    }

    pub(super) fn check_cleaned_host_event(
        &self,
        isolation: &RequestIsolation,
        id: Option<&HostEventId>,
    ) -> Result<(), GatewayError> {
        if id.is_some_and(|id| {
            self.cleaned
                .iter()
                .any(|cleaned| &cleaned.isolation == isolation && cleaned.host_ids.contains(id))
        }) {
            return Err(GatewayError::Revoked);
        }
        Ok(())
    }

    pub(super) fn check_replay_budget(&self) -> Result<(), GatewayError> {
        if self.replay_protection_exhausted {
            return Err(GatewayError::ReplayProtectionExhausted);
        }
        Ok(())
    }

    fn record_cleaned_tombstone(
        &mut self,
        isolation: &RequestIsolation,
        host_ids: Vec<HostEventId>,
        raw_keys: Vec<u64>,
        request_sequences: BTreeSet<u64>,
        highest_source: Option<u64>,
    ) {
        if self.replay_protection_exhausted {
            return;
        }
        let highest_request = request_sequences.last().copied().unwrap_or(0);
        let existing = self
            .cleaned
            .iter()
            .position(|entry| entry.isolation == *isolation);
        let domain_count = self
            .cleaned
            .len()
            .checked_add(usize::from(existing.is_none()));
        let request_sequences: BTreeSet<_> = request_sequences
            .difference(&self.cleaned_requests)
            .copied()
            .collect();
        let host_ids: Vec<_> = host_ids
            .into_iter()
            .filter(|id| self.check_cleaned_host_event(isolation, Some(id)).is_ok())
            .collect();
        let raw_keys: Vec<_> = raw_keys
            .into_iter()
            .filter(|key| existing.is_none_or(|index| !self.cleaned[index].raw_keys.contains(key)))
            .collect();
        let recorded = self.cleaned.iter().try_fold(0usize, |total, entry| {
            total
                .checked_add(entry.host_ids.len())
                .and_then(|total| total.checked_add(entry.raw_keys.len()))
        });
        let host_count = recorded.and_then(|total| {
            total
                .checked_add(host_ids.len())
                .and_then(|total| total.checked_add(raw_keys.len()))
        });
        let request_count = self
            .cleaned_requests
            .len()
            .checked_add(request_sequences.len());
        if host_count.is_none_or(|total| total > self.limits.events_total)
            || request_count.is_none_or(|total| total > self.limits.events_total)
            || domain_count.is_none_or(|total| total > self.limits.events_total)
        {
            self.replay_protection_exhausted = true;
            return;
        }
        self.cleaned_requests.extend(request_sequences);
        if let Some(index) = existing {
            let existing = &mut self.cleaned[index];
            existing.host_ids.extend(host_ids);
            existing.raw_keys.extend(raw_keys);
            existing.highest_request = existing.highest_request.max(highest_request);
            existing.highest_source = existing.highest_source.max(highest_source);
        } else {
            self.cleaned.push(CleanedFragment {
                isolation: isolation.clone(),
                host_ids,
                raw_keys,
                highest_request,
                highest_source,
            });
        }
    }

    /// 受信宿主在本机主人校验完成后安装最小管理能力。能力绑定当前
    /// 协调器、入口运行实例、签发配置代际、完整桌宠身份、原隔离域与
    /// 精确片段范围；默认无权限，不提供通配全部私密记忆的捷径。
    /// 范围内任一片段的原始授权代际落后于当前权威入口配置时整体拒绝
    /// （`Revoked`），不用新能力重新放行旧片段；失败不产生能力记录。
    pub fn install_management_capability(
        &mut self,
        ingress: &TrustedIngress,
        identity: PetIdentity,
        isolation: RequestIsolation,
        fragments: Vec<SessionId>,
        permissions: ManagementPermissions,
    ) -> Result<ManagementCapability, GatewayError> {
        if self.retention.is_none() {
            return Err(GatewayError::RetentionNotConfigured);
        }
        if ingress.run_instance() != self.ingress_run {
            return Err(GatewayError::ForeignRequest);
        }
        if fragments.is_empty() {
            return Err(GatewayError::InvalidInput(
                "management capability must target an exact fragment scope",
            ));
        }
        if let RequestIsolation::Hinted(hint) = &isolation
            && hint.identity() != &identity
        {
            return Err(GatewayError::InvalidInput(
                "capability identity must match the isolation domain",
            ));
        }
        if self.capabilities.len() >= MAX_MANAGEMENT_CAPABILITIES {
            return Err(GatewayError::CapacityExceeded);
        }
        let mut scoped = Vec::new();
        for fragment in fragments {
            if !Arc::ptr_eq(&self.owner, &fragment.owner) {
                return Err(GatewayError::ForeignRequest);
            }
            let index = self
                .sessions
                .iter()
                .position(|candidate| candidate.id == fragment)
                .ok_or(GatewayError::StaleHandle)?;
            if self.sessions[index].isolation != isolation {
                return Err(GatewayError::ForeignRequest);
            }
            if self.sessions[index].authorization_generation != ingress.generation() {
                return Err(GatewayError::Revoked);
            }
            if !scoped.contains(&fragment) {
                scoped.push(fragment);
            }
        }
        let capability = ManagementCapability {
            owner: self.owner.clone(),
            index: self.capabilities.len(),
        };
        self.capabilities.push(CapabilityRecord {
            ingress_run: ingress.run_instance(),
            generation: ingress.generation(),
            identity,
            isolation,
            fragments: scoped,
            permissions,
            revoked: false,
        });
        Ok(capability)
    }

    /// 撤销管理能力；幂等，撤销后立即拒绝后续使用。记录保留在有界账本内。
    pub fn revoke_management_capability(
        &mut self,
        capability: &ManagementCapability,
    ) -> Result<(), GatewayError> {
        let index = self.management_index(capability)?;
        self.capabilities[index].revoked = true;
        Ok(())
    }

    /// 列出能力范围内仍存在的片段摘要（不含正文）；已清理片段不再出现，
    /// 摘要以 `logically_expired` 与 `final_expired` 区分正常到期待转移
    /// 与最终到期，不依赖定时维护。
    pub fn list_managed_fragments(
        &mut self,
        access: SessionAccess<'_>,
        capability: &ManagementCapability,
    ) -> Result<Vec<ManagedFragmentSummary>, GatewayError> {
        let index = self.authorize(&access, capability, |p| p.can_view())?;
        let scope = self.capabilities[index].fragments.clone();
        let generation = self.capabilities[index].generation;
        let recycle_ms = self
            .retention
            .as_ref()
            .expect("retention configured")
            .recycle_ms;
        let mut summaries = Vec::new();
        for fragment in &scope {
            let Some(position) = self
                .sessions
                .iter()
                .position(|candidate| &candidate.id == fragment)
            else {
                continue;
            };
            let session = &self.sessions[position];
            if session.authorization_generation != generation {
                return Err(GatewayError::Revoked);
            }
            let lifecycle = session.lifecycle.as_ref().expect("retention configured");
            summaries.push(ManagedFragmentSummary {
                session: session.id.clone(),
                isolation: session.isolation.clone(),
                zone: lifecycle.zone,
                lifecycle_version: lifecycle.version,
                created_at_ms: lifecycle.created_at_ms,
                normal_deadline_ms: lifecycle.normal_deadline_ms,
                final_deadline_ms: lifecycle.final_deadline_ms,
                processed_through: session.processed_through,
                event_count: session.events.len(),
                body_chars: session.char_count,
                logically_expired: lifecycle.zone == RetentionZone::Normal
                    && access.now_ms >= lifecycle.normal_deadline_ms,
                final_expired: lifecycle.final_expired(access.now_ms, recycle_ms)?,
            });
        }
        self.last_now_ms = access.now_ms;
        Ok(summaries)
    }

    /// 查看片段只读快照；要求 view 权限，回收区与逻辑到期待转移的片段
    /// 仍可查看；到达最终截止（含未维护的正常区硬截止）即返回
    /// `RawExpired`，已清理片段返回 `StaleHandle`，原片段授权代际落后
    /// 于当前权威配置返回 `Revoked`。
    pub fn inspect_fragment(
        &mut self,
        access: SessionAccess<'_>,
        capability: &ManagementCapability,
        session: &SessionId,
    ) -> Result<ManagedFragmentSnapshot, GatewayError> {
        let index = self.authorize(&access, capability, |p| p.can_view())?;
        let recycle_ms = self
            .retention
            .as_ref()
            .expect("retention configured")
            .recycle_ms;
        let snapshot = {
            let record = &self.capabilities[index];
            let position = self.managed_session_index(record, session)?;
            let fragment = &self.sessions[position];
            let lifecycle = fragment.lifecycle.as_ref().expect("retention configured");
            if lifecycle.final_expired(access.now_ms, recycle_ms)? {
                return Err(GatewayError::RawExpired);
            }
            ManagedFragmentSnapshot {
                session: fragment.id.clone(),
                isolation: fragment.isolation.clone(),
                zone: lifecycle.zone,
                lifecycle_version: lifecycle.version,
                created_at_ms: lifecycle.created_at_ms,
                normal_deadline_ms: lifecycle.normal_deadline_ms,
                final_deadline_ms: lifecycle.final_deadline_ms,
                processed_through: fragment.processed_through,
                logically_expired: lifecycle.zone == RetentionZone::Normal
                    && access.now_ms >= lifecycle.normal_deadline_ms,
                events: fragment
                    .events
                    .iter()
                    .map(|event| event.staged.clone())
                    .collect(),
            }
        };
        self.last_now_ms = access.now_ms;
        Ok(snapshot)
    }

    /// 获准软删除：权限、版本、期限与回收容量全部先行检查；逻辑到期
    /// 待转移的片段仍可软删除，已到最终截止的片段返回 `RawExpired`
    ///（应由受信维护或清空移除）。失败时原文、状态、两区统计、水位、
    /// 票据与截止时间保持不变，也不驱逐其它记录。
    pub fn soft_delete_fragment(
        &mut self,
        access: SessionAccess<'_>,
        capability: &ManagementCapability,
        session: &SessionId,
        expected_lifecycle_version: u64,
    ) -> Result<LifecycleReceipt, GatewayError> {
        let index = self.authorize(&access, capability, |p| p.can_soft_delete())?;
        let position = {
            let record = &self.capabilities[index];
            self.managed_session_index(record, session)?
        };
        {
            let recycle_ms = self
                .retention
                .as_ref()
                .expect("retention configured")
                .recycle_ms;
            let lifecycle = self.lifecycle(position);
            if lifecycle.zone != RetentionZone::Normal {
                return Err(GatewayError::InvalidInput(
                    "fragment is already in the recycle zone",
                ));
            }
            if lifecycle.version != expected_lifecycle_version {
                return Err(GatewayError::LifecycleVersionConflict);
            }
            if lifecycle.final_expired(access.now_ms, recycle_ms)? {
                return Err(GatewayError::RawExpired);
            }
        }
        let normal_deadline_ms = self.lifecycle(position).normal_deadline_ms;
        let logical_recycle_ms = if access.now_ms >= normal_deadline_ms {
            normal_deadline_ms
        } else {
            access.now_ms
        };
        self.transfer_to_recycle(position, logical_recycle_ms)?;
        self.last_now_ms = access.now_ms;
        Ok(self.lifecycle_receipt(position))
    }

    /// 获准恢复：要求当前有效能力、预期生命周期版本、未到最终截止且
    /// 正常区全部容量可用；原片段授权代际落后于当前权威配置时立即
    /// 拒绝，不能用重新签发的能力复活旧授权。成功后进入新生命周期
    /// 代际，正常截止为 min(恢复时刻 + 正常留存, 最终截止)，不恢复
    /// 活跃截止、旧交互执行权、旧批次或旧 Agent 任务。
    pub fn restore_fragment(
        &mut self,
        access: SessionAccess<'_>,
        capability: &ManagementCapability,
        session: &SessionId,
        expected_lifecycle_version: u64,
    ) -> Result<LifecycleReceipt, GatewayError> {
        let index = self.authorize(&access, capability, |p| p.can_restore())?;
        let position = {
            let record = &self.capabilities[index];
            self.managed_session_index(record, session)?
        };
        let retention = self.retention.as_ref().expect("retention configured");
        let final_deadline_ms = {
            let lifecycle = self.lifecycle(position);
            if lifecycle.zone != RetentionZone::Recycle {
                return Err(GatewayError::InvalidInput(
                    "fragment is not in the recycle zone",
                ));
            }
            if lifecycle.version != expected_lifecycle_version {
                return Err(GatewayError::LifecycleVersionConflict);
            }
            let final_deadline_ms = lifecycle
                .final_deadline_ms
                .expect("recycled fragments carry a fixed final deadline");
            if access.now_ms >= final_deadline_ms {
                return Err(GatewayError::RawExpired);
            }
            final_deadline_ms
        };
        let session = &self.sessions[position];
        let events = session.events.len();
        let chars = session.char_count;
        if self.normal_fragment_count() + 1 > self.limits.sessions
            || self
                .events_total
                .checked_add(events)
                .is_none_or(|total| total > self.limits.events_total)
            || self
                .chars_total
                .checked_add(chars)
                .is_none_or(|total| total > self.limits.total_chars)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        let version = self
            .lifecycle(position)
            .version
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let revision = session
            .revision
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let generation = self.next_lifecycle_generation()?;
        let normal_deadline_ms = access
            .now_ms
            .checked_add(retention.normal_ms)
            .ok_or(GatewayError::IdentifierExhausted)?
            .min(final_deadline_ms);
        let session = &mut self.sessions[position];
        let lifecycle = session.lifecycle.as_mut().expect("retention configured");
        lifecycle.zone = RetentionZone::Normal;
        lifecycle.normal_deadline_ms = normal_deadline_ms;
        lifecycle.version = version;
        session.revision = revision;
        self.recycle_events -= events;
        self.recycle_chars -= chars;
        self.events_total += events;
        self.chars_total += chars;
        self.lifecycle_generation = generation;
        self.last_now_ms = access.now_ms;
        Ok(self.lifecycle_receipt(position))
    }

    /// 获准清空单个片段（正常区或回收区），版本冲突与已清理句柄均明确
    /// 拒绝；清理只移除受管理在线数据，不追回已交付副本。
    pub fn purge_fragment(
        &mut self,
        access: SessionAccess<'_>,
        capability: &ManagementCapability,
        session: &SessionId,
        expected_lifecycle_version: u64,
    ) -> Result<PurgeReceipt, GatewayError> {
        let index = self.authorize(&access, capability, |p| p.can_purge())?;
        let position = {
            let record = &self.capabilities[index];
            self.managed_session_index(record, session)?
        };
        {
            let lifecycle = self.lifecycle(position);
            if lifecycle.version != expected_lifecycle_version {
                return Err(GatewayError::LifecycleVersionConflict);
            }
        }
        let receipt_session = self.sessions[position].id.clone();
        let released_events = self.sessions[position].events.len();
        let released_chars = self.sessions[position].char_count;
        self.cleanup_fragment(position)?;
        self.last_now_ms = access.now_ms;
        Ok(PurgeReceipt {
            session: receipt_session,
            released_events,
            released_chars,
        })
    }

    /// 获准清空能力范围内回收区全部片段；重复调用幂等返回零清空。
    /// 任一在范围回收片段的原始授权代际落后于当前权威配置时，先于
    /// 任何清空整体拒绝；此类内容由受信维护撤权清理移除。
    pub fn empty_recycle(
        &mut self,
        access: SessionAccess<'_>,
        capability: &ManagementCapability,
    ) -> Result<RecycleEmptyReport, GatewayError> {
        let index = self.authorize(&access, capability, |p| p.can_purge())?;
        let scope = self.capabilities[index].fragments.clone();
        let generation = self.capabilities[index].generation;
        if self.sessions.iter().any(|session| {
            session.zone() == RetentionZone::Recycle
                && session.authorization_generation != generation
                && scope.contains(&session.id)
        }) {
            return Err(GatewayError::Revoked);
        }
        let mut report = RecycleEmptyReport::default();
        for position in (0..self.sessions.len()).rev() {
            let eligible = self.sessions[position]
                .lifecycle
                .as_ref()
                .is_some_and(|lifecycle| {
                    lifecycle.zone == RetentionZone::Recycle
                        && scope
                            .iter()
                            .any(|scoped| *scoped == self.sessions[position].id)
                });
            if !eligible {
                continue;
            }
            let released_events = self.sessions[position].events.len();
            let released_chars = self.sessions[position].char_count;
            self.cleanup_fragment(position)?;
            report.purged += 1;
            report.released_events += released_events;
            report.released_chars += released_chars;
        }
        self.last_now_ms = access.now_ms;
        Ok(report)
    }

    /// 显式受信维护入口：以调用方传入的当前权威 [`TrustedIngress`] 先
    /// 执行撤权清理——原片段授权代际落后于当前配置的受管理原文被移除
    /// 并计入 `authorization_removed`——再转移到期片段、移除最终到期
    /// 内容并释放容量。可重复调用；多片段处理没有全局事务，单片段
    /// 转换原子。回收区满额时保持"逻辑到期、待转移"的原区计费状态并
    /// 在报告中显式背压，不静默清除仍在恢复期内的原文。
    pub fn maintain_retention(
        &mut self,
        ingress: &TrustedIngress,
        now_ms: u64,
    ) -> Result<MaintenanceReport, GatewayError> {
        if self.retention.is_none() {
            return Err(GatewayError::RetentionNotConfigured);
        }
        if ingress.run_instance() != self.ingress_run {
            return Err(GatewayError::ForeignRequest);
        }
        if now_ms < self.last_now_ms {
            return Err(GatewayError::ClockWentBackwards);
        }
        let generation = ingress.generation();
        let recycle_ms = self
            .retention
            .as_ref()
            .expect("retention configured")
            .recycle_ms;
        let mut report = MaintenanceReport::default();
        for position in (0..self.sessions.len()).rev() {
            let session = &self.sessions[position];
            let stale_authorization =
                session.lifecycle.is_some() && session.authorization_generation != generation;
            if stale_authorization {
                self.cleanup_for_maintenance(position, &mut report, true)?;
            }
        }
        for position in (0..self.sessions.len()).rev() {
            let decision = {
                let session = &self.sessions[position];
                let Some(lifecycle) = &session.lifecycle else {
                    continue;
                };
                match lifecycle.zone {
                    RetentionZone::Recycle => {
                        let final_deadline_ms = lifecycle
                            .final_deadline_ms
                            .expect("recycled fragments carry a fixed final deadline");
                        if now_ms >= final_deadline_ms {
                            Some(None)
                        } else {
                            None
                        }
                    }
                    RetentionZone::Normal => {
                        if now_ms < lifecycle.normal_deadline_ms {
                            None
                        } else {
                            let logical_final = lifecycle.logical_final_deadline(recycle_ms)?;
                            if now_ms >= logical_final {
                                Some(None)
                            } else {
                                Some(Some(lifecycle.normal_deadline_ms))
                            }
                        }
                    }
                }
            };
            match decision {
                None => {}
                Some(None) => {
                    self.cleanup_for_maintenance(position, &mut report, false)?;
                }
                Some(Some(logical_recycle_ms)) => {
                    match self.transfer_to_recycle(position, logical_recycle_ms) {
                        Ok(_) => report.transferred += 1,
                        Err(GatewayError::CapacityExceeded) => report.pending_transfer += 1,
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        self.last_now_ms = now_ms;
        Ok(report)
    }

    fn cleanup_for_maintenance(
        &mut self,
        position: usize,
        report: &mut MaintenanceReport,
        authorization_removed: bool,
    ) -> Result<(), GatewayError> {
        let released_events = report
            .released_events
            .checked_add(self.sessions[position].events.len())
            .ok_or(GatewayError::IdentifierExhausted)?;
        let released_chars = report
            .released_chars
            .checked_add(self.sessions[position].char_count)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let count = if authorization_removed {
            report.authorization_removed
        } else {
            report.removed
        }
        .checked_add(1)
        .ok_or(GatewayError::IdentifierExhausted)?;
        self.cleanup_fragment(position)?;
        report.released_events = released_events;
        report.released_chars = released_chars;
        if authorization_removed {
            report.authorization_removed = count;
        } else {
            report.removed = count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallerSubject, ChannelId, ChannelScope, IngressToken, RawIngressCall, ReplyTarget,
        SessionLimits, SessionTiming, UserEvent, UserEventKind,
    };
    use moe_memory_core::Namespace;

    fn fixture(retention: RawRetention, limits: SessionLimits) -> (TrustedIngress, SessionRuntime) {
        let mut ingress = TrustedIngress::new();
        ingress
            .register_route(
                "buddy",
                PetIdentity::new(Namespace::new("workspace", "owner").unwrap(), "pet").unwrap(),
            )
            .unwrap();
        ingress
            .install_credential(
                IngressToken::new("synthetic-token").unwrap(),
                CallerSubject::new("caller").unwrap(),
                ["buddy".to_owned()],
                ChannelId::new("channel").unwrap(),
                ChannelScope::new("scope").unwrap(),
                ReplyTarget::new("reply").unwrap(),
            )
            .unwrap();
        let runtime = SessionRuntime::with_retention(
            &ingress,
            limits,
            SessionTiming::default(),
            retention,
            0,
        );
        (ingress, runtime)
    }

    fn request(ingress: &mut TrustedIngress) -> crate::TrustedRequest {
        ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("synthetic-token").unwrap(),
                    "buddy",
                    "合成原文",
                )
                .with_session_hint("hint"),
            )
            .unwrap()
    }

    fn retention() -> RawRetention {
        RawRetention::new(1_000, 2_000, 8, 128, 8_192).unwrap()
    }

    fn limits() -> SessionLimits {
        SessionLimits::new(8, 16, 64, 1_024, 8_192, 2).unwrap()
    }

    #[test]
    fn registration_deadline_overflow_publishes_nothing() {
        let (mut ingress, mut runtime) =
            fixture(RawRetention::new(u64::MAX, 1, 1, 1, 1).unwrap(), limits());
        let request = request(&mut ingress);
        assert_eq!(
            runtime.register_user(
                SessionAccess::new(&ingress, &request, 5),
                UserEvent::new(UserEventKind::New),
            ),
            Err(GatewayError::IdentifierExhausted)
        );
        assert!(runtime.sessions.is_empty());
        assert_eq!(runtime.session_sequence, 0);
        assert_eq!(runtime.events_total, 0);
        assert_eq!(runtime.chars_total, 0);
        assert_eq!(runtime.last_now_ms, 0);
    }

    #[test]
    fn lifecycle_generation_overflow_blocks_transfer_without_half_update() {
        let (mut ingress, mut runtime) = fixture(retention(), limits());
        let request = request(&mut ingress);
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 0),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let capability = runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_soft_delete(),
            )
            .unwrap();
        let access = SessionAccess::new(&ingress, &request, 10);
        runtime.lifecycle_generation = u64::MAX;
        assert_eq!(
            runtime.soft_delete_fragment(access, &capability, &registration.session, 0),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(runtime.lifecycle_generation, u64::MAX);
        assert_eq!(runtime.lifecycle(0).zone, RetentionZone::Normal);
        assert_eq!(runtime.lifecycle(0).version, 0);
        assert_eq!(runtime.events_total, 1);
        assert_eq!(runtime.recycle_events, 0);
    }

    #[test]
    fn lifecycle_version_overflow_blocks_transition_without_half_update() {
        let (mut ingress, mut runtime) = fixture(retention(), limits());
        let request = request(&mut ingress);
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 0),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let capability = runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_soft_delete(),
            )
            .unwrap();
        let access = SessionAccess::new(&ingress, &request, 10);
        runtime.sessions[0].lifecycle.as_mut().unwrap().version = u64::MAX;
        assert_eq!(
            runtime.soft_delete_fragment(access, &capability, &registration.session, u64::MAX),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(runtime.lifecycle(0).zone, RetentionZone::Normal);
        assert_eq!(runtime.lifecycle(0).version, u64::MAX);
        assert_eq!(runtime.lifecycle_generation, 0);
    }

    #[test]
    fn recycle_counter_overflow_rejects_transfer_atomically() {
        let (mut ingress, mut runtime) = fixture(retention(), limits());
        let request = request(&mut ingress);
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 0),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let capability = runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_soft_delete(),
            )
            .unwrap();
        let access = SessionAccess::new(&ingress, &request, 10);
        runtime.recycle_events = usize::MAX;
        assert_eq!(
            runtime.soft_delete_fragment(access, &capability, &registration.session, 0),
            Err(GatewayError::CapacityExceeded)
        );
        assert_eq!(runtime.lifecycle(0).zone, RetentionZone::Normal);
        assert_eq!(runtime.recycle_events, usize::MAX);
        assert_eq!(runtime.recycle_chars, 0);
        assert_eq!(runtime.lifecycle_generation, 0);
    }

    #[test]
    fn revision_overflow_blocks_transition_without_half_update() {
        let (mut ingress, mut runtime) = fixture(retention(), limits());
        let request = request(&mut ingress);
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 0),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let capability = runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_soft_delete(),
            )
            .unwrap();
        let access = SessionAccess::new(&ingress, &request, 10);
        runtime.sessions[0].revision = u64::MAX;
        assert_eq!(
            runtime.soft_delete_fragment(access, &capability, &registration.session, 0),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(runtime.sessions[0].revision, u64::MAX);
        assert_eq!(runtime.lifecycle(0).zone, RetentionZone::Normal);
    }

    #[test]
    fn cleaned_tombstone_ledger_stays_bounded() {
        let (mut ingress, mut runtime) = fixture(
            retention(),
            SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap(),
        );
        let first = request(&mut ingress);
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &first, 0),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-a").unwrap()),
            )
            .unwrap();
        let capability = runtime
            .install_management_capability(
                &ingress,
                first.identity().clone(),
                first.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_purge(),
            )
            .unwrap();
        let second = request(&mut ingress);
        runtime
            .register_user(
                SessionAccess::new(&ingress, &second, 1),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-b").unwrap()),
            )
            .unwrap();
        runtime
            .purge_fragment(
                SessionAccess::new(&ingress, &second, 2),
                &capability,
                &registration.session,
                0,
            )
            .unwrap();
        assert_eq!(
            runtime
                .cleaned
                .iter()
                .map(|entry| entry.host_ids.len())
                .sum::<usize>(),
            2
        );
        let third = request(&mut ingress);
        let next = runtime
            .register_user(
                SessionAccess::new(&ingress, &third, 3),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-c").unwrap()),
            )
            .unwrap();
        let next_capability = runtime
            .install_management_capability(
                &ingress,
                third.identity().clone(),
                third.isolation().clone(),
                vec![next.session.clone()],
                ManagementPermissions::default().with_purge(),
            )
            .unwrap();
        runtime
            .purge_fragment(
                SessionAccess::new(&ingress, &third, 4),
                &next_capability,
                &next.session,
                0,
            )
            .unwrap();
        assert_eq!(
            runtime
                .cleaned
                .iter()
                .map(|entry| entry.host_ids.len())
                .sum::<usize>(),
            2
        );
        let replay = request(&mut ingress);
        assert_eq!(
            runtime.register_user(
                SessionAccess::new(&ingress, &replay, 5),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-a").unwrap()),
            ),
            Err(GatewayError::Revoked)
        );
        let fourth = request(&mut ingress);
        assert_eq!(
            runtime.register_user(
                SessionAccess::new(&ingress, &fourth, 6),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-c").unwrap()),
            ),
            Err(GatewayError::ReplayProtectionExhausted)
        );
        assert!(runtime.replay_protection_exhausted);
        assert!(runtime.cleaned_requests.len() <= runtime.limits.events_total);
    }

    #[test]
    fn ordering_watermarks_stay_bounded_without_host_ids() {
        let (mut ingress, mut runtime) = fixture(
            retention(),
            SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap(),
        );
        for (now_ms, hint) in [(0, "domain-a"), (2, "domain-b"), (4, "domain-c")] {
            let request = ingress
                .admit(
                    RawIngressCall::new(
                        IngressToken::new("synthetic-token").unwrap(),
                        "buddy",
                        "合成原文",
                    )
                    .with_session_hint(hint),
                )
                .unwrap();
            let registration = runtime
                .register_user(
                    SessionAccess::new(&ingress, &request, now_ms),
                    UserEvent::new(UserEventKind::New).with_source_order(10 + now_ms),
                )
                .unwrap();
            runtime
                .revoke_session(
                    SessionAccess::new(&ingress, &request, now_ms + 1),
                    &registration.session,
                )
                .unwrap();
            assert!(runtime.cleaned.len() <= runtime.limits.events_total);
            assert!(
                runtime
                    .cleaned
                    .iter()
                    .all(|entry| entry.host_ids.is_empty())
            );
        }
        assert_eq!(runtime.cleaned.len(), 2);
        assert_eq!(runtime.cleaned[0].highest_request, 1);
        assert_eq!(runtime.cleaned[0].highest_source, Some(10));
        assert_eq!(runtime.cleaned[1].highest_request, 2);
        assert_eq!(runtime.cleaned[1].highest_source, Some(12));
        assert!(runtime.replay_protection_exhausted);
        assert!(runtime.sessions.is_empty());
        assert_eq!(runtime.events_total, 0);
        assert_eq!(runtime.chars_total, 0);
        let fresh = request(&mut ingress);
        assert_eq!(
            runtime.register_user(
                SessionAccess::new(&ingress, &fresh, 100),
                UserEvent::new(UserEventKind::New).with_source_order(100),
            ),
            Err(GatewayError::ReplayProtectionExhausted),
        );
        assert_eq!(runtime.last_now_ms, 5);
        assert_eq!(runtime.session_sequence, 3);
    }

    #[test]
    fn ordering_watermarks_are_unchanged_on_cleanup_and_registration_failure() {
        let (mut ingress, mut runtime) = fixture(retention(), limits());
        let first = request(&mut ingress);
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &first, 0),
                UserEvent::new(UserEventKind::New).with_source_order(10),
            )
            .unwrap();
        runtime.lifecycle_generation = u64::MAX;
        assert_eq!(
            runtime.revoke_session(
                SessionAccess::new(&ingress, &first, 1),
                &registration.session,
            ),
            Err(GatewayError::IdentifierExhausted),
        );
        assert!(runtime.cleaned.is_empty());
        assert!(runtime.cleaned_requests.is_empty());
        assert_eq!(runtime.sessions.len(), 1);
        assert_eq!(runtime.events_total, 1);
        assert_eq!(runtime.chars_total, 4);
        assert_eq!(runtime.last_now_ms, 0);
        runtime.lifecycle_generation = 0;
        runtime
            .revoke_session(
                SessionAccess::new(&ingress, &first, 1),
                &registration.session,
            )
            .unwrap();
        let rejected = request(&mut ingress);
        let body_limit = runtime.limits.body_chars;
        runtime.limits.body_chars = 1;
        assert_eq!(
            runtime.register_user(
                SessionAccess::new(&ingress, &rejected, 100),
                UserEvent::new(UserEventKind::New).with_source_order(100),
            ),
            Err(GatewayError::CapacityExceeded),
        );
        assert_eq!(runtime.cleaned.len(), 1);
        assert_eq!(runtime.cleaned[0].highest_request, first.sequence());
        assert_eq!(runtime.cleaned[0].highest_source, Some(10));
        assert_eq!(runtime.last_now_ms, 1);
        assert!(runtime.sessions.is_empty());
        runtime.limits.body_chars = body_limit;
        let current = request(&mut ingress);
        assert!(
            runtime
                .register_user(
                    SessionAccess::new(&ingress, &current, 2),
                    UserEvent::new(UserEventKind::New).with_source_order(11),
                )
                .unwrap()
                .renewed
        );
    }
}
