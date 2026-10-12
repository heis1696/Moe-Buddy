use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, Error as DeError, MapAccess, SeqAccess, Visitor};

use crate::reader::MAX_HOOK_INPUT_BYTES;
use crate::version::VerifiedCliVersion;

/// 本适配器处理的唯一事件名；其它值按拒绝处理。
const EVENT_NAME: &str = "UserPromptSubmit";
/// `session_id` 最大字节数。已核验 CLI 发送 UUID 形式（36 字符）；
/// 该上限只拒绝荒谬输入，不施加格式约束。
pub const MAX_SESSION_ID_BYTES: usize = 256;
/// 容器（对象/数组）最大嵌套深度，根对象计为深度 1。
pub const MAX_CONTAINER_DEPTH: usize = 64;

const FIELD_SESSION_ID: &str = "session_id";
const FIELD_HOOK_EVENT_NAME: &str = "hook_event_name";
const FIELD_PROMPT: &str = "prompt";
const MSG_DUPLICATE_KEY: &str = "moe-cli-hooks: duplicate object key";
const MSG_TOO_DEEP: &str = "moe-cli-hooks: container nesting too deep";
const MSG_NOT_AN_OBJECT: &str = "moe-cli-hooks: expected a single JSON object";
const MSG_SESSION_ID_TOO_LONG: &str = "moe-cli-hooks: session_id too long";

#[derive(Clone, Copy, PartialEq, Eq)]
enum RejectKind {
    DuplicateKey,
    TooDeep,
    NotAnObject,
    SessionIdTooLong,
}

/// 解析失败的全部类别。变体不回显任何输入内容，只携带类别、上限与
/// 解析器位置（行/列）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookInputError(HookErrorKind);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookErrorKind {
    NotUtf8,
    TooLong { limit: usize },
    DuplicateKey { line: usize, column: usize },
    TooDeep { limit: usize },
    NotAnObject { line: usize, column: usize },
    NotAString { line: usize, column: usize },
    MissingField { field: &'static str },
    SessionIdTooLong { limit: usize },
    EventMismatch,
    InvalidJson { line: usize, column: usize },
}

impl fmt::Display for HookInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            HookErrorKind::NotUtf8 => formatter.write_str("hook input is not valid UTF-8"),
            HookErrorKind::TooLong { limit } => {
                write!(
                    formatter,
                    "hook input exceeds the {}-byte input limit",
                    limit
                )
            }
            HookErrorKind::DuplicateKey { line, column } => {
                write!(
                    formatter,
                    "duplicate object key at line {} column {}",
                    line, column
                )
            }
            HookErrorKind::TooDeep { limit } => {
                write!(
                    formatter,
                    "hook input exceeds the container depth limit of {}",
                    limit
                )
            }
            HookErrorKind::NotAnObject { line, column } => write!(
                formatter,
                "hook input is not a JSON object at line {} column {}",
                line, column
            ),
            HookErrorKind::NotAString { line, column } => write!(
                formatter,
                "hook field is not a JSON string at line {} column {}",
                line, column
            ),
            HookErrorKind::MissingField { field } => {
                write!(formatter, "required hook field `{}` is missing", field)
            }
            HookErrorKind::SessionIdTooLong { limit } => {
                write!(formatter, "session_id exceeds the {}-byte limit", limit)
            }
            HookErrorKind::EventMismatch => {
                write!(formatter, "hook_event_name is not `{}`", EVENT_NAME)
            }
            HookErrorKind::InvalidJson { line, column } => {
                write!(
                    formatter,
                    "invalid JSON syntax at line {} column {}",
                    line, column
                )
            }
        }
    }
}

impl std::error::Error for HookInputError {}

/// 一个被接纳的 `UserPromptSubmit` 载荷。
///
/// 不变量：有界、无重复键，且只剩两个获准数据字段。按
/// docs/GATEWAY.md §1，这是 **UnverifiedPrompt**：prompt 不被认证为
/// 人类发言（外部通知也走同一入口，stdin 没有可信角色标志），不得登记为
/// 用户事件、不得进入用户推理入口、不得转为长期证据；输入中的 role、
/// owner、权限、取消或回复目标字段已被丢弃，不产生任何权力。
pub struct ClaudePromptSubmission {
    session_id: String,
    prompt: String,
}

impl ClaudePromptSubmission {
    /// CLI 会话标识。仅是 opaque 关联键：不能独立认证、绑定身份或按其
    /// 去重（§1）；缺少逐事件 ID 时不按相同文本自动合并。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 未核验的 prompt 文本，与输入 JSON 解码结果逐字一致。
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
}

impl fmt::Debug for ClaudePromptSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudePromptSubmission")
            .field("session_id", &"REDACTED")
            .field(
                "prompt",
                &format_args!("<redacted: {} bytes>", self.prompt.len()),
            )
            .finish()
    }
}

/// 解析并校验一个 `UserPromptSubmit` 载荷。
///
/// `version` 必须来自 [`VerifiedCliVersion`]（唯一可构造值为 2.1.286）。
/// 载荷本身不含版本，该参数把契约版本钉在调用处。
///
/// 语义（docs/GATEWAY.md §1/§3）：
/// - 只保留 `session_id`、`hook_event_name`、`prompt`；其余字段（包括
///   `transcript_path` 等条件字段）只做有界语法检查后丢弃；
/// - 重复键在建对象前递归拒绝（转义等价拼写计为重复），官方
///   JSON.parse 后值覆盖的行为不可能被用来夹带字段；
/// - `hook_event_name` 必须存在且精确等于 `UserPromptSubmit`；
/// - 成功只表示"有界、语法合法、获准字段齐全"，不证明 prompt 是人类
///   发言；调用方必须按 `UnverifiedPrompt` 对待 [`ClaudePromptSubmission::prompt`]。
///
/// # Examples
///
/// ```
/// use moe_cli_hooks::{parse_user_prompt_submit, VerifiedCliVersion};
///
/// let raw = br#"{"session_id":"8f0c","hook_event_name":"UserPromptSubmit","prompt":"hello","cwd":"C:\\tmp"}"#;
/// let version = VerifiedCliVersion::from_host_report("2.1.286").unwrap();
/// let submission = parse_user_prompt_submit(version, raw).unwrap();
/// assert_eq!(submission.session_id(), "8f0c");
/// assert_eq!(submission.prompt(), "hello");
/// ```
pub fn parse_user_prompt_submit(
    version: VerifiedCliVersion,
    raw: &[u8],
) -> Result<ClaudePromptSubmission, HookInputError> {
    let _pinned: VerifiedCliVersion = version;
    if raw.len() > MAX_HOOK_INPUT_BYTES {
        return Err(HookInputError(HookErrorKind::TooLong {
            limit: MAX_HOOK_INPUT_BYTES,
        }));
    }
    let text = std::str::from_utf8(raw).map_err(|_| HookInputError(HookErrorKind::NotUtf8))?;
    let reject = Cell::new(None);
    let mut de = serde_json::Deserializer::from_str(text);
    let fields = match de.deserialize_any(RootVisitor { reject: &reject }) {
        Ok(fields) => {
            if let Err(err) = de.end() {
                return Err(surface(&reject, err));
            }
            fields
        }
        Err(err) => return Err(surface(&reject, err)),
    };
    let Fields {
        session_id,
        hook_event_name,
        prompt,
    } = fields;
    let hook_event_name = hook_event_name.ok_or(HookInputError(HookErrorKind::MissingField {
        field: FIELD_HOOK_EVENT_NAME,
    }))?;
    if hook_event_name != EVENT_NAME {
        return Err(HookInputError(HookErrorKind::EventMismatch));
    }
    let session_id = session_id.ok_or(HookInputError(HookErrorKind::MissingField {
        field: FIELD_SESSION_ID,
    }))?;
    let prompt = prompt.ok_or(HookInputError(HookErrorKind::MissingField {
        field: FIELD_PROMPT,
    }))?;
    Ok(ClaudePromptSubmission { session_id, prompt })
}

#[derive(Default)]
struct Fields {
    session_id: Option<String>,
    hook_event_name: Option<String>,
    prompt: Option<String>,
}

fn surface(reject: &Cell<Option<RejectKind>>, err: serde_json::Error) -> HookInputError {
    let line = err.line();
    let column = err.column();
    let kind = match reject.replace(None) {
        Some(RejectKind::DuplicateKey) => HookErrorKind::DuplicateKey { line, column },
        Some(RejectKind::TooDeep) => HookErrorKind::TooDeep {
            limit: MAX_CONTAINER_DEPTH,
        },
        Some(RejectKind::NotAnObject) => HookErrorKind::NotAnObject { line, column },
        Some(RejectKind::SessionIdTooLong) => HookErrorKind::SessionIdTooLong {
            limit: MAX_SESSION_ID_BYTES,
        },
        None => match err.classify() {
            serde_json::error::Category::Data => HookErrorKind::NotAString { line, column },
            _ => HookErrorKind::InvalidJson { line, column },
        },
    };
    HookInputError(kind)
}

struct RootVisitor<'c> {
    reject: &'c Cell<Option<RejectKind>>,
}

impl<'c> RootVisitor<'c> {
    fn reject<E: DeError>(&self, kind: RejectKind, msg: &'static str) -> E {
        self.reject.set(Some(kind));
        DeError::custom(msg)
    }
}

impl<'de> Visitor<'de> for RootVisitor<'_> {
    type Value = Fields;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a single UserPromptSubmit JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut fields = Fields::default();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(self.reject(RejectKind::DuplicateKey, MSG_DUPLICATE_KEY));
            }
            match key.as_str() {
                FIELD_SESSION_ID => {
                    let value = map.next_value::<String>()?;
                    if value.len() > MAX_SESSION_ID_BYTES {
                        return Err(
                            self.reject(RejectKind::SessionIdTooLong, MSG_SESSION_ID_TOO_LONG)
                        );
                    }
                    fields.session_id = Some(value);
                }
                FIELD_HOOK_EVENT_NAME => {
                    fields.hook_event_name = Some(map.next_value::<String>()?);
                }
                FIELD_PROMPT => fields.prompt = Some(map.next_value::<String>()?),
                _ => map.next_value_seed(DiscardSeed {
                    depth: 2,
                    reject: self.reject,
                })?,
            }
        }
        Ok(fields)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _seq: A) -> Result<Fields, A::Error> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }

    fn visit_bool<E: DeError>(self, _v: bool) -> Result<Fields, E> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }

    fn visit_i64<E: DeError>(self, _v: i64) -> Result<Fields, E> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }

    fn visit_u64<E: DeError>(self, _v: u64) -> Result<Fields, E> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }

    fn visit_f64<E: DeError>(self, _v: f64) -> Result<Fields, E> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }

    fn visit_str<E: DeError>(self, _v: &str) -> Result<Fields, E> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }

    fn visit_unit<E: DeError>(self) -> Result<Fields, E> {
        Err(self.reject(RejectKind::NotAnObject, MSG_NOT_AN_OBJECT))
    }
}

/// 有界丢弃种子：对未知值递归做与已知字段相同的语法检查（重复键、
/// 深度、合法性），但一概不保留内容。
#[derive(Clone, Copy)]
struct DiscardSeed<'c> {
    depth: usize,
    reject: &'c Cell<Option<RejectKind>>,
}

impl<'de> DeserializeSeed<'de> for DiscardSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(DiscardVisitor {
            depth: self.depth,
            reject: self.reject,
        })
    }
}

struct DiscardVisitor<'c> {
    depth: usize,
    reject: &'c Cell<Option<RejectKind>>,
}

impl DiscardVisitor<'_> {
    fn reject<E: DeError>(&self, kind: RejectKind, msg: &'static str) -> E {
        self.reject.set(Some(kind));
        DeError::custom(msg)
    }
}

impl<'de> Visitor<'de> for DiscardVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        if self.depth > MAX_CONTAINER_DEPTH {
            return Err(self.reject(RejectKind::TooDeep, MSG_TOO_DEEP));
        }
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(self.reject(RejectKind::DuplicateKey, MSG_DUPLICATE_KEY));
            }
            map.next_value_seed(DiscardSeed {
                depth: self.depth + 1,
                reject: self.reject,
            })?;
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.depth > MAX_CONTAINER_DEPTH {
            return Err(self.reject(RejectKind::TooDeep, MSG_TOO_DEEP));
        }
        while seq
            .next_element_seed(DiscardSeed {
                depth: self.depth + 1,
                reject: self.reject,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_bool<E: DeError>(self, _v: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: DeError>(self, _v: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: DeError>(self, _v: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: DeError>(self, _v: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: DeError>(self, _v: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: DeError>(self) -> Result<(), E> {
        Ok(())
    }
}
