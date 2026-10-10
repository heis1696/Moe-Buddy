use super::{Command, MemoryCommand};
use crate::model::SourceKey;
use crate::{
    Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryId, MemoryScope, Namespace,
    PetIdentity, RecordVersion, Sensitivity, SessionId, SourceRef, validation,
};

const MAX_COMMAND_BYTES: usize = 128 * 1024;

impl MemoryCommand {
    pub fn to_bytes(&self) -> Result<Vec<u8>, MemoryError> {
        let mut writer = Encoder(vec![self.1]);
        match &self.0 {
            Command::Source(scope, id, revision, epistemic) => {
                writer.byte(0);
                writer.scope(scope)?;
                writer.string(id)?;
                writer.number(*revision);
                writer.byte(epistemic.tag());
            }
            Command::Create(identity, draft, now_ms) => {
                writer.byte(1);
                writer.identity(identity)?;
                writer.draft(draft, self.1)?;
                writer.number(*now_ms);
            }
            Command::Update(identity, id, revision, draft, now_ms) => {
                writer.byte(2);
                writer.identity(identity)?;
                writer.number(id.0);
                writer.number(*revision);
                writer.draft(draft, self.1)?;
                writer.number(*now_ms);
            }
            Command::Delete(identity, id, revision, now_ms) => {
                writer.byte(3);
                writer.identity(identity)?;
                writer.number(id.0);
                writer.number(*revision);
                writer.number(*now_ms);
            }
            Command::Archive(identity, id, revision, now_ms)
            | Command::RestoreArchive(identity, id, revision, now_ms) => {
                writer.byte(if matches!(self.0, Command::Archive(..)) {
                    4
                } else {
                    5
                });
                writer.identity(identity)?;
                writer.number(id.0);
                writer.number(*revision);
                writer.number(*now_ms);
            }
            Command::Replace(identity, id, revision, draft, now_ms) => {
                writer.byte(6);
                writer.identity(identity)?;
                writer.number(id.0);
                writer.number(*revision);
                writer.draft(draft, 2)?;
                writer.number(*now_ms);
            }
            Command::RevokeSource(source) | Command::RestoreSource(source, _) => {
                writer.byte(if matches!(self.0, Command::RevokeSource(_)) {
                    7
                } else {
                    8
                });
                writer.scope(&source.key.scope)?;
                writer.string(&source.key.id)?;
                writer.number(source.revision);
                if let Command::RestoreSource(_, revision) = &self.0 {
                    writer.number(*revision);
                }
            }
            Command::Cancel(generation) => {
                writer.byte(9);
                writer.number(*generation);
            }
            Command::FencedCreate(identity, draft, now_ms, stamp) => {
                writer.byte(10);
                writer.identity(identity)?;
                writer.draft(draft, 2)?;
                writer.number(*now_ms);
                writer.number(stamp.generation);
                writer.number(stamp.configuration);
                writer.number(stamp.policy);
                writer.string(stamp.session.as_str())?;
                writer.number(stamp.request.len() as u64);
                writer.0.extend_from_slice(&stamp.request);
            }
            Command::PublishShared(identity, draft, now_ms, stamp, policy) => {
                writer.byte(11);
                writer.identity(identity)?;
                writer.draft(draft, 2)?;
                writer.number(*now_ms);
                writer.task_stamp(stamp)?;
                writer.number(*policy);
            }
            Command::Formal(namespace, target, draft, now_ms, stamp) => {
                writer.byte(if target.is_some() { 13 } else { 12 });
                writer.namespace(namespace)?;
                if let Some((id, revision)) = target {
                    writer.number(id.0);
                    writer.number(*revision);
                }
                writer.draft(draft, 2)?;
                writer.number(*now_ms);
                writer.number(stamp.generation);
                writer.number(stamp.configuration);
                writer.number(stamp.policy);
            }
        }
        if writer.0.len() > MAX_COMMAND_BYTES {
            return Err(invalid());
        }
        Ok(writer.0)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MemoryError> {
        if bytes.len() > MAX_COMMAND_BYTES {
            return Err(invalid());
        }
        let mut reader = Decoder(bytes);
        let version = reader.byte()?;
        if !matches!(version, 1 | 2) {
            return Err(invalid());
        }
        let tag = reader.byte()?;
        if version == 1 && tag > 3 {
            return Err(invalid());
        }
        let command = match tag {
            0 => Command::Source(
                reader.scope()?,
                reader.identifier()?,
                reader.positive()?,
                reader.enumeration()?,
            ),
            1 => Command::Create(reader.identity()?, reader.draft(version)?, reader.number()?),
            2 => Command::Update(
                reader.identity()?,
                MemoryId(reader.positive()?),
                reader.number()?,
                reader.draft(version)?,
                reader.number()?,
            ),
            3 => Command::Delete(
                reader.identity()?,
                MemoryId(reader.positive()?),
                reader.number()?,
                reader.number()?,
            ),
            4 | 5 => {
                let identity = reader.identity()?;
                let id = MemoryId(reader.positive()?);
                let revision = reader.number()?;
                let now_ms = reader.number()?;
                if tag == 4 {
                    Command::Archive(identity, id, revision, now_ms)
                } else {
                    Command::RestoreArchive(identity, id, revision, now_ms)
                }
            }
            6 => Command::Replace(
                reader.identity()?,
                MemoryId(reader.positive()?),
                reader.number()?,
                reader.draft(2)?,
                reader.number()?,
            ),
            7 => Command::RevokeSource(reader.source()?),
            8 => Command::RestoreSource(reader.source()?, reader.positive()?),
            9 => Command::Cancel(reader.number()?),
            10 => Command::FencedCreate(
                reader.identity()?,
                reader.draft(2)?,
                reader.number()?,
                crate::management::TaskStamp {
                    generation: reader.number()?,
                    configuration: reader.number()?,
                    policy: reader.positive()?,
                    session: SessionId::new(reader.identifier()?)?,
                    request: reader.request()?,
                },
            ),
            11 => Command::PublishShared(
                reader.identity()?,
                reader.draft(2)?,
                reader.number()?,
                reader.task_stamp()?,
                reader.positive()?,
            ),
            12 | 13 => {
                let namespace = reader.namespace()?;
                let target = if tag == 13 {
                    Some((MemoryId(reader.positive()?), reader.positive()?))
                } else {
                    None
                };
                Command::Formal(
                    namespace,
                    target,
                    reader.draft(2)?,
                    reader.number()?,
                    crate::publication::FormalStamp {
                        generation: reader.number()?,
                        configuration: reader.number()?,
                        policy: reader.positive()?,
                    },
                )
            }
            _ => return Err(invalid()),
        };
        if !reader.0.is_empty() {
            return Err(invalid());
        }
        Ok(Self(command, version, None))
    }
}

fn invalid() -> MemoryError {
    MemoryError::InvalidInput("invalid versioned command")
}

trait WireEnum: Sized + Copy {
    fn tag(self) -> u8;
    fn from_tag(tag: u8) -> Result<Self, MemoryError>;
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $tag:literal),+ $(,)? }) => {
        impl WireEnum for $name {
            fn tag(self) -> u8 {
                match self { $(Self::$variant => $tag),+ }
            }
            fn from_tag(tag: u8) -> Result<Self, MemoryError> {
                match tag { $($tag => Ok(Self::$variant)),+, _ => Err(invalid()) }
            }
        }
    };
}

wire_enum!(Category {
    Identity = 0, UserProfile = 1, Relationship = 2, Episode = 3, WorkingContext = 4,
    Knowledge = 5, Procedure = 6, ToolDefinition = 7, Protocol = 8,
});
wire_enum!(Epistemic {
    UserStatement = 0, Observation = 1, Inference = 2, Fiction = 3, Reference = 4,
    AuthoritativeState = 5,
});
wire_enum!(Lifecycle { ShortTerm = 0, LongTerm = 1 });
wire_enum!(Sensitivity { Normal = 0, Private = 1, Restricted = 2 });

struct Encoder(Vec<u8>);

impl Encoder {
    fn task_stamp(&mut self, stamp: &crate::management::TaskStamp) -> Result<(), MemoryError> {
        self.number(stamp.generation);
        self.number(stamp.configuration);
        self.number(stamp.policy);
        self.string(stamp.session.as_str())?;
        self.number(stamp.request.len() as u64);
        self.0.extend_from_slice(&stamp.request);
        Ok(())
    }
    fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    fn number(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), MemoryError> {
        if value.len() > validation::MAX_TEXT_CHARS * 4 {
            return Err(invalid());
        }
        self.number(value.len() as u64);
        self.0.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn namespace(&mut self, namespace: &Namespace) -> Result<(), MemoryError> {
        self.string(namespace.workspace_id())?;
        self.string(namespace.user_id())
    }

    fn identity(&mut self, identity: &PetIdentity) -> Result<(), MemoryError> {
        self.namespace(identity.namespace())?;
        self.string(identity.instance_id())
    }

    fn scope(&mut self, scope: &MemoryScope) -> Result<(), MemoryError> {
        match scope {
            MemoryScope::Private(identity) => {
                self.byte(0);
                self.identity(identity)
            }
            MemoryScope::Shared(namespace) => {
                self.byte(1);
                self.namespace(namespace)
            }
        }
    }

    fn draft(&mut self, draft: &MemoryDraft, version: u8) -> Result<(), MemoryError> {
        self.string(&draft.text)?;
        self.byte(draft.category.tag());
        self.byte(draft.epistemic.tag());
        self.byte(draft.lifecycle.tag());
        self.byte(draft.sensitivity.tag());
        match draft.expires_at_ms {
            Some(expiration) => {
                self.byte(1);
                self.number(expiration);
            }
            None => self.byte(0),
        }
        if draft.sources.len() > validation::MAX_SOURCES {
            return Err(invalid());
        }
        self.number(draft.sources.len() as u64);
        for source in &draft.sources {
            self.scope(&source.key.scope)?;
            self.string(&source.key.id)?;
            self.number(source.revision);
        }
        if version == 2 {
            if draft.dependencies.len() > validation::MAX_SOURCES {
                return Err(invalid());
            }
            self.number(draft.dependencies.len() as u64);
            for dependency in &draft.dependencies {
                self.number(dependency.id().0);
                self.number(dependency.revision());
            }
        }
        Ok(())
    }
}

struct Decoder<'bytes>(&'bytes [u8]);

impl Decoder<'_> {
    fn task_stamp(&mut self) -> Result<crate::management::TaskStamp, MemoryError> {
        Ok(crate::management::TaskStamp {
            generation: self.number()?,
            configuration: self.number()?,
            policy: self.positive()?,
            session: SessionId::new(self.identifier()?)?,
            request: self.request()?,
        })
    }
    fn take(&mut self, length: usize) -> Result<&[u8], MemoryError> {
        let (value, remaining) = self.0.split_at_checked(length).ok_or_else(invalid)?;
        self.0 = remaining;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, MemoryError> {
        Ok(self.take(1)?[0])
    }

    fn number(&mut self) -> Result<u64, MemoryError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| invalid())?,
        ))
    }

    fn positive(&mut self) -> Result<u64, MemoryError> {
        let value = self.number()?;
        if value == 0 {
            return Err(invalid());
        }
        Ok(value)
    }

    fn enumeration<Value: WireEnum>(&mut self) -> Result<Value, MemoryError> {
        Value::from_tag(self.byte()?)
    }

    fn string(&mut self, max_bytes: usize) -> Result<String, MemoryError> {
        let length = usize::try_from(self.number()?).map_err(|_| invalid())?;
        if length > max_bytes {
            return Err(invalid());
        }
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| invalid())
    }

    fn identifier(&mut self) -> Result<String, MemoryError> {
        let value = self.string(128 * 4)?;
        validation::identifier(&value)?;
        Ok(value)
    }

    fn namespace(&mut self) -> Result<Namespace, MemoryError> {
        Namespace::new(self.identifier()?, self.identifier()?)
    }

    fn identity(&mut self) -> Result<PetIdentity, MemoryError> {
        PetIdentity::new(self.namespace()?, self.identifier()?)
    }

    fn scope(&mut self) -> Result<MemoryScope, MemoryError> {
        match self.byte()? {
            0 => Ok(MemoryScope::Private(self.identity()?)),
            1 => Ok(MemoryScope::Shared(self.namespace()?)),
            _ => Err(invalid()),
        }
    }

    fn source(&mut self) -> Result<SourceRef, MemoryError> {
        Ok(SourceRef {
            key: SourceKey {
                scope: self.scope()?,
                id: self.identifier()?,
            },
            revision: self.positive()?,
        })
    }

    fn request(&mut self) -> Result<Vec<u8>, MemoryError> {
        let length = usize::try_from(self.number()?).map_err(|_| invalid())?;
        if length > MAX_COMMAND_BYTES {
            return Err(invalid());
        }
        Ok(self.take(length)?.to_vec())
    }

    fn draft(&mut self, version: u8) -> Result<MemoryDraft, MemoryError> {
        let text = self.string(validation::MAX_TEXT_CHARS * 4)?;
        validation::text(&text)?;
        let category = self.enumeration()?;
        let epistemic = self.enumeration()?;
        let lifecycle = self.enumeration()?;
        let sensitivity = self.enumeration()?;
        let expires_at_ms = match self.byte()? {
            0 => None,
            1 => Some(self.number()?),
            _ => return Err(invalid()),
        };
        let count = usize::try_from(self.number()?).map_err(|_| invalid())?;
        if count > validation::MAX_SOURCES {
            return Err(invalid());
        }
        let mut sources = Vec::with_capacity(count);
        for _ in 0..count {
            sources.push(SourceRef {
                key: SourceKey {
                    scope: self.scope()?,
                    id: self.identifier()?,
                },
                revision: self.positive()?,
            });
        }
        let mut dependencies = Vec::new();
        if version == 2 {
            let count = usize::try_from(self.number()?).map_err(|_| invalid())?;
            if count > validation::MAX_SOURCES {
                return Err(invalid());
            }
            for _ in 0..count {
                dependencies.push(RecordVersion::new(
                    MemoryId(self.positive()?),
                    self.positive()?,
                )?);
            }
        }
        Ok(MemoryDraft {
            text,
            category,
            epistemic,
            sources,
            lifecycle,
            sensitivity,
            expires_at_ms,
            dependencies,
        })
    }
}
