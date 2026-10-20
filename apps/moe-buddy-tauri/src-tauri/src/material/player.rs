use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use moe_agent_runtime::animation::{
    AnimationClip, ClipCapabilities, ClipPlayer, PlaybackAck, PlaybackAdvance, PlaybackBoundary,
    PlaybackMode, PlaybackPosition, PlaybackTicket,
};

use super::manifest::{DecodedClip, MaterialPack};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequencePlayerError {
    UnknownClip,
    ClipVersionMismatch,
    ClipDurationMismatch,
    ActivePlayback,
    NoPreparedClip,
    WrongTicket,
    Paused,
    PlaybackComplete,
    TimeOverflow,
    ReadViewPoisoned,
}

impl fmt::Display for SequencePlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sequence player error: {self:?}")
    }
}
impl Error for SequencePlayerError {}

struct Prepared {
    clip_id: String,
    asset_version: u64,
}
#[derive(Clone)]
struct Playback {
    clip: Arc<DecodedClip>,
    ticket: PlaybackTicket,
    mode: PlaybackMode,
    elapsed_ms: u64,
    paused: bool,
}

#[derive(Clone)]
pub struct ReadOnlyFrameView {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    pub frame_index: usize,
}

#[derive(Clone)]
/// An independent read handle for immutable pixels published by a sequence player.
pub struct SequenceFrameReader {
    published: Arc<Mutex<Option<PublishedFrame>>>,
}

#[derive(Clone)]
struct PublishedFrame {
    ticket: PlaybackTicket,
    frame: ReadOnlyFrameView,
}

impl SequenceFrameReader {
    /// Returns the last published frame only when its playback ticket still matches.
    pub fn current_frame(
        &self,
        ticket: &PlaybackTicket,
    ) -> Result<ReadOnlyFrameView, SequencePlayerError> {
        self.published
            .lock()
            .map_err(|_| SequencePlayerError::ReadViewPoisoned)?
            .as_ref()
            .filter(|published| &published.ticket == ticket)
            .map(|published| published.frame.clone())
            .ok_or(SequencePlayerError::WrongTicket)
    }
}

/// Borrowed pixels for the frame selected by the latest host-driven progress.
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    pub frame_index: usize,
}

/// A concrete variable-duration sequence backend for the existing Animator.
pub struct SequenceClipPlayer {
    pack: MaterialPack,
    prepared: Option<Prepared>,
    playback: Option<Playback>,
    published_frame: Arc<Mutex<Option<PublishedFrame>>>,
}

impl SequenceClipPlayer {
    pub fn new(pack: MaterialPack) -> Self {
        Self {
            pack,
            prepared: None,
            playback: None,
            published_frame: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates a read-only handle that does not retain a mutable playback instance.
    pub fn frame_reader(&self) -> SequenceFrameReader {
        SequenceFrameReader {
            published: Arc::clone(&self.published_frame),
        }
    }

    pub fn current_frame(
        &self,
        ticket: &PlaybackTicket,
    ) -> Result<FrameView<'_>, SequencePlayerError> {
        let playback = self
            .playback
            .as_ref()
            .filter(|playing| &playing.ticket == ticket)
            .ok_or(SequencePlayerError::WrongTicket)?;
        let index = frame_index(&playback.clip, playback.elapsed_ms);
        let frame = &playback.clip.frames[index];
        Ok(FrameView {
            width: frame.width,
            height: frame.height,
            rgba: &frame.rgba,
            frame_index: index,
        })
    }

    pub fn current_action_frame(&self, action_id: &str) -> Option<FrameView<'_>> {
        let action = self.pack.action(action_id)?;
        let playback = self.playback.as_ref()?;
        if playback.clip.info.id != action.clip_id {
            return None;
        }
        let index = frame_index(&playback.clip, playback.elapsed_ms);
        let frame = &playback.clip.frames[index];
        Some(FrameView {
            width: frame.width,
            height: frame.height,
            rgba: &frame.rgba,
            frame_index: index,
        })
    }

    pub fn pack(&self) -> &MaterialPack {
        &self.pack
    }
}

impl ClipPlayer for SequenceClipPlayer {
    type Error = SequencePlayerError;

    fn prepare(
        &mut self,
        clip: &AnimationClip,
        _mode: PlaybackMode,
    ) -> Result<ClipCapabilities, Self::Error> {
        let found = self
            .pack
            .decoded_clip(clip.id())
            .ok_or(SequencePlayerError::UnknownClip)?;
        if clip.asset_version() != self.pack.version() {
            return Err(SequencePlayerError::ClipVersionMismatch);
        }
        if clip.duration_ms() != found.info.duration_ms {
            return Err(SequencePlayerError::ClipDurationMismatch);
        }
        self.prepared = Some(Prepared {
            clip_id: clip.id().to_owned(),
            asset_version: clip.asset_version(),
        });
        Ok(ClipCapabilities { pause_resume: true })
    }

    fn start(
        &mut self,
        clip: &AnimationClip,
        mode: PlaybackMode,
        ticket: &PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error> {
        if self.playback.is_some() {
            return Err(SequencePlayerError::ActivePlayback);
        }
        let prepared = self
            .prepared
            .as_ref()
            .ok_or(SequencePlayerError::NoPreparedClip)?;
        if prepared.clip_id != clip.id() || prepared.asset_version != clip.asset_version() {
            return Err(SequencePlayerError::NoPreparedClip);
        }
        if ticket.clip() != clip.id() || ticket.asset_version() != clip.asset_version() {
            return Err(SequencePlayerError::WrongTicket);
        }
        let decoded = Arc::new(
            self.pack
                .decoded_clip(clip.id())
                .ok_or(SequencePlayerError::UnknownClip)?
                .clone(),
        );
        self.prepared = None;
        self.playback = Some(Playback {
            clip: decoded,
            ticket: ticket.clone(),
            mode,
            elapsed_ms: 0,
            paused: false,
        });
        self.publish_read_view()?;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }

    fn advance(
        &mut self,
        ticket: &PlaybackTicket,
        delta_ms: u64,
    ) -> Result<PlaybackAdvance, Self::Error> {
        let playback = self
            .playback
            .as_mut()
            .filter(|playing| &playing.ticket == ticket)
            .ok_or(SequencePlayerError::WrongTicket)?;
        if playback.paused {
            return Err(SequencePlayerError::Paused);
        }
        let duration = playback.clip.info.duration_ms;
        if playback.mode == PlaybackMode::Once && playback.elapsed_ms == duration {
            return Err(SequencePlayerError::PlaybackComplete);
        }
        if delta_ms == 0 {
            return Ok(PlaybackAdvance {
                ticket: ticket.clone(),
                consumed_ms: 0,
                boundary: None,
            });
        }
        let remaining = duration
            .checked_sub(playback.elapsed_ms)
            .ok_or(SequencePlayerError::TimeOverflow)?;
        if delta_ms < remaining {
            playback.elapsed_ms = playback
                .elapsed_ms
                .checked_add(delta_ms)
                .ok_or(SequencePlayerError::TimeOverflow)?;
            self.publish_read_view()?;
            return Ok(PlaybackAdvance {
                ticket: ticket.clone(),
                consumed_ms: delta_ms,
                boundary: None,
            });
        }
        playback.elapsed_ms = match playback.mode {
            PlaybackMode::Loop => 0,
            PlaybackMode::Once => duration,
        };
        let boundary = match playback.mode {
            PlaybackMode::Loop => PlaybackBoundary::LoopBoundary,
            PlaybackMode::Once => PlaybackBoundary::Completed,
        };
        self.publish_read_view()?;
        Ok(PlaybackAdvance {
            ticket: ticket.clone(),
            consumed_ms: remaining,
            boundary: Some(boundary),
        })
    }

    fn pause(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error> {
        let playback = self
            .playback
            .as_mut()
            .filter(|playing| &playing.ticket == ticket)
            .ok_or(SequencePlayerError::WrongTicket)?;
        playback.paused = true;
        self.publish_read_view()?;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }

    fn resume(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error> {
        let playback = self
            .playback
            .as_mut()
            .filter(|playing| &playing.ticket == ticket)
            .ok_or(SequencePlayerError::WrongTicket)?;
        playback.paused = false;
        self.publish_read_view()?;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }

    fn stop(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error> {
        if self
            .playback
            .as_ref()
            .is_none_or(|playing| &playing.ticket != ticket)
        {
            return Err(SequencePlayerError::WrongTicket);
        }
        self.playback = None;
        *self
            .published_frame
            .lock()
            .map_err(|_| SequencePlayerError::ReadViewPoisoned)? = None;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }

    fn progress(&self, ticket: &PlaybackTicket) -> Result<PlaybackPosition, Self::Error> {
        let playback = self
            .playback
            .as_ref()
            .filter(|playing| &playing.ticket == ticket)
            .ok_or(SequencePlayerError::WrongTicket)?;
        Ok(PlaybackPosition {
            ticket: ticket.clone(),
            elapsed_ms: playback.elapsed_ms,
            paused: playback.paused,
        })
    }
}

impl SequenceClipPlayer {
    fn publish_read_view(&self) -> Result<(), SequencePlayerError> {
        let published = self.playback.as_ref().map(|playback| {
            let index = frame_index(&playback.clip, playback.elapsed_ms);
            let frame = &playback.clip.frames[index];
            PublishedFrame {
                ticket: playback.ticket.clone(),
                frame: ReadOnlyFrameView {
                    width: frame.width,
                    height: frame.height,
                    rgba: Arc::clone(&frame.rgba),
                    frame_index: index,
                },
            }
        });
        *self
            .published_frame
            .lock()
            .map_err(|_| SequencePlayerError::ReadViewPoisoned)? = published;
        Ok(())
    }
}

fn frame_index(clip: &DecodedClip, elapsed_ms: u64) -> usize {
    let mut boundary = 0u64;
    for (index, frame) in clip.frames.iter().enumerate() {
        boundary += frame.duration_ms;
        if elapsed_ms < boundary {
            return index;
        }
    }
    clip.frames.len() - 1
}
