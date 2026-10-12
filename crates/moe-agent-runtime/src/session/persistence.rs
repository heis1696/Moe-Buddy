//! G4 步骤 2：原文快照导出/导入纯运行契约。
//!
//! 无文件、网络或系统时钟 I/O；快照是不带权限证明的版本化数据，恢复必须由
//! 当前运行实例以新接纳的 [`TrustedRequest`] 加宿主显式安装的独立历史许可
//! 重新鉴权。导出/导入按完整稳定域绑定，完整预检后原子发布；撤销、顺序与
//! 耗尽栅栏跨恢复保留。线格式 v1 见 `docs/GATEWAY.md`。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use moe_memory_core::{Namespace, PetIdentity};
use sha2::{Digest, Sha256};

use super::lifecycle::logical_final_deadline;
use super::{
    BatchAcknowledgement, CleanedFragment, EventId, EventRecord, EventRole, FragmentLifecycle,
    HostEventId, Interaction, RetentionZone, Session, SessionId, SessionRuntime, StagedEvent,
    TerminalState, UserEvent, UserEventKind,
};
use crate::{
    CallerSubject, ChannelId, ChannelScope, GatewayError, RawRetention, ReplyTarget,
    RequestIsolation, SessionAccess, TrustedIngress, TrustedRequest, validation,
};

const MAGIC: &[u8; 8] = b"MOERAW1\0";
const FORMAT_VERSION: u32 = 1;
const MAX_RAW_HISTORY_GRANTS: usize = 64;
const MAX_RAW_IMPORT_RECORDS: usize = 64;
/// 导入记录账本的累计字节预算：全部已接纳快照的内容区字节
/// 总和不得超过一个完整快照上限；超出时新导入整体拒绝，不静默淘汰。
const MAX_RAW_IMPORT_LEDGER_BYTES: usize = RAW_SNAPSHOT_MAX_TOTAL_BYTES;
const RESTORED_REPLY_TARGET: &str = "restored-history-target";

/// 快照字节总长度上限：解码在分配前拒绝更大的输入，编码超出时失败。
pub const RAW_SNAPSHOT_MAX_TOTAL_BYTES: usize = 16_777_216;

/// 跨重启可比对的稳定存储归属域：完整桌宠身份、调用主体、渠道、渠道范围、
/// 受信宿主声明的稳定凭据归属与会话 hint 六元组。运行实例号与凭据局部句柄
/// 不进入稳定域；进入稳定域的是宿主为该凭据句柄显式绑定的归属，因此同一
/// 入口内归属不同的两张有效凭据必然落在不同稳定域。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RawDomainKey {
    identity: PetIdentity,
    subject: CallerSubject,
    channel: ChannelId,
    scope: ChannelScope,
    credential: String,
    hint: String,
}

impl RawDomainKey {
    /// 由受信宿主显式给出发量构造稳定域；凭据归属与 hint 按 identifier
    /// 规则校验。
    pub fn new(
        identity: PetIdentity,
        subject: CallerSubject,
        channel: ChannelId,
        scope: ChannelScope,
        credential: String,
        hint: String,
    ) -> Result<Self, GatewayError> {
        validation::identifier(&credential)?;
        validation::identifier(&hint)?;
        Ok(Self {
            identity,
            subject,
            channel,
            scope,
            credential,
            hint,
        })
    }

    /// 由当前已接纳请求投影稳定域：凭据归属按当前已认证
    /// [`CredentialId`](crate::CredentialId) 的宿主绑定解析。无会话提示的临时请求没有稳定归属；凭据未绑定归属
    /// 时同样拒绝持久化——两者都不凭运行内数字推测。
    pub fn from_request(
        ingress: &TrustedIngress,
        request: &TrustedRequest,
    ) -> Result<Self, GatewayError> {
        let Some(hint) = request.session_hint() else {
            return Err(GatewayError::InvalidInput(
                "request has no stable session hint",
            ));
        };
        let credential = ingress
            .credential_attribution(request.credential())
            .ok_or(GatewayError::InvalidInput(
                "credential has no stable attribution",
            ))?
            .to_owned();
        Ok(Self {
            identity: request.identity().clone(),
            subject: request.subject().clone(),
            channel: request.channel().clone(),
            scope: request.scope().clone(),
            credential,
            hint: hint.to_owned(),
        })
    }

    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    pub fn subject(&self) -> &CallerSubject {
        &self.subject
    }

    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }

    pub fn scope(&self) -> &ChannelScope {
        &self.scope
    }

    /// 受信宿主声明、当前已认证凭据所绑定的稳定归属。
    pub fn credential(&self) -> &str {
        &self.credential
    }

    pub fn hint(&self) -> &str {
        &self.hint
    }
}

impl fmt::Debug for RawDomainKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawDomainKey")
            .field(
                "identity_workspace",
                &self.identity.namespace().workspace_id(),
            )
            .field("identity_user", &self.identity.namespace().user_id())
            .field("identity_instance", &self.identity.instance_id())
            .field("subject", &self.subject.as_str())
            .field("channel", &self.channel.as_str())
            .field("scope", &self.scope.as_str())
            .field("credential", &self.credential)
            .field("hint", &"REDACTED")
            .finish()
    }
}

/// 独立、精确历史访问许可：由受信宿主在本机主人校验并确认旧授权仍有效后
/// 安装，绑定当前协调器、入口运行实例与精确稳定域；私有构造，可撤销。
/// 快照数据、校验和或档内数字均不能替代本许可。
#[derive(Clone)]
pub struct RawHistoryGrant {
    owner: Arc<()>,
    index: usize,
}

impl fmt::Debug for RawHistoryGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RawHistoryGrant(REDACTED)")
    }
}

pub(super) struct RawGrantRecord {
    pub(super) ingress_run: u64,
    pub(super) domain: RawDomainKey,
    pub(super) revoked: bool,
}

pub(super) struct RawImportRecord {
    pub(super) domain: RawDomainKey,
    pub(super) fingerprint: [u8; 32],
    pub(super) fragment_keys: BTreeSet<u64>,
    pub(super) committed_at_ms: u64,
    /// 首导入的内容区字节数：保留累计接纳预算，不持有原文副本。
    pub(super) content_bytes: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct SnapshotFragment {
    key: u64,
    zone: RetentionZone,
    created_at_ms: u64,
    normal_deadline_ms: u64,
    hard_deadline_ms: u64,
    final_deadline_ms: Option<u64>,
    lifecycle_version: u64,
    processed_through: usize,
    events: Vec<SnapshotEvent>,
}

#[derive(Clone, PartialEq, Eq)]
struct SnapshotEvent {
    role: EventRole,
    body: String,
    received_ms: u64,
    parent_ordinal: usize,
    source_timestamp_ms: Option<u64>,
    source_order: Option<u64>,
    client_request_hint: Option<String>,
    host_event_id: Option<String>,
    terminal: Option<TerminalState>,
    acknowledgement: Option<BatchAcknowledgement>,
}

/// 不带权限证明的结构化 v1 快照：一个稳定域的原文片段、生命周期期限、
/// 授权修订与撤销/顺序/耗尽栅栏（含稳定片段键撤销栅栏）。Debug 不含正文；
/// 相同内容的 `encode` 字节确定。
#[derive(Clone, PartialEq, Eq)]
pub struct RawSnapshotV1 {
    domain: RawDomainKey,
    committed_at_ms: u64,
    /// 导出时该域稳定凭据归属绑定的授权修订号；导入必须与当前绑定一致，
    /// 已撤销的旧修订不能凭相同局部数字或重签许可恢复。
    authorization_revision: u64,
    exhausted: bool,
    revoked_host_event_ids: Vec<String>,
    /// 已撤销片段的稳定片段键：与宿主事件 ID 栅栏互补，事件没有宿主
    /// ID 时仍能阻止旧快照以存活片段复活已清理正文。
    revoked_fragment_keys: Vec<u64>,
    highest_source_order: Option<u64>,
    fragments: Vec<SnapshotFragment>,
}

impl RawSnapshotV1 {
    pub fn domain(&self) -> &RawDomainKey {
        &self.domain
    }

    pub fn committed_at_ms(&self) -> u64 {
        self.committed_at_ms
    }

    pub fn fragment_count(&self) -> usize {
        self.fragments.len()
    }

    /// 按本文线格式编码；结构化内容相同则字节相同，超过总长上限失败。
    /// 内容区逐字段前置预算检查，累计超限立即失败，不先分配超限缓冲。
    /// FNV-1a 校验和覆盖提交时间与内容区（提交时间纳入完整性校验；
    /// 幂等重复判定使用规范内容 SHA-256 指纹，独立排除提交时间）。
    pub fn encode(&self) -> Result<Vec<u8>, GatewayError> {
        let mut writer = ContentWriter::new();
        self.encode_content(&mut writer)?;
        let content = writer.into_inner();
        let total = content
            .len()
            .checked_add(24)
            .ok_or(GatewayError::CapacityExceeded)?;
        if total > RAW_SNAPSHOT_MAX_TOTAL_BYTES {
            return Err(GatewayError::CapacityExceeded);
        }
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&self.committed_at_ms.to_le_bytes());
        bytes.extend_from_slice(&content);
        let checksum = fnv1a(&bytes[16..]);
        bytes[12..16].copy_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }

    /// 严格解码：先检查总长与头部，再按剩余字节检查每个长度前缀与计数，
    /// 校验枚举、标志、UTF-8、标识符与正文规则；随后执行与结构化导入
    /// 共享的语义/预算校验。坏档明确拒绝且不分配。
    pub fn decode(bytes: &[u8]) -> Result<Self, GatewayError> {
        if bytes.len() > RAW_SNAPSHOT_MAX_TOTAL_BYTES {
            return Err(GatewayError::RawSnapshotInvalid(
                "snapshot exceeds total byte budget",
            ));
        }
        if bytes.len() < 24 {
            return Err(GatewayError::RawSnapshotInvalid("truncated header"));
        }
        if &bytes[0..8] != MAGIC {
            return Err(GatewayError::RawSnapshotInvalid("unknown magic"));
        }
        if u32::from_le_bytes(bytes[8..12].try_into().expect("fixed width")) != FORMAT_VERSION {
            return Err(GatewayError::RawSnapshotInvalid(
                "unsupported format version",
            ));
        }
        let checksum = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed width"));
        let committed_at_ms = u64::from_le_bytes(bytes[16..24].try_into().expect("fixed width"));
        let content = &bytes[24..];
        if fnv1a(&bytes[16..]) != checksum {
            return Err(GatewayError::RawSnapshotInvalid(
                "content checksum mismatch",
            ));
        }
        let mut cursor = Cursor::new(content);
        let domain = decode_domain(&mut cursor)?;
        let authorization_revision = cursor.u64()?;
        let exhausted = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return Err(GatewayError::RawSnapshotInvalid("invalid exhausted flag")),
        };
        let revoked_count = cursor.u32()?;
        let mut revoked_host_event_ids = Vec::new();
        for _ in 0..revoked_count {
            let id = cursor.identifier_string("revoked host event id")?;
            if revoked_host_event_ids.contains(&id) {
                return Err(GatewayError::RawSnapshotInvalid(
                    "duplicate revoked host event id",
                ));
            }
            revoked_host_event_ids.push(id);
        }
        let revoked_key_count = cursor.u32()?;
        let mut revoked_fragment_keys = Vec::new();
        for _ in 0..revoked_key_count {
            let key = cursor.u64()?;
            if revoked_fragment_keys.contains(&key) {
                return Err(GatewayError::RawSnapshotInvalid(
                    "duplicate revoked fragment key",
                ));
            }
            revoked_fragment_keys.push(key);
        }
        let highest_source_order = cursor.optional_u64()?;
        let fragment_count = cursor.u32()?;
        let mut fragments = Vec::new();
        for _ in 0..fragment_count {
            fragments.push(decode_fragment(&mut cursor)?);
        }
        if !cursor.is_empty() {
            return Err(GatewayError::RawSnapshotInvalid(
                "trailing bytes after content",
            ));
        }
        let snapshot = Self {
            domain,
            committed_at_ms,
            authorization_revision,
            exhausted,
            revoked_host_event_ids,
            revoked_fragment_keys,
            highest_source_order,
            fragments,
        };
        snapshot.validate_content()?;
        Ok(snapshot)
    }

    /// 除提交时间外的规范内容 SHA-256 指纹：仅用于有界进程内重复判定，
    /// 不持有正文，不是授权或数学上的无碰撞证明；授权修订参与计算。
    fn content_fingerprint(&self) -> Result<[u8; 32], GatewayError> {
        let mut content = ContentWriter::new();
        self.encode_content(&mut content)?;
        Ok(Sha256::digest(content.into_inner()).into())
    }

    /// 内容区的精确编码字节数（不含 24 字节头部）：结构化导入在拥有化、
    /// 克隆或发布前用它核对 16MiB 总预算与导入记录累计预算。
    fn content_bytes(&self) -> usize {
        fn string_bytes(total: Option<usize>, value: &str) -> Option<usize> {
            let prefix = total?.checked_add(4)?;
            prefix.checked_add(value.len())
        }
        fn optional_u64_bytes(total: Option<usize>, value: Option<u64>) -> Option<usize> {
            match value {
                None => total?.checked_add(1),
                Some(_) => total?.checked_add(1 + 8),
            }
        }
        fn optional_string_bytes(total: Option<usize>, value: &Option<String>) -> Option<usize> {
            match value {
                None => total?.checked_add(1),
                Some(inner) => string_bytes(total?.checked_add(1), inner),
            }
        }
        let mut total = Some(0usize);
        for value in [
            self.domain.identity.namespace().workspace_id(),
            self.domain.identity.namespace().user_id(),
            self.domain.identity.instance_id(),
            self.domain.subject.as_str(),
            self.domain.channel.as_str(),
            self.domain.scope.as_str(),
            self.domain.credential.as_str(),
            self.domain.hint.as_str(),
        ] {
            total = string_bytes(total, value);
        }
        total = total.and_then(|total| total.checked_add(8 + 1 + 4));
        for id in &self.revoked_host_event_ids {
            total = string_bytes(total, id);
        }
        total = total.and_then(|total| {
            let keys = 8usize.checked_mul(self.revoked_fragment_keys.len())?;
            total.checked_add(4)?.checked_add(keys)
        });
        total = optional_u64_bytes(total, self.highest_source_order);
        total = total.and_then(|total| total.checked_add(4));
        for fragment in &self.fragments {
            total = total.and_then(|total| total.checked_add(8 + 1 + 8 + 8 + 8));
            total = optional_u64_bytes(total, fragment.final_deadline_ms);
            total = total.and_then(|total| total.checked_add(8 + 4 + 4));
            for event in &fragment.events {
                total = total.and_then(|total| total.checked_add(1 + 8 + 4));
                total = string_bytes(total, &event.body);
                total = optional_u64_bytes(total, event.source_timestamp_ms);
                total = optional_u64_bytes(total, event.source_order);
                total = optional_string_bytes(total, &event.client_request_hint);
                total = optional_string_bytes(total, &event.host_event_id);
                total = total.and_then(|total| total.checked_add(1 + 1));
            }
        }
        total.unwrap_or(usize::MAX)
    }

    /// 结构化快照的语义/预算校验：解码、结构化导入与净化共享同一实现，
    /// 语义坏档（冲突标识、非法期限/事件时间、回收区无固定最终截止、
    /// 确认前缀含未确认事件、用户/父事件角色与关系不一致）在发布前被
    /// 拒绝，不能先发布再等待维护触发错误。
    fn validate_content(&self) -> Result<(), GatewayError> {
        let mut live_keys = BTreeSet::new();
        let mut live_host_ids = BTreeSet::new();
        for fragment in &self.fragments {
            if !live_keys.insert(fragment.key) {
                return Err(snapshot_error("duplicate fragment key"));
            }
            if self.revoked_fragment_keys.contains(&fragment.key) {
                return Err(snapshot_error("fragment key is both live and revoked"));
            }
            if fragment.zone == RetentionZone::Recycle && fragment.final_deadline_ms.is_none() {
                return Err(snapshot_error(
                    "recycled fragment has no fixed final deadline",
                ));
            }
            if fragment.created_at_ms > self.committed_at_ms {
                return Err(snapshot_error("fragment created after commit time"));
            }
            if fragment.created_at_ms > fragment.normal_deadline_ms {
                return Err(snapshot_error("normal deadline before fragment creation"));
            }
            if fragment.normal_deadline_ms > fragment.hard_deadline_ms {
                return Err(snapshot_error("normal deadline after hard deadline"));
            }
            if let Some(final_deadline) = fragment.final_deadline_ms {
                if final_deadline > fragment.hard_deadline_ms {
                    return Err(snapshot_error("final deadline after hard deadline"));
                }
                if final_deadline < fragment.created_at_ms {
                    return Err(snapshot_error("final deadline before fragment creation"));
                }
                if fragment.zone == RetentionZone::Normal
                    && final_deadline < fragment.normal_deadline_ms
                {
                    return Err(snapshot_error("normal deadline after final deadline"));
                }
            }
            if fragment.processed_through > fragment.events.len() {
                return Err(snapshot_error("processed watermark beyond events"));
            }
            if fragment
                .events
                .get(fragment.processed_through)
                .is_some_and(|event| event.acknowledgement.is_some())
            {
                return Err(snapshot_error(
                    "processed watermark precedes confirmed prefix",
                ));
            }
            for (index, event) in fragment.events.iter().enumerate() {
                let ordinal = index + 1;
                if event.received_ms > self.committed_at_ms {
                    return Err(snapshot_error("event received after commit time"));
                }
                if event.received_ms < fragment.created_at_ms {
                    return Err(snapshot_error("event received before fragment creation"));
                }
                if ordinal <= fragment.processed_through && event.acknowledgement.is_none() {
                    return Err(snapshot_error(
                        "confirmed prefix contains an unacknowledged event",
                    ));
                }
                let is_user = matches!(event.role, EventRole::User | EventRole::UserHistory);
                if is_user {
                    if event.parent_ordinal != 0 {
                        return Err(snapshot_error("user event carries a parent event"));
                    }
                } else {
                    let parent = event
                        .parent_ordinal
                        .checked_sub(1)
                        .and_then(|parent| fragment.events.get(parent))
                        .ok_or(snapshot_error("non-user event has no parent event"))?;
                    if !matches!(parent.role, EventRole::User | EventRole::UserHistory) {
                        return Err(snapshot_error("event parent is not a user event"));
                    }
                }
                if let Some(host_id) = &event.host_event_id {
                    if self.revoked_host_event_ids.contains(host_id) {
                        return Err(snapshot_error("host event id is both live and revoked"));
                    }
                    if !live_host_ids.insert(host_id.as_str()) {
                        return Err(snapshot_error("duplicate live host event id"));
                    }
                }
            }
        }
        Ok(())
    }

    /// 快照过期维护：无 I/O、不改动输入；先拒绝早于提交时间的可信时刻，
    /// 再对全部片段按 §4 收紧期限做全量预检（含全部 `checked_add`），然后
    /// 构建净化后的新快照。已到有效最终截止（含等号，未固定时按原正常
    /// 截止加当前回收时长推算并封顶硬截止，推算溢出整单失败）的片段正文
    /// 整体移除，其稳定片段键、宿主事件 ID 与来源顺序最大值一并并入撤销
    /// 栅栏；逻辑到期的正常区片段按原（收紧后）正常截止转入回收区并固定
    /// 最终截止；保留片段同样固化收紧期限，原文逐字节保留。不重置耗尽、不解除栅栏、不放宽期限、
    /// 不签发任何执行权。
    pub fn sweep_expired(
        &self,
        now_ms: u64,
        retention: &RawRetention,
    ) -> Result<RawSnapshotSweep, GatewayError> {
        self.validate_content()?;
        if now_ms < self.committed_at_ms {
            return Err(GatewayError::ClockWentBackwards);
        }
        enum Decision {
            Keep,
            Transfer {
                final_deadline_ms: u64,
                version: u64,
            },
            Remove,
        }
        let mut decisions = Vec::with_capacity(self.fragments.len());
        let mut removed = 0usize;
        let mut transferred = 0usize;
        let mut retained = 0usize;
        for fragment in &self.fragments {
            let (effective_normal, effective_hard, effective_final) = tightened_deadlines(
                retention,
                fragment.created_at_ms,
                fragment.normal_deadline_ms,
                fragment.hard_deadline_ms,
                fragment.final_deadline_ms,
            );
            let logical_final = logical_final_deadline(
                effective_final,
                effective_normal,
                effective_hard,
                retention.recycle_ms(),
            )?;
            let decision = if now_ms >= logical_final {
                removed += 1;
                Decision::Remove
            } else if fragment.zone == RetentionZone::Recycle {
                retained += 1;
                Decision::Keep
            } else if now_ms >= effective_normal {
                let version = fragment
                    .lifecycle_version
                    .checked_add(1)
                    .ok_or(GatewayError::IdentifierExhausted)?;
                transferred += 1;
                Decision::Transfer {
                    final_deadline_ms: logical_final,
                    version,
                }
            } else {
                retained += 1;
                Decision::Keep
            };
            decisions.push((decision, effective_normal, effective_hard, effective_final));
        }
        let mut revoked_host_event_ids = self.revoked_host_event_ids.clone();
        let mut revoked_fragment_keys = self.revoked_fragment_keys.clone();
        let mut highest_source_order = self.highest_source_order;
        let mut fragments = Vec::with_capacity(self.fragments.len());
        for (fragment, (decision, normal_deadline_ms, hard_deadline_ms, final_deadline_ms)) in
            self.fragments.iter().zip(decisions)
        {
            match decision {
                Decision::Keep => fragments.push(SnapshotFragment {
                    normal_deadline_ms,
                    hard_deadline_ms,
                    final_deadline_ms,
                    ..fragment.clone()
                }),
                Decision::Transfer {
                    final_deadline_ms,
                    version,
                } => fragments.push(SnapshotFragment {
                    zone: RetentionZone::Recycle,
                    normal_deadline_ms,
                    hard_deadline_ms,
                    final_deadline_ms: Some(final_deadline_ms),
                    lifecycle_version: version,
                    ..fragment.clone()
                }),
                Decision::Remove => {
                    if !revoked_fragment_keys.contains(&fragment.key) {
                        revoked_fragment_keys.push(fragment.key);
                    }
                    for event in &fragment.events {
                        if let Some(host_id) = &event.host_event_id
                            && !revoked_host_event_ids.contains(host_id)
                        {
                            revoked_host_event_ids.push(host_id.clone());
                        }
                        highest_source_order = highest_source_order.max(event.source_order);
                    }
                }
            }
        }
        Ok(RawSnapshotSweep {
            snapshot: RawSnapshotV1 {
                domain: self.domain.clone(),
                committed_at_ms: self.committed_at_ms,
                authorization_revision: self.authorization_revision,
                exhausted: self.exhausted,
                revoked_host_event_ids,
                revoked_fragment_keys,
                highest_source_order,
                fragments,
            },
            removed,
            transferred,
            retained,
        })
    }

    fn encode_content(&self, out: &mut ContentWriter) -> Result<(), GatewayError> {
        put_str(out, self.domain.identity.namespace().workspace_id())?;
        put_str(out, self.domain.identity.namespace().user_id())?;
        put_str(out, self.domain.identity.instance_id())?;
        put_str(out, self.domain.subject.as_str())?;
        put_str(out, self.domain.channel.as_str())?;
        put_str(out, self.domain.scope.as_str())?;
        put_str(out, &self.domain.credential)?;
        put_str(out, &self.domain.hint)?;
        put_u64(out, self.authorization_revision)?;
        put_u8(out, u8::from(self.exhausted))?;
        let revoked_count = u32::try_from(self.revoked_host_event_ids.len())
            .map_err(|_| GatewayError::CapacityExceeded)?;
        put_u32(out, revoked_count)?;
        for id in &self.revoked_host_event_ids {
            put_str(out, id)?;
        }
        let revoked_key_count = u32::try_from(self.revoked_fragment_keys.len())
            .map_err(|_| GatewayError::CapacityExceeded)?;
        put_u32(out, revoked_key_count)?;
        for key in &self.revoked_fragment_keys {
            put_u64(out, *key)?;
        }
        put_optional_u64(out, self.highest_source_order)?;
        let fragment_count =
            u32::try_from(self.fragments.len()).map_err(|_| GatewayError::CapacityExceeded)?;
        put_u32(out, fragment_count)?;
        for fragment in &self.fragments {
            put_u64(out, fragment.key)?;
            put_u8(
                out,
                match fragment.zone {
                    RetentionZone::Normal => 0,
                    RetentionZone::Recycle => 1,
                },
            )?;
            put_u64(out, fragment.created_at_ms)?;
            put_u64(out, fragment.normal_deadline_ms)?;
            put_u64(out, fragment.hard_deadline_ms)?;
            put_optional_u64(out, fragment.final_deadline_ms)?;
            put_u64(out, fragment.lifecycle_version)?;
            put_u32(
                out,
                u32::try_from(fragment.processed_through)
                    .map_err(|_| GatewayError::CapacityExceeded)?,
            )?;
            put_u32(
                out,
                u32::try_from(fragment.events.len()).map_err(|_| GatewayError::CapacityExceeded)?,
            )?;
            for event in &fragment.events {
                put_u8(
                    out,
                    match event.role {
                        EventRole::User => 1,
                        EventRole::UserHistory => 2,
                        EventRole::Assistant => 3,
                        EventRole::ExternalAgent => 4,
                        EventRole::ToolReceipt => 5,
                        EventRole::InjectedContext => 6,
                        EventRole::Background => 7,
                    },
                )?;
                put_str(out, &event.body)?;
                put_u64(out, event.received_ms)?;
                put_u32(
                    out,
                    u32::try_from(event.parent_ordinal)
                        .map_err(|_| GatewayError::CapacityExceeded)?,
                )?;
                put_optional_u64(out, event.source_timestamp_ms)?;
                put_optional_u64(out, event.source_order)?;
                match &event.client_request_hint {
                    None => put_u8(out, 0)?,
                    Some(hint) => {
                        put_u8(out, 1)?;
                        put_str(out, hint)?;
                    }
                }
                match &event.host_event_id {
                    None => put_u8(out, 0)?,
                    Some(id) => {
                        put_u8(out, 1)?;
                        put_str(out, id)?;
                    }
                }
                put_u8(
                    out,
                    match event.terminal {
                        None => 0,
                        Some(TerminalState::Completed) => 1,
                        Some(TerminalState::Failed) => 2,
                        Some(TerminalState::Cancelled) => 3,
                        Some(TerminalState::Unknown) => 4,
                    },
                )?;
                put_u8(
                    out,
                    match event.acknowledgement {
                        None => 0,
                        Some(BatchAcknowledgement::Confirmed) => 1,
                        Some(BatchAcknowledgement::Skipped) => 2,
                    },
                )?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for RawSnapshotV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawSnapshotV1")
            .field("domain", &self.domain)
            .field("committed_at_ms", &self.committed_at_ms)
            .field("authorization_revision", &self.authorization_revision)
            .field("fragments", &self.fragments.len())
            .field(
                "events",
                &self.fragments.iter().map(|f| f.events.len()).sum::<usize>(),
            )
            .field("revoked_host_event_ids", &self.revoked_host_event_ids.len())
            .field("revoked_fragment_keys", &self.revoked_fragment_keys.len())
            .field("exhausted", &self.exhausted)
            .finish_non_exhaustive()
    }
}

/// 导入回执：`duplicate` 表示同内容快照的幂等重放；`fragments` 按快照内
/// 片段键列出受信服务新签发的本地句柄，已清理片段不再出现且不复活。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawImportReceipt {
    pub domain: RawDomainKey,
    pub committed_at_ms: u64,
    pub duplicate: bool,
    pub replay_protection_exhausted: bool,
    pub fragments: Vec<RestoredRawFragment>,
}

/// 恢复出的单个历史片段句柄；期限为收紧后的生效值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredRawFragment {
    pub key: u64,
    pub session: SessionId,
    pub zone: RetentionZone,
    pub normal_deadline_ms: u64,
    pub final_deadline_ms: Option<u64>,
}

/// `sweep_expired` 的净化回执：`snapshot` 是构建出的新快照（输入不被
/// 改动），三个计数与其中片段一一对应。Debug 经 `RawSnapshotV1` 脱敏。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSnapshotSweep {
    pub snapshot: RawSnapshotV1,
    pub removed: usize,
    pub transferred: usize,
    pub retained: usize,
}

/// 按隔离域内凭据句柄的宿主绑定解析稳定域；`Fresh` 隔离或凭据从未绑定
/// 归属时返回 `None`——这样的数据不属于任何稳定域，导出/导入不触达。
fn raw_domain_of(isolation: &RequestIsolation, ingress: &TrustedIngress) -> Option<RawDomainKey> {
    let hint = match isolation {
        RequestIsolation::Fresh { .. } => return None,
        RequestIsolation::Hinted(hint) => hint,
    };
    let credential = ingress
        .credential_attribution(hint.credential())?
        .to_owned();
    Some(RawDomainKey {
        identity: hint.identity().clone(),
        subject: hint.subject().clone(),
        channel: hint.channel().clone(),
        scope: hint.scope().clone(),
        credential,
        hint: hint.hint().to_owned(),
    })
}

fn snapshot_error(reason: &'static str) -> GatewayError {
    GatewayError::RawSnapshotInvalid(reason)
}

fn decode_domain(cursor: &mut Cursor<'_>) -> Result<RawDomainKey, GatewayError> {
    let workspace = cursor.string()?;
    let user = cursor.string()?;
    let instance = cursor.string()?;
    let subject = cursor.string()?;
    let channel = cursor.string()?;
    let scope = cursor.string()?;
    let credential = cursor.identifier_string("credential field is invalid")?;
    let hint = cursor.string()?;
    let namespace = Namespace::new(workspace, user)
        .map_err(|_| snapshot_error("identity namespace field is invalid"))?;
    let identity = PetIdentity::new(namespace, instance)
        .map_err(|_| snapshot_error("identity instance field is invalid"))?;
    let subject =
        CallerSubject::new(subject).map_err(|_| snapshot_error("subject field is invalid"))?;
    let channel =
        ChannelId::new(channel).map_err(|_| snapshot_error("channel field is invalid"))?;
    let scope = ChannelScope::new(scope).map_err(|_| snapshot_error("scope field is invalid"))?;
    validation::identifier(&hint).map_err(|_| snapshot_error("hint field is invalid"))?;
    Ok(RawDomainKey {
        identity,
        subject,
        channel,
        scope,
        credential,
        hint,
    })
}

fn decode_fragment(cursor: &mut Cursor<'_>) -> Result<SnapshotFragment, GatewayError> {
    let key = cursor.u64()?;
    let zone = match cursor.u8()? {
        0 => RetentionZone::Normal,
        1 => RetentionZone::Recycle,
        _ => return Err(snapshot_error("unknown retention zone")),
    };
    let created_at_ms = cursor.u64()?;
    let normal_deadline_ms = cursor.u64()?;
    let hard_deadline_ms = cursor.u64()?;
    let final_deadline_ms = cursor.optional_u64()?;
    let lifecycle_version = cursor.u64()?;
    let processed_through = cursor.u32()? as usize;
    let event_count = cursor.u32()? as usize;
    let mut events = Vec::new();
    for ordinal in 1..=event_count {
        let role = match cursor.u8()? {
            1 => EventRole::User,
            2 => EventRole::UserHistory,
            3 => EventRole::Assistant,
            4 => EventRole::ExternalAgent,
            5 => EventRole::ToolReceipt,
            6 => EventRole::InjectedContext,
            7 => EventRole::Background,
            _ => return Err(snapshot_error("unknown event role")),
        };
        let body = cursor.string()?;
        validation::text(&body, RAW_SNAPSHOT_MAX_TOTAL_BYTES)
            .map_err(|_| snapshot_error("event body is invalid"))?;
        let received_ms = cursor.u64()?;
        let parent_ordinal = cursor.u32()? as usize;
        if parent_ordinal >= ordinal {
            return Err(snapshot_error("event parent exceeds fragment order"));
        }
        let source_timestamp_ms = cursor.optional_u64()?;
        let source_order = cursor.optional_u64()?;
        let client_request_hint = cursor.optional_string("client request hint")?;
        let host_event_id = cursor.optional_string("host event id")?;
        let terminal = match cursor.u8()? {
            0 => None,
            1 => Some(TerminalState::Completed),
            2 => Some(TerminalState::Failed),
            3 => Some(TerminalState::Cancelled),
            4 => Some(TerminalState::Unknown),
            _ => return Err(snapshot_error("unknown terminal state")),
        };
        let acknowledgement = match cursor.u8()? {
            0 => None,
            1 => Some(BatchAcknowledgement::Confirmed),
            2 => Some(BatchAcknowledgement::Skipped),
            _ => return Err(snapshot_error("unknown acknowledgement")),
        };
        events.push(SnapshotEvent {
            role,
            body,
            received_ms,
            parent_ordinal,
            source_timestamp_ms,
            source_order,
            client_request_hint,
            host_event_id,
            terminal,
            acknowledgement,
        });
    }
    Ok(SnapshotFragment {
        key,
        zone,
        created_at_ms,
        normal_deadline_ms,
        hard_deadline_ms,
        final_deadline_ms,
        lifecycle_version,
        processed_through,
        events,
    })
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// 内容区编码写入器：每次追加前检查累计长度，超过快照总字节预算立即
/// 失败，不先分配超限缓冲。
struct ContentWriter {
    buffer: Vec<u8>,
}

impl ContentWriter {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn into_inner(self) -> Vec<u8> {
        self.buffer
    }

    fn reserve(&self, additional: usize) -> Result<(), GatewayError> {
        if self
            .buffer
            .len()
            .checked_add(additional)
            .is_none_or(|total| total > RAW_SNAPSHOT_MAX_TOTAL_BYTES)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        Ok(())
    }
}

fn put_u8(out: &mut ContentWriter, value: u8) -> Result<(), GatewayError> {
    out.reserve(1)?;
    out.buffer.push(value);
    Ok(())
}

fn put_u32(out: &mut ContentWriter, value: u32) -> Result<(), GatewayError> {
    out.reserve(4)?;
    out.buffer.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_u64(out: &mut ContentWriter, value: u64) -> Result<(), GatewayError> {
    out.reserve(8)?;
    out.buffer.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_str(out: &mut ContentWriter, value: &str) -> Result<(), GatewayError> {
    let length = u32::try_from(value.len()).map_err(|_| GatewayError::CapacityExceeded)?;
    out.reserve(
        4usize
            .checked_add(value.len())
            .ok_or(GatewayError::CapacityExceeded)?,
    )?;
    out.buffer.extend_from_slice(&length.to_le_bytes());
    out.buffer.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_optional_u64(out: &mut ContentWriter, value: Option<u64>) -> Result<(), GatewayError> {
    match value {
        None => put_u8(out, 0),
        Some(inner) => {
            put_u8(out, 1)?;
            put_u64(out, inner)
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], GatewayError> {
        if self.remaining() < length {
            return Err(snapshot_error("length prefix exceeds remaining bytes"));
        }
        let slice = &self.bytes[self.position..self.position + length];
        self.position += length;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, GatewayError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, GatewayError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("fixed width"),
        ))
    }

    fn u64(&mut self) -> Result<u64, GatewayError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("fixed width"),
        ))
    }

    fn string(&mut self) -> Result<String, GatewayError> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| snapshot_error("length exceeds platform usize"))?;
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| snapshot_error("field is not valid UTF-8"))
    }

    fn identifier_string(&mut self, field: &'static str) -> Result<String, GatewayError> {
        let value = self.string()?;
        validation::identifier(&value).map_err(|_| snapshot_error(field))?;
        Ok(value)
    }

    fn optional_u64(&mut self) -> Result<Option<u64>, GatewayError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err(snapshot_error("invalid optional flag")),
        }
    }

    fn optional_string(&mut self, field: &'static str) -> Result<Option<String>, GatewayError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.identifier_string(field)?)),
            _ => Err(snapshot_error("invalid optional flag")),
        }
    }
}

/// 按当前留存政策对档内期限取收紧结果；政策候选上界溢出视为不收紧，
/// 任何结果都不放宽档内原值。
fn tightened_deadlines(
    retention: &RawRetention,
    created_at_ms: u64,
    saved_normal: u64,
    saved_hard: u64,
    saved_final: Option<u64>,
) -> (u64, u64, Option<u64>) {
    let policy_normal = created_at_ms.checked_add(retention.normal_ms());
    let policy_hard = policy_normal.and_then(|normal| normal.checked_add(retention.recycle_ms()));
    let normal = saved_normal.min(policy_normal.unwrap_or(u64::MAX));
    let hard = saved_hard.min(policy_hard.unwrap_or(u64::MAX));
    let final_deadline = saved_final.map(|deadline| deadline.min(hard));
    (normal, hard, final_deadline)
}

impl SessionRuntime {
    /// 受信宿主在本机主人校验并核对旧授权仍有效后安装独立历史许可；
    /// 绑定当前协调器、入口运行实例与精确稳定域，账本上限 64 条。域内
    /// 凭据归属必须当前有在册凭据持有——映射缺失即拒绝，不预发许可。
    pub fn install_raw_history_grant(
        &mut self,
        ingress: &TrustedIngress,
        domain: RawDomainKey,
    ) -> Result<RawHistoryGrant, GatewayError> {
        if self.retention.is_none() {
            return Err(GatewayError::RetentionNotConfigured);
        }
        if ingress.run_instance() != self.ingress_run {
            return Err(GatewayError::ForeignRequest);
        }
        if !ingress.has_live_attribution(domain.credential()) {
            return Err(GatewayError::InvalidInput(
                "domain credential attribution is not bound to a live credential",
            ));
        }
        if self.raw_grants.len() >= MAX_RAW_HISTORY_GRANTS {
            return Err(GatewayError::CapacityExceeded);
        }
        let grant = RawHistoryGrant {
            owner: self.owner.clone(),
            index: self.raw_grants.len(),
        };
        self.raw_grants.push(RawGrantRecord {
            ingress_run: ingress.run_instance(),
            domain,
            revoked: false,
        });
        Ok(grant)
    }

    /// 撤销历史许可；幂等，撤销后立即拒绝导入。记录保留在有界账本内。
    pub fn revoke_raw_history_grant(
        &mut self,
        grant: &RawHistoryGrant,
    ) -> Result<(), GatewayError> {
        let index = self.raw_grant_index(grant)?;
        self.raw_grants[index].revoked = true;
        Ok(())
    }

    fn raw_grant_index(&self, grant: &RawHistoryGrant) -> Result<usize, GatewayError> {
        if !Arc::ptr_eq(&self.owner, &grant.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        if grant.index >= self.raw_grants.len() {
            return Err(GatewayError::StaleHandle);
        }
        Ok(grant.index)
    }

    /// 导出当前请求稳定域的原文快照：域内全部片段（正常区、回收区与
    /// 逻辑到期待转移者）与该域全部清理栅栏（含稳定片段键撤销栅栏）。
    /// 三阶段执行，任一失败不产生半更新：先对域内全部片段完整校验——
    /// `revoked` 置位或原授权代际落后于当前权威配置整体拒绝（`Revoked`），
    /// 已到有效最终截止拒绝（`RawExpired`）；再在本地状态构建快照并预演
    /// 编码——编码超限或键序号耗尽时片段键与序号都不先行发布；全部通过
    /// 后才一次性提交新片段键。能力撤销与历史授权撤销的内容都不得借导出
    /// 洗成可恢复历史。`committed_at_ms` 由受信宿主注入且不得早于当前
    /// 时刻；快照携带导出时归属绑定的授权修订号。
    pub fn export_raw_snapshot(
        &mut self,
        access: SessionAccess<'_>,
        committed_at_ms: u64,
    ) -> Result<RawSnapshotV1, GatewayError> {
        if self.retention.is_none() {
            return Err(GatewayError::RetentionNotConfigured);
        }
        let domain = RawDomainKey::from_request(access.ingress, access.request)?;
        self.check(&access)?;
        if committed_at_ms < access.now_ms {
            return Err(GatewayError::InvalidInput(
                "committed_at_ms precedes the export present",
            ));
        }
        let recycle_ms = self
            .retention
            .as_ref()
            .expect("retention configured")
            .recycle_ms;
        let mut keyless = 0usize;
        for session in &self.sessions {
            if raw_domain_of(&session.isolation, access.ingress).as_ref() != Some(&domain) {
                continue;
            }
            if session.revoked {
                return Err(GatewayError::Revoked);
            }
            if session.authorization_generation != access.request.generation() {
                return Err(GatewayError::Revoked);
            }
            let lifecycle = session
                .lifecycle
                .as_ref()
                .expect("retention-configured fragments carry lifecycle state");
            if lifecycle.final_expired(access.now_ms, recycle_ms)? {
                return Err(GatewayError::RawExpired);
            }
            if session.raw_key.is_none() {
                keyless += 1;
            }
        }
        let authorization_revision = access
            .ingress
            .stable_credential_revision(access.request.credential())
            .ok_or(GatewayError::InvalidInput(
                "credential has no stable attribution",
            ))?;
        let mut used_keys = BTreeSet::new();
        for session in &self.sessions {
            if raw_domain_of(&session.isolation, access.ingress).as_ref() == Some(&domain)
                && let Some(key) = session.raw_key
            {
                used_keys.insert(key);
            }
        }
        if let Some(record) = self
            .raw_imports
            .iter()
            .find(|record| record.domain == domain)
        {
            used_keys.extend(record.fragment_keys.iter().copied());
        }
        for entry in &self.cleaned {
            if raw_domain_of(&entry.isolation, access.ingress).as_ref() == Some(&domain) {
                used_keys.extend(entry.raw_keys.iter().copied());
            }
        }
        let mut allocated = Vec::with_capacity(keyless);
        let mut candidate = self.raw_key_sequence;
        while allocated.len() < keyless {
            candidate = candidate
                .checked_add(1)
                .ok_or(GatewayError::IdentifierExhausted)?;
            if !used_keys.contains(&candidate) {
                allocated.push(candidate);
            }
        }
        let mut fragments = Vec::new();
        let mut allocated_iter = allocated.iter().copied();
        for session in &self.sessions {
            if raw_domain_of(&session.isolation, access.ingress).as_ref() != Some(&domain) {
                continue;
            }
            let key = match session.raw_key {
                Some(key) => key,
                None => allocated_iter
                    .next()
                    .expect("allocation covers every keyless fragment"),
            };
            let lifecycle = session
                .lifecycle
                .as_ref()
                .expect("retention-configured fragments carry lifecycle state");
            let events = session
                .events
                .iter()
                .map(|record| SnapshotEvent {
                    role: record.staged.role,
                    body: record.staged.body.clone(),
                    received_ms: record.staged.received_ms,
                    parent_ordinal: record.parent.unwrap_or(0),
                    source_timestamp_ms: record.staged.source_timestamp_ms,
                    source_order: record.staged.source_order,
                    client_request_hint: record.staged.client_request_hint.clone(),
                    host_event_id: record.host_id.clone().map(|HostEventId(value)| value),
                    terminal: record.staged.terminal,
                    acknowledgement: record.staged.acknowledgement,
                })
                .collect();
            fragments.push(SnapshotFragment {
                key,
                zone: lifecycle.zone,
                created_at_ms: lifecycle.created_at_ms,
                normal_deadline_ms: lifecycle.normal_deadline_ms,
                hard_deadline_ms: lifecycle.hard_deadline_ms,
                final_deadline_ms: lifecycle.final_deadline_ms,
                lifecycle_version: lifecycle.version,
                processed_through: session.processed_through,
                events,
            });
        }
        let mut revoked_host_event_ids = Vec::new();
        let mut revoked_fragment_keys = Vec::new();
        let mut highest_source_order = None;
        for entry in &self.cleaned {
            if raw_domain_of(&entry.isolation, access.ingress).as_ref() != Some(&domain) {
                continue;
            }
            for HostEventId(value) in &entry.host_ids {
                if !revoked_host_event_ids.contains(value) {
                    revoked_host_event_ids.push(value.clone());
                }
            }
            for key in &entry.raw_keys {
                if !revoked_fragment_keys.contains(key) {
                    revoked_fragment_keys.push(*key);
                }
            }
            highest_source_order = highest_source_order.max(entry.highest_source);
        }
        let snapshot = RawSnapshotV1 {
            domain,
            committed_at_ms,
            authorization_revision,
            exhausted: self.replay_protection_exhausted,
            revoked_host_event_ids,
            revoked_fragment_keys,
            highest_source_order,
            fragments,
        };
        snapshot.encode()?;
        let last_allocated = allocated.last().copied();
        let mut assignments = allocated.into_iter();
        for session in &mut self.sessions {
            if session.raw_key.is_none()
                && raw_domain_of(&session.isolation, access.ingress).as_ref()
                    == Some(&snapshot.domain)
            {
                session.raw_key = Some(
                    assignments
                        .next()
                        .expect("allocation covers every keyless fragment"),
                );
            }
        }
        if let Some(last) = last_allocated {
            self.raw_key_sequence = last;
        }
        self.last_now_ms = access.now_ms;
        Ok(snapshot)
    }

    /// 历史访问闸门：供外层文件适配在读取、净化与导入前对当前请求做
    /// 完整鉴权——留存已启用、当前受信请求复验（异实例/代际落后/凭据
    /// 撤销拒绝、时间不回退）、域投影等于目标稳定域（含凭据归属解析）、
    /// 历史许可在册且未撤销、绑定同一入口运行实例与精确域。任何失败不
    /// 改变状态。注意：通过本闸门不等于内容可导入，导入另受授权修订、
    /// 期限、预算与语义校验约束；签名保持
    /// `(&self, SessionAccess<'_>, &RawHistoryGrant, &RawDomainKey)
    /// -> Result<(), GatewayError>` 供适配固定依赖。
    pub fn validate_raw_history_access(
        &self,
        access: SessionAccess<'_>,
        grant: &RawHistoryGrant,
        domain: &RawDomainKey,
    ) -> Result<(), GatewayError> {
        if self.retention.is_none() {
            return Err(GatewayError::RetentionNotConfigured);
        }
        self.check(&access)?;
        let current_domain = RawDomainKey::from_request(access.ingress, access.request)?;
        let grant_index = self.raw_grant_index(grant)?;
        let record = &self.raw_grants[grant_index];
        if record.revoked {
            return Err(GatewayError::Revoked);
        }
        if record.ingress_run != access.ingress.run_instance()
            || &record.domain != domain
            || &current_domain != domain
        {
            return Err(GatewayError::ForeignRequest);
        }
        Ok(())
    }

    /// 完整预检后原子导入：当前请求加独立历史许可重新鉴权（复用
    /// [`SessionRuntime::validate_raw_history_access`]，与文件适配的
    /// 读写/净化前鉴权同一实现），快照授权修订必须等于当前归属绑定修订，
    /// 精确域绑定、CAS 与容量、期限、编码预算全部通过后才发布新历史片段
    /// 句柄；导入不产生活跃会话、可完结交互、批次或 Agent 执行权，不复制
    /// 旧证明。撤销/顺序/耗尽栅栏（含稳定片段键栅栏）并入当前隔离域，
    /// 跨恢复不清零；栅栏或导入记录账本预算放不下时整体拒绝，不静默遗忘
    /// 后放行，也不在耗尽后继续增长。重复导入按规范内容 SHA-256 指纹判定，
    /// 32 位校验和碰撞不构成重复。
    pub fn import_raw_snapshot(
        &mut self,
        access: SessionAccess<'_>,
        grant: &RawHistoryGrant,
        snapshot: &RawSnapshotV1,
    ) -> Result<RawImportReceipt, GatewayError> {
        self.validate_raw_history_access(
            SessionAccess::new(access.ingress, access.request, access.now_ms),
            grant,
            &snapshot.domain,
        )?;
        let domain = RawDomainKey::from_request(access.ingress, access.request)?;
        let current_revision = access
            .ingress
            .stable_credential_revision(access.request.credential())
            .ok_or(GatewayError::InvalidInput(
                "credential has no stable attribution",
            ))?;
        if snapshot.authorization_revision != current_revision {
            return Err(GatewayError::Revoked);
        }
        if access.now_ms < snapshot.committed_at_ms {
            return Err(GatewayError::ClockWentBackwards);
        }
        snapshot.validate_content()?;
        let content_bytes = snapshot.content_bytes();
        if content_bytes
            .checked_add(24)
            .is_none_or(|total| total > RAW_SNAPSHOT_MAX_TOTAL_BYTES)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        let fingerprint = snapshot.content_fingerprint()?;
        let record_index = self
            .raw_imports
            .iter()
            .position(|record| record.domain == snapshot.domain);
        let duplicate = match record_index {
            Some(index) if self.raw_imports[index].fingerprint == fingerprint => true,
            Some(_) => return Err(GatewayError::VersionConflict),
            None => false,
        };
        let owned_keys: BTreeSet<u64> = record_index
            .map(|index| self.raw_imports[index].fragment_keys.clone())
            .unwrap_or_default();
        for session in &self.sessions {
            if raw_domain_of(&session.isolation, access.ingress).as_ref() == Some(&domain)
                && !session.raw_key.is_some_and(|key| owned_keys.contains(&key))
            {
                return Err(GatewayError::InvalidInput(
                    "domain already holds fragments outside this snapshot",
                ));
            }
        }
        if duplicate {
            return self.duplicate_import_receipt(&access, snapshot.domain.clone(), &owned_keys);
        }
        self.check_replay_budget()?;
        if self.raw_imports.len() >= MAX_RAW_IMPORT_RECORDS {
            return Err(GatewayError::CapacityExceeded);
        }
        let ledger_bytes = self
            .raw_imports
            .iter()
            .try_fold(0usize, |total, record| {
                total.checked_add(record.content_bytes)
            })
            .ok_or(GatewayError::CapacityExceeded)?;
        if ledger_bytes
            .checked_add(content_bytes)
            .is_none_or(|total| total > MAX_RAW_IMPORT_LEDGER_BYTES)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        let limits = self.limits;
        let retention = *self.retention.as_ref().expect("retention configured");
        let isolation = access.request.isolation().clone();
        let domain_entry_of = |entry: &CleanedFragment| {
            raw_domain_of(&entry.isolation, access.ingress).as_ref() == Some(&domain)
        };
        let fenced_host_ids: BTreeSet<&str> = self
            .cleaned
            .iter()
            .filter(|entry| domain_entry_of(entry))
            .flat_map(|entry| entry.host_ids.iter().map(|HostEventId(id)| id.as_str()))
            .collect();
        let fenced_keys: BTreeSet<u64> = self
            .cleaned
            .iter()
            .filter(|entry| domain_entry_of(entry))
            .flat_map(|entry| entry.raw_keys.iter().copied())
            .collect();
        for fragment in &snapshot.fragments {
            if fenced_keys.contains(&fragment.key) {
                return Err(GatewayError::InvalidInput(
                    "snapshot fragment key conflicts with an existing revocation fence",
                ));
            }
            for event in &fragment.events {
                if let Some(host_id) = &event.host_event_id
                    && fenced_host_ids.contains(host_id.as_str())
                {
                    return Err(GatewayError::InvalidInput(
                        "snapshot event conflicts with an existing revocation fence",
                    ));
                }
            }
        }
        for fragment in &snapshot.fragments {
            if fragment.events.len() > limits.events_per_session {
                return Err(GatewayError::CapacityExceeded);
            }
            for event in &fragment.events {
                let chars = event.body.chars().count();
                if chars > limits.body_chars {
                    return Err(GatewayError::CapacityExceeded);
                }
            }
            let (effective_normal, effective_hard, effective_final) = tightened_deadlines(
                &retention,
                fragment.created_at_ms,
                fragment.normal_deadline_ms,
                fragment.hard_deadline_ms,
                fragment.final_deadline_ms,
            );
            let logical_final = logical_final_deadline(
                effective_final,
                effective_normal,
                effective_hard,
                retention.recycle_ms(),
            )?;
            if access.now_ms >= logical_final {
                return Err(GatewayError::RawExpired);
            }
        }
        let mut normal_fragments = 0usize;
        let mut recycle_fragments = 0usize;
        let mut normal_events = 0usize;
        let mut recycle_events = 0usize;
        let mut normal_chars = 0usize;
        let mut recycle_chars = 0usize;
        for fragment in &snapshot.fragments {
            let mut fragment_chars = 0usize;
            for event in &fragment.events {
                let chars = event.body.chars().count();
                fragment_chars = fragment_chars
                    .checked_add(chars)
                    .ok_or(GatewayError::CapacityExceeded)?;
            }
            match fragment.zone {
                RetentionZone::Normal => {
                    normal_fragments = normal_fragments
                        .checked_add(1)
                        .ok_or(GatewayError::CapacityExceeded)?;
                    normal_events = normal_events
                        .checked_add(fragment.events.len())
                        .ok_or(GatewayError::CapacityExceeded)?;
                    normal_chars = normal_chars
                        .checked_add(fragment_chars)
                        .ok_or(GatewayError::CapacityExceeded)?;
                }
                RetentionZone::Recycle => {
                    recycle_fragments = recycle_fragments
                        .checked_add(1)
                        .ok_or(GatewayError::CapacityExceeded)?;
                    recycle_events = recycle_events
                        .checked_add(fragment.events.len())
                        .ok_or(GatewayError::CapacityExceeded)?;
                    recycle_chars = recycle_chars
                        .checked_add(fragment_chars)
                        .ok_or(GatewayError::CapacityExceeded)?;
                }
            }
        }
        if self
            .normal_fragment_count()
            .checked_add(normal_fragments)
            .is_none_or(|total| total > limits.sessions)
            || self
                .events_total
                .checked_add(normal_events)
                .is_none_or(|total| total > limits.events_total)
            || self
                .chars_total
                .checked_add(normal_chars)
                .is_none_or(|total| total > limits.total_chars)
            || self
                .recycle_fragment_count()
                .checked_add(recycle_fragments)
                .is_none_or(|total| total > retention.recycle_fragments())
            || self
                .recycle_events
                .checked_add(recycle_events)
                .is_none_or(|total| total > retention.recycle_events())
            || self
                .recycle_chars
                .checked_add(recycle_chars)
                .is_none_or(|total| total > retention.recycle_chars())
        {
            return Err(GatewayError::CapacityExceeded);
        }
        let new_host_ids: Vec<HostEventId> = snapshot
            .revoked_host_event_ids
            .iter()
            .filter(|id| !fenced_host_ids.contains(id.as_str()))
            .map(|id| HostEventId(id.clone()))
            .collect();
        let new_fragment_keys: Vec<u64> = snapshot
            .revoked_fragment_keys
            .iter()
            .filter(|key| !fenced_keys.contains(key))
            .copied()
            .collect();
        let existing_entry = self
            .cleaned
            .iter()
            .position(|entry| entry.isolation == isolation);
        let existing_items = self
            .cleaned
            .iter()
            .try_fold(0usize, |total, entry| {
                total
                    .checked_add(entry.host_ids.len())
                    .and_then(|total| total.checked_add(entry.raw_keys.len()))
            })
            .ok_or(GatewayError::CapacityExceeded)?;
        let projected_items = existing_items
            .checked_add(new_host_ids.len())
            .and_then(|total| total.checked_add(new_fragment_keys.len()));
        let projected_domains = self
            .cleaned
            .len()
            .checked_add(usize::from(existing_entry.is_none()));
        if projected_items.is_none_or(|total| total > limits.events_total)
            || projected_domains.is_none_or(|total| total > limits.events_total)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        let total_fragments = normal_fragments + recycle_fragments;
        self.session_sequence
            .checked_add(total_fragments as u64)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let receipt_fragments =
            self.publish_imported_fragments(&access, snapshot, retention, total_fragments)?;
        match existing_entry {
            Some(index) => {
                let entry = &mut self.cleaned[index];
                entry.host_ids.extend(new_host_ids);
                entry.raw_keys.extend(new_fragment_keys);
                entry.highest_source = entry.highest_source.max(snapshot.highest_source_order);
            }
            None => self.cleaned.push(CleanedFragment {
                isolation,
                host_ids: new_host_ids,
                raw_keys: new_fragment_keys,
                highest_request: 0,
                highest_source: snapshot.highest_source_order,
            }),
        }
        if snapshot.exhausted {
            self.replay_protection_exhausted = true;
        }
        self.raw_imports.push(RawImportRecord {
            domain: snapshot.domain.clone(),
            fingerprint,
            fragment_keys: snapshot
                .fragments
                .iter()
                .map(|fragment| fragment.key)
                .collect(),
            committed_at_ms: snapshot.committed_at_ms,
            content_bytes,
        });
        self.last_now_ms = access.now_ms;
        Ok(RawImportReceipt {
            domain: snapshot.domain.clone(),
            committed_at_ms: snapshot.committed_at_ms,
            duplicate: false,
            replay_protection_exhausted: self.replay_protection_exhausted,
            fragments: receipt_fragments,
        })
    }

    fn duplicate_import_receipt(
        &mut self,
        access: &SessionAccess<'_>,
        domain: RawDomainKey,
        owned_keys: &BTreeSet<u64>,
    ) -> Result<RawImportReceipt, GatewayError> {
        let committed_at_ms = self
            .raw_imports
            .iter()
            .find(|record| record.domain == domain)
            .map(|record| record.committed_at_ms)
            .unwrap_or_default();
        let fragments = owned_keys
            .iter()
            .filter_map(|key| {
                let session = self.sessions.iter().find(|session| {
                    session.raw_key.as_ref() == Some(key)
                        && raw_domain_of(&session.isolation, access.ingress).as_ref()
                            == Some(&domain)
                })?;
                let lifecycle = session
                    .lifecycle
                    .as_ref()
                    .expect("retention-configured fragments carry lifecycle state");
                Some(RestoredRawFragment {
                    key: *key,
                    session: session.id.clone(),
                    zone: session.zone(),
                    normal_deadline_ms: lifecycle.normal_deadline_ms,
                    final_deadline_ms: lifecycle.final_deadline_ms,
                })
            })
            .collect();
        self.last_now_ms = access.now_ms;
        Ok(RawImportReceipt {
            domain,
            committed_at_ms,
            duplicate: true,
            replay_protection_exhausted: self.replay_protection_exhausted,
            fragments,
        })
    }

    fn publish_imported_fragments(
        &mut self,
        access: &SessionAccess<'_>,
        snapshot: &RawSnapshotV1,
        retention: RawRetention,
        total_fragments: usize,
    ) -> Result<Vec<RestoredRawFragment>, GatewayError> {
        let isolation = access.request.isolation().clone();
        let generation = access.request.generation();
        let restored_target =
            ReplyTarget::new(RESTORED_REPLY_TARGET).expect("constant is a valid identifier");
        let mut receipt_fragments = Vec::with_capacity(total_fragments);
        for fragment in &snapshot.fragments {
            let sequence = self.session_sequence + 1;
            self.session_sequence = sequence;
            let session_id = SessionId {
                owner: self.owner.clone(),
                sequence,
            };
            let (normal_deadline_ms, hard_deadline_ms, final_deadline_ms) = tightened_deadlines(
                &retention,
                fragment.created_at_ms,
                fragment.normal_deadline_ms,
                fragment.hard_deadline_ms,
                fragment.final_deadline_ms,
            );
            let mut events = Vec::with_capacity(fragment.events.len());
            let mut char_count = 0usize;
            for (index, event) in fragment.events.iter().enumerate() {
                let is_user = matches!(event.role, EventRole::User | EventRole::UserHistory);
                let terminal = if is_user {
                    Some(event.terminal.unwrap_or(TerminalState::Cancelled))
                } else {
                    event.terminal
                };
                let id = EventId {
                    session: session_id.clone(),
                    ordinal: index + 1,
                };
                let host_id = event.host_event_id.clone().map(HostEventId);
                let input = is_user.then(|| UserEvent {
                    id: host_id.clone(),
                    kind: UserEventKind::History,
                    source_order: event.source_order,
                    source_timestamp_ms: event.source_timestamp_ms,
                });
                let interaction = is_user.then(|| Interaction {
                    request_sequence: 0,
                    reply_target: restored_target.clone(),
                    deadline_ms: 0,
                    reply_event: None,
                });
                char_count += event.body.chars().count();
                events.push(EventRecord {
                    staged: StagedEvent {
                        id,
                        role: event.role,
                        body: event.body.clone(),
                        received_ms: event.received_ms,
                        source_timestamp_ms: event.source_timestamp_ms,
                        source_order: event.source_order,
                        client_request_hint: event.client_request_hint.clone(),
                        terminal,
                        acknowledgement: event.acknowledgement,
                    },
                    host_id,
                    input,
                    interaction,
                    parent: if event.parent_ordinal == 0 {
                        None
                    } else {
                        Some(event.parent_ordinal)
                    },
                });
            }
            self.sessions.push(Session {
                id: session_id.clone(),
                isolation: isolation.clone(),
                authorization_generation: generation,
                revoked: false,
                active_until_ms: None,
                last_user_ms: None,
                revision: 0,
                processed_through: fragment.processed_through,
                events,
                batch: None,
                char_count,
                lifecycle: Some(FragmentLifecycle {
                    zone: fragment.zone,
                    created_at_ms: fragment.created_at_ms,
                    normal_deadline_ms,
                    hard_deadline_ms,
                    final_deadline_ms,
                    version: fragment.lifecycle_version,
                }),
                raw_key: Some(fragment.key),
            });
            match fragment.zone {
                RetentionZone::Normal => {
                    self.events_total += fragment.events.len();
                    self.chars_total += char_count;
                }
                RetentionZone::Recycle => {
                    self.recycle_events += fragment.events.len();
                    self.recycle_chars += char_count;
                }
            }
            receipt_fragments.push(RestoredRawFragment {
                key: fragment.key,
                session: session_id,
                zone: fragment.zone,
                normal_deadline_ms,
                final_deadline_ms,
            });
        }
        Ok(receipt_fragments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_domain(hint: &str) -> RawDomainKey {
        RawDomainKey::new(
            PetIdentity::new(Namespace::new("workspace", "owner").unwrap(), "pet").unwrap(),
            CallerSubject::new("caller").unwrap(),
            ChannelId::new("channel").unwrap(),
            ChannelScope::new("scope").unwrap(),
            "credential-1".to_owned(),
            hint.to_owned(),
        )
        .unwrap()
    }

    fn test_event(role: EventRole) -> SnapshotEvent {
        SnapshotEvent {
            role,
            body: "合成正文".to_owned(),
            received_ms: 5,
            parent_ordinal: 0,
            source_timestamp_ms: Some(3),
            source_order: Some(9),
            client_request_hint: Some("client-1".to_owned()),
            host_event_id: Some("host-1".to_owned()),
            terminal: Some(TerminalState::Completed),
            acknowledgement: Some(BatchAcknowledgement::Confirmed),
        }
    }

    #[test]
    fn fnv1a_matches_known_vectors() {
        assert_eq!(fnv1a(b""), 0x811c_9dc5);
        assert_eq!(fnv1a(b"a"), 0xe40c_292c);
        assert_eq!(fnv1a(b"foobar"), 0xbf9c_f968);
    }

    #[test]
    fn tightening_only_reduces_and_overflow_declines_to_tighten() {
        let retention = RawRetention::new(1_000, 2_000, 4, 64, 8_192).unwrap();
        assert_eq!(
            tightened_deadlines(&retention, 5_000, 6_000, 8_000, Some(7_500)),
            (6_000, 8_000, Some(7_500))
        );
        assert_eq!(
            tightened_deadlines(&retention, 0, 100_000, 200_000, Some(150_000)),
            (1_000, 3_000, Some(3_000))
        );
        let huge = RawRetention::new(u64::MAX, u64::MAX, 1, 1, 1).unwrap();
        assert_eq!(
            tightened_deadlines(&huge, 10, 100, 200, None),
            (100, 200, None)
        );
    }

    #[test]
    fn fingerprint_ignores_commit_time_but_nothing_else() {
        let base = RawSnapshotV1 {
            domain: test_domain("hint"),
            committed_at_ms: 42,
            authorization_revision: 1,
            exhausted: false,
            revoked_host_event_ids: Vec::new(),
            revoked_fragment_keys: Vec::new(),
            highest_source_order: None,
            fragments: Vec::new(),
        };
        let resealed = RawSnapshotV1 {
            committed_at_ms: 99,
            ..base.clone()
        };
        let fingerprint = base.content_fingerprint().unwrap();
        assert_eq!(fingerprint, resealed.content_fingerprint().unwrap());
        assert_ne!(
            fingerprint,
            RawSnapshotV1 {
                exhausted: true,
                ..base.clone()
            }
            .content_fingerprint()
            .unwrap()
        );
        assert_ne!(
            fingerprint,
            RawSnapshotV1 {
                highest_source_order: Some(1),
                ..base.clone()
            }
            .content_fingerprint()
            .unwrap()
        );
        assert_ne!(
            fingerprint,
            RawSnapshotV1 {
                revoked_host_event_ids: vec!["host-gone".to_owned()],
                ..base.clone()
            }
            .content_fingerprint()
            .unwrap()
        );
        assert_ne!(
            fingerprint,
            RawSnapshotV1 {
                revoked_fragment_keys: vec![7],
                ..base.clone()
            }
            .content_fingerprint()
            .unwrap()
        );
        assert_ne!(
            fingerprint,
            RawSnapshotV1 {
                authorization_revision: 2,
                ..base.clone()
            }
            .content_fingerprint()
            .unwrap()
        );
        let other_domain = RawSnapshotV1 {
            domain: test_domain("other-hint"),
            ..base.clone()
        };
        assert_ne!(fingerprint, other_domain.content_fingerprint().unwrap());
    }

    #[test]
    fn content_bytes_matches_encoded_length() {
        let snapshot = RawSnapshotV1 {
            domain: test_domain("hint"),
            committed_at_ms: 42,
            authorization_revision: 3,
            exhausted: true,
            revoked_host_event_ids: vec!["host-gone".to_owned()],
            revoked_fragment_keys: vec![5, 9],
            highest_source_order: Some(7),
            fragments: vec![SnapshotFragment {
                key: 3,
                zone: RetentionZone::Recycle,
                created_at_ms: 1,
                normal_deadline_ms: 10,
                hard_deadline_ms: 30,
                final_deadline_ms: Some(20),
                lifecycle_version: 2,
                processed_through: 1,
                events: vec![test_event(EventRole::User)],
            }],
        };
        assert_eq!(
            snapshot.content_bytes() + 24,
            snapshot.encode().unwrap().len()
        );
    }

    #[test]
    fn content_bytes_counts_every_optional_tag_combination() {
        for mask in 0..64 {
            let mut event = test_event(EventRole::User);
            event.source_timestamp_ms = (mask & 4 != 0).then_some(3);
            event.source_order = (mask & 8 != 0).then_some(9);
            event.client_request_hint = (mask & 16 != 0).then(|| "client-1".to_owned());
            event.host_event_id = (mask & 32 != 0).then(|| "host-1".to_owned());
            let snapshot = RawSnapshotV1 {
                domain: test_domain("hint"),
                committed_at_ms: 42,
                authorization_revision: 1,
                exhausted: false,
                revoked_host_event_ids: Vec::new(),
                revoked_fragment_keys: Vec::new(),
                highest_source_order: (mask & 1 != 0).then_some(7),
                fragments: vec![SnapshotFragment {
                    key: 1,
                    zone: RetentionZone::Normal,
                    created_at_ms: 1,
                    normal_deadline_ms: 10,
                    hard_deadline_ms: 30,
                    final_deadline_ms: (mask & 2 != 0).then_some(20),
                    lifecycle_version: 0,
                    processed_through: 1,
                    events: vec![event],
                }],
            };
            let encoded = snapshot.encode().unwrap();
            assert_eq!(snapshot.content_bytes() + 24, encoded.len());
            assert_eq!(RawSnapshotV1::decode(&encoded).unwrap(), snapshot);
        }
    }

    #[test]
    fn normal_final_deadline_and_confirmed_prefix_are_consistent() {
        let snapshot = RawSnapshotV1 {
            domain: test_domain("hint"),
            committed_at_ms: 42,
            authorization_revision: 1,
            exhausted: false,
            revoked_host_event_ids: Vec::new(),
            revoked_fragment_keys: Vec::new(),
            highest_source_order: None,
            fragments: vec![SnapshotFragment {
                key: 1,
                zone: RetentionZone::Normal,
                created_at_ms: 1,
                normal_deadline_ms: 10,
                hard_deadline_ms: 30,
                final_deadline_ms: Some(9),
                lifecycle_version: 0,
                processed_through: 1,
                events: vec![test_event(EventRole::User)],
            }],
        };
        assert_eq!(
            RawSnapshotV1::decode(&snapshot.encode().unwrap()),
            Err(snapshot_error("normal deadline after final deadline"))
        );
        let mut recycled = snapshot.clone();
        recycled.fragments[0].zone = RetentionZone::Recycle;
        assert_eq!(
            RawSnapshotV1::decode(&recycled.encode().unwrap()).unwrap(),
            recycled
        );

        let mut inconsistent = snapshot;
        inconsistent.fragments[0].final_deadline_ms = Some(20);
        inconsistent.fragments[0].processed_through = 0;
        assert_eq!(
            RawSnapshotV1::decode(&inconsistent.encode().unwrap()),
            Err(snapshot_error(
                "processed watermark precedes confirmed prefix"
            ))
        );
        assert!(
            inconsistent
                .sweep_expired(42, &RawRetention::new(10, 20, 2, 8, 128).unwrap())
                .is_err()
        );
        inconsistent.fragments[0].events[0].acknowledgement = None;
        let mut later = test_event(EventRole::User);
        later.host_event_id = Some("host-2".to_owned());
        inconsistent.fragments[0].events.push(later);
        assert_eq!(
            RawSnapshotV1::decode(&inconsistent.encode().unwrap()).unwrap(),
            inconsistent
        );
    }

    #[test]
    fn import_record_ledger_byte_budget_rejects_before_publish() {
        use crate::{IngressToken, RawIngressCall, SessionLimits, SessionTiming};
        let mut ingress = TrustedIngress::new();
        ingress
            .register_route(
                "buddy",
                PetIdentity::new(Namespace::new("workspace", "owner").unwrap(), "pet").unwrap(),
            )
            .unwrap();
        let credential = ingress
            .install_credential(
                IngressToken::new("tok").unwrap(),
                CallerSubject::new("caller").unwrap(),
                ["buddy".to_owned()],
                ChannelId::new("channel").unwrap(),
                ChannelScope::new("scope").unwrap(),
                ReplyTarget::new("reply").unwrap(),
            )
            .unwrap();
        ingress
            .bind_stable_credential(credential, "cred-1")
            .unwrap();
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            SessionLimits::new(8, 16, 64, 1_024, 8_192, 2).unwrap(),
            SessionTiming::default(),
            RawRetention::new(1_000_000, 1_000_000, 8, 128, 8_192).unwrap(),
            0,
        );
        fn admit_hint(ingress: &mut TrustedIngress, hint: &str) -> TrustedRequest {
            ingress
                .admit(
                    RawIngressCall::new(IngressToken::new("tok").unwrap(), "buddy", "合成正文")
                        .with_session_hint(hint),
                )
                .unwrap()
        }
        let fence_snapshot = |domain: &RawDomainKey, id: &str| RawSnapshotV1 {
            domain: domain.clone(),
            committed_at_ms: 10,
            authorization_revision: 1,
            exhausted: false,
            revoked_host_event_ids: vec![id.to_owned()],
            revoked_fragment_keys: Vec::new(),
            highest_source_order: None,
            fragments: Vec::new(),
        };
        for (hint, id) in [("hint-a", "fence-a"), ("hint-b", "fence-b")] {
            let request = admit_hint(&mut ingress, hint);
            let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
            let grant = runtime
                .install_raw_history_grant(&ingress, domain.clone())
                .unwrap();
            runtime
                .import_raw_snapshot(
                    SessionAccess::new(&ingress, &request, 10),
                    &grant,
                    &fence_snapshot(&domain, id),
                )
                .unwrap();
        }
        runtime.raw_imports[1].content_bytes = MAX_RAW_IMPORT_LEDGER_BYTES;
        let request_c = admit_hint(&mut ingress, "hint-c");
        let domain_c = RawDomainKey::from_request(&ingress, &request_c).unwrap();
        let grant_c = runtime
            .install_raw_history_grant(&ingress, domain_c.clone())
            .unwrap();
        let snapshot_c = fence_snapshot(&domain_c, "fence-c");
        assert_eq!(
            runtime.import_raw_snapshot(
                SessionAccess::new(&ingress, &request_c, 10),
                &grant_c,
                &snapshot_c,
            ),
            Err(GatewayError::CapacityExceeded)
        );
        assert_eq!(
            runtime.import_raw_snapshot(
                SessionAccess::new(&ingress, &request_c, 11),
                &grant_c,
                &snapshot_c,
            ),
            Err(GatewayError::CapacityExceeded),
            "预算拒绝不得留下半记录把重试变成版本冲突"
        );
        assert_eq!(runtime.raw_imports.len(), 2);
    }

    fn check_import_ledger_cleanup(automatic: bool) {
        use crate::{
            IngressToken, ManagementPermissions, RawIngressCall, SessionLimits, SessionTiming,
        };

        let mut ingress = TrustedIngress::new();
        let domain = test_domain("hint");
        ingress
            .register_route("buddy", domain.identity.clone())
            .unwrap();
        let credential = ingress
            .install_credential(
                IngressToken::new("synthetic-cleanup-token").unwrap(),
                domain.subject.clone(),
                ["buddy".to_owned()],
                domain.channel.clone(),
                domain.scope.clone(),
                ReplyTarget::new("reply").unwrap(),
            )
            .unwrap();
        ingress
            .bind_stable_credential(credential, domain.credential())
            .unwrap();
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("synthetic-cleanup-token").unwrap(),
                    "buddy",
                    "new-input",
                )
                .with_session_hint("hint"),
            )
            .unwrap();
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            SessionLimits::new(8, 16, 64, 1_024, 8_192, 2).unwrap(),
            SessionTiming::default(),
            RawRetention::new(1_000, 2_000, 8, 128, 8_192).unwrap(),
            0,
        );
        let grant = runtime
            .install_raw_history_grant(&ingress, domain.clone())
            .unwrap();
        let snapshot = RawSnapshotV1 {
            domain,
            committed_at_ms: 10,
            authorization_revision: 1,
            exhausted: false,
            revoked_host_event_ids: Vec::new(),
            revoked_fragment_keys: Vec::new(),
            highest_source_order: Some(9),
            fragments: vec![SnapshotFragment {
                key: 1,
                zone: RetentionZone::Normal,
                created_at_ms: 0,
                normal_deadline_ms: 1_000,
                hard_deadline_ms: 3_000,
                final_deadline_ms: None,
                lifecycle_version: 0,
                processed_through: 0,
                events: vec![SnapshotEvent {
                    terminal: None,
                    acknowledgement: None,
                    ..test_event(EventRole::User)
                }],
            }],
        };
        let imported = runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 10),
                &grant,
                &snapshot,
            )
            .unwrap();
        let now_ms = if automatic {
            let report = runtime.maintain_retention(&ingress, 3_000).unwrap();
            assert_eq!(report.removed, 1);
            3_000
        } else {
            let session = &imported.fragments[0].session;
            let capability = runtime
                .install_management_capability(
                    &ingress,
                    request.identity().clone(),
                    request.isolation().clone(),
                    vec![session.clone()],
                    ManagementPermissions::default().with_purge(),
                )
                .unwrap();
            runtime
                .purge_fragment(
                    SessionAccess::new(&ingress, &request, 20),
                    &capability,
                    session,
                    0,
                )
                .unwrap();
            20
        };
        assert!(runtime.sessions.is_empty());
        let RawImportRecord {
            domain,
            fingerprint,
            fragment_keys,
            committed_at_ms,
            content_bytes,
        } = &runtime.raw_imports[0];
        assert_eq!(domain, snapshot.domain());
        assert_eq!(*fingerprint, snapshot.content_fingerprint().unwrap());
        assert_eq!(fragment_keys, &BTreeSet::from([1]));
        assert_eq!(*committed_at_ms, 10);
        assert_eq!(*content_bytes, snapshot.content_bytes());
        let duplicate = runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, now_ms),
                &grant,
                &snapshot,
            )
            .unwrap();
        assert!(duplicate.duplicate);
        assert!(duplicate.fragments.is_empty());
        let mut changed = snapshot;
        changed.fragments[0].events[0].body.push_str("changed");
        assert_eq!(
            runtime.import_raw_snapshot(
                SessionAccess::new(&ingress, &request, now_ms),
                &grant,
                &changed,
            ),
            Err(GatewayError::VersionConflict)
        );
    }

    #[test]
    fn purge_releases_import_ledger_plaintext_without_allowing_reimport() {
        check_import_ledger_cleanup(false);
    }

    #[test]
    fn final_expiry_releases_import_ledger_plaintext_without_allowing_reimport() {
        check_import_ledger_cleanup(true);
    }

    #[test]
    fn sweep_transfer_uses_original_normal_deadline_and_overflows_closed() {
        let retention = RawRetention::new(1_000, 1_000, 8, 128, 8_192).unwrap();
        let fragment = |version: u64, normal: u64, hard: u64| SnapshotFragment {
            key: 1,
            zone: RetentionZone::Normal,
            created_at_ms: 0,
            normal_deadline_ms: normal,
            hard_deadline_ms: hard,
            final_deadline_ms: None,
            lifecycle_version: version,
            processed_through: 0,
            events: Vec::new(),
        };
        let snapshot = RawSnapshotV1 {
            domain: test_domain("hint"),
            committed_at_ms: 0,
            authorization_revision: 1,
            exhausted: false,
            revoked_host_event_ids: Vec::new(),
            revoked_fragment_keys: Vec::new(),
            highest_source_order: None,
            fragments: vec![fragment(0, 1_000, 3_000)],
        };
        let untouched = snapshot.sweep_expired(500, &retention).unwrap();
        assert_eq!(
            (untouched.removed, untouched.transferred, untouched.retained),
            (0, 0, 1)
        );
        assert_eq!(untouched.snapshot.fragments[0].lifecycle_version, 0);
        let swept = snapshot.sweep_expired(1_500, &retention).unwrap();
        assert_eq!(
            (swept.removed, swept.transferred, swept.retained),
            (0, 1, 0)
        );
        let swept_fragment = &swept.snapshot.fragments[0];
        assert_eq!(swept_fragment.zone, RetentionZone::Recycle);
        assert_eq!(swept_fragment.final_deadline_ms, Some(2_000));
        assert_eq!(swept_fragment.lifecycle_version, 1);
        let swept_final = snapshot.sweep_expired(3_000, &retention).unwrap();
        assert_eq!(
            (
                swept_final.removed,
                swept_final.transferred,
                swept_final.retained
            ),
            (1, 0, 0)
        );
        let exhausted = RawSnapshotV1 {
            fragments: vec![fragment(u64::MAX, 1_000, 3_000)],
            ..snapshot
        };
        assert_eq!(
            exhausted.sweep_expired(1_500, &retention).unwrap_err(),
            GatewayError::IdentifierExhausted
        );
        assert_eq!(exhausted.fragments[0].lifecycle_version, u64::MAX);
    }

    #[test]
    fn sweep_persists_tightened_deadlines_for_keep_transfer_and_recycle() {
        let strict = RawRetention::new(100, 200, 8, 128, 8_192).unwrap();
        let wide = RawRetention::new(1_000, 2_000, 8, 128, 8_192).unwrap();
        for (zone, now_ms, saved_final, expected_zone, expected_final) in [
            (RetentionZone::Normal, 50, None, RetentionZone::Normal, None),
            (
                RetentionZone::Normal,
                100,
                None,
                RetentionZone::Recycle,
                Some(300),
            ),
            (
                RetentionZone::Recycle,
                50,
                Some(2_500),
                RetentionZone::Recycle,
                Some(300),
            ),
        ] {
            let snapshot = RawSnapshotV1 {
                domain: test_domain("hint"),
                committed_at_ms: 10,
                authorization_revision: 1,
                exhausted: false,
                revoked_host_event_ids: Vec::new(),
                revoked_fragment_keys: Vec::new(),
                highest_source_order: Some(9),
                fragments: vec![SnapshotFragment {
                    key: 1,
                    zone,
                    created_at_ms: 0,
                    normal_deadline_ms: 1_000,
                    hard_deadline_ms: 3_000,
                    final_deadline_ms: saved_final,
                    lifecycle_version: 0,
                    processed_through: 1,
                    events: vec![test_event(EventRole::User)],
                }],
            };
            let swept = snapshot.sweep_expired(now_ms, &strict).unwrap();
            let fragment = &swept.snapshot.fragments[0];
            assert_eq!(
                (fragment.normal_deadline_ms, fragment.hard_deadline_ms),
                (100, 300)
            );
            assert_eq!(
                (fragment.zone, fragment.final_deadline_ms),
                (expected_zone, expected_final)
            );
            assert!(fragment.events == snapshot.fragments[0].events);
            let decoded = RawSnapshotV1::decode(&swept.snapshot.encode().unwrap()).unwrap();
            let widened = decoded.sweep_expired(now_ms, &wide).unwrap();
            assert!(widened.snapshot.fragments == swept.snapshot.fragments);
            assert_eq!(decoded.sweep_expired(300, &wide).unwrap().removed, 1);
        }
    }

    #[test]
    fn v1_codec_roundtrips_deterministically() {
        let snapshot = RawSnapshotV1 {
            domain: test_domain("hint"),
            committed_at_ms: 42,
            authorization_revision: 2,
            exhausted: true,
            revoked_host_event_ids: vec!["host-gone".to_owned()],
            revoked_fragment_keys: vec![11],
            highest_source_order: Some(7),
            fragments: vec![SnapshotFragment {
                key: 3,
                zone: RetentionZone::Recycle,
                created_at_ms: 1,
                normal_deadline_ms: 10,
                hard_deadline_ms: 30,
                final_deadline_ms: Some(20),
                lifecycle_version: 2,
                processed_through: 1,
                events: vec![
                    test_event(EventRole::User),
                    SnapshotEvent {
                        parent_ordinal: 1,
                        terminal: None,
                        acknowledgement: None,
                        source_order: None,
                        client_request_hint: None,
                        host_event_id: None,
                        ..test_event(EventRole::Assistant)
                    },
                ],
            }],
        };
        let bytes = snapshot.encode().unwrap();
        assert_eq!(bytes, snapshot.encode().unwrap());
        assert_eq!(RawSnapshotV1::decode(&bytes).unwrap(), snapshot);
    }

    #[test]
    fn v1_codec_rejects_corruption() {
        let snapshot = RawSnapshotV1 {
            domain: test_domain("hint"),
            committed_at_ms: 42,
            authorization_revision: 1,
            exhausted: false,
            revoked_host_event_ids: Vec::new(),
            revoked_fragment_keys: Vec::new(),
            highest_source_order: None,
            fragments: Vec::new(),
        };
        let bytes = snapshot.encode().unwrap();
        let mut magic = bytes.clone();
        magic[0] = b"X"[0];
        assert_eq!(
            RawSnapshotV1::decode(&magic),
            Err(GatewayError::RawSnapshotInvalid("unknown magic"))
        );
        let mut version = bytes.clone();
        version[8] = 9;
        assert_eq!(
            RawSnapshotV1::decode(&version),
            Err(GatewayError::RawSnapshotInvalid(
                "unsupported format version"
            ))
        );
        let mut checksum = bytes.clone();
        checksum[24] ^= 0xff;
        assert_eq!(
            RawSnapshotV1::decode(&checksum),
            Err(GatewayError::RawSnapshotInvalid(
                "content checksum mismatch"
            ))
        );
        let mut committed = bytes.clone();
        committed[16] ^= 0xff;
        assert_eq!(
            RawSnapshotV1::decode(&committed),
            Err(GatewayError::RawSnapshotInvalid(
                "content checksum mismatch"
            ))
        );
        assert_eq!(
            RawSnapshotV1::decode(&bytes[..20]),
            Err(GatewayError::RawSnapshotInvalid("truncated header"))
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        let repaired_trailing = rewrite_checksum(trailing);
        assert_eq!(
            RawSnapshotV1::decode(&repaired_trailing),
            Err(GatewayError::RawSnapshotInvalid(
                "trailing bytes after content"
            ))
        );
        let oversize = vec![0u8; RAW_SNAPSHOT_MAX_TOTAL_BYTES + 1];
        assert_eq!(
            RawSnapshotV1::decode(&oversize),
            Err(GatewayError::RawSnapshotInvalid(
                "snapshot exceeds total byte budget"
            ))
        );
        let with_event = RawSnapshotV1 {
            fragments: vec![SnapshotFragment {
                key: 1,
                zone: RetentionZone::Normal,
                created_at_ms: 1,
                normal_deadline_ms: 10,
                hard_deadline_ms: 30,
                final_deadline_ms: None,
                lifecycle_version: 0,
                processed_through: 0,
                events: vec![test_event(EventRole::User)],
            }],
            ..snapshot
        };
        let mut acknowledgement = with_event.encode().unwrap();
        let last = acknowledgement.len() - 1;
        acknowledgement[last] = 9;
        let repaired = rewrite_checksum(acknowledgement);
        assert_eq!(
            RawSnapshotV1::decode(&repaired),
            Err(GatewayError::RawSnapshotInvalid("unknown acknowledgement"))
        );
    }

    fn rewrite_checksum(mut bytes: Vec<u8>) -> Vec<u8> {
        let checksum = fnv1a(&bytes[16..]);
        bytes[12..16].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }
}
