use moe_agent_runtime::animation::{
    AnimationAction as RuntimeAction, AnimationClip, AnimationDefinition, AnimationNode,
    AnimationSpec, Animator, PlaybackMode, PlaybackTicket, PreemptionPolicy,
};
use serde::{Deserialize, Serialize};
use std::error::Error;

use crate::contract::FrameSnapshot;

use super::{ActionMode, MaterialPack, ReadOnlyFrameView, SequenceClipPlayer, SequenceFrameReader};

const MAX_JS_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialState {
    pub revision: u64,
    pub resource_generation: u64,
    pub playback_generation: u64,
    pub source: String,
    pub pack: Option<PackView>,
    pub actions: Vec<ActionView>,
    pub action_id: Option<String>,
    pub status: String,
    pub loading: bool,
    pub decoded_bytes: usize,
    pub frame_count: usize,
    pub error: Option<MaterialErrorView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackView {
    pub id: String,
    pub label: String,
    pub version: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionView {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialErrorView {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MaterialCommand {
    Load {
        path: String,
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
    Play {
        action_id: String,
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
    Pause {
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
    Resume {
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
    Stop {
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
    Unload {
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
    Builtin {
        expected_resource_generation: u64,
        expected_playback_generation: u64,
    },
}

impl MaterialCommand {
    pub fn generations(&self) -> (u64, u64) {
        match self {
            Self::Load {
                expected_resource_generation,
                expected_playback_generation,
                ..
            }
            | Self::Play {
                expected_resource_generation,
                expected_playback_generation,
                ..
            }
            | Self::Pause {
                expected_resource_generation,
                expected_playback_generation,
            }
            | Self::Resume {
                expected_resource_generation,
                expected_playback_generation,
            }
            | Self::Stop {
                expected_resource_generation,
                expected_playback_generation,
            }
            | Self::Unload {
                expected_resource_generation,
                expected_playback_generation,
            }
            | Self::Builtin {
                expected_resource_generation,
                expected_playback_generation,
            } => (*expected_resource_generation, *expected_playback_generation),
        }
    }
}

pub struct MaterialHost {
    state: MaterialState,
    pack: Option<MaterialPack>,
    animator: Option<Animator<SequenceClipPlayer>>,
    reader: Option<SequenceFrameReader>,
    presentation_sequence: u64,
    frame_signature: Option<(String, usize, String)>,
    active_ticket: Option<PlaybackTicket>,
    backend_error: Option<Box<dyn Error>>,
}

impl Default for MaterialHost {
    fn default() -> Self {
        Self {
            state: MaterialState {
                revision: 1,
                resource_generation: 1,
                playback_generation: 1,
                source: "builtin".into(),
                pack: None,
                actions: vec![ActionView {
                    id: "idle".into(),
                    label: "Idle".into(),
                }],
                action_id: Some("idle".into()),
                status: "playing".into(),
                loading: false,
                decoded_bytes: 0,
                frame_count: 4,
                error: None,
            },
            pack: None,
            animator: None,
            reader: None,
            presentation_sequence: 1,
            frame_signature: None,
            active_ticket: None,
            backend_error: None,
        }
    }
}

impl MaterialHost {
    #[cfg(test)]
    pub(super) fn set_revision_for_test(&mut self, revision: u64) {
        self.state.revision = revision;
    }
    pub fn state(&self) -> MaterialState {
        self.state.clone()
    }
    pub fn frame_triple(&self) -> (u64, u64, u64) {
        (
            self.state.resource_generation,
            self.state.playback_generation,
            self.presentation_sequence,
        )
    }
    pub fn set_presentation_sequence(&mut self, sequence: u64) -> Result<(), String> {
        if sequence > MAX_JS_INTEGER {
            return Err("material_generation_exhausted".into());
        }
        if sequence < self.presentation_sequence {
            return Err("material_generation_stale".into());
        }
        self.presentation_sequence = sequence;
        Ok(())
    }
    pub fn set_builtin_status(&mut self, status: &str) -> Result<(), String> {
        self.ensure_revision_available()?;
        if !matches!(status, "playing" | "paused" | "stopped") {
            return Err("material_status_invalid".into());
        }
        let playback_generation = if status == "stopped" {
            Some(next(self.state.playback_generation)?)
        } else {
            None
        };
        let presentation_sequence = next(self.presentation_sequence)?;
        self.state.status = status.into();
        self.state.action_id = (status == "playing" || status == "paused").then(|| "idle".into());
        if let Some(generation) = playback_generation {
            self.state.playback_generation = generation;
        }
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }
    pub fn restart_builtin(&mut self) -> Result<(), String> {
        self.ensure_revision_available()?;
        if self.state.source != "builtin" {
            return Err("material_builtin_unavailable".into());
        }
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        self.state.playback_generation = playback_generation;
        self.state.status = "playing".into();
        self.state.action_id = Some("idle".into());
        self.state.error = None;
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }

    pub fn set_loading(&mut self, loading: bool) -> Result<(), String> {
        self.ensure_revision_available()?;
        self.state.loading = loading;
        if loading {
            self.state.error = None;
        }
        self.bump_revision();
        Ok(())
    }
    pub fn set_error(&mut self, code: &str, message: &str) -> Result<(), String> {
        self.ensure_revision_available()?;
        self.state.loading = false;
        self.state.error = Some(MaterialErrorView {
            code: code.into(),
            message: message.into(),
        });
        self.bump_revision();
        Ok(())
    }
    pub fn set_playback_fault(&mut self, code: &str, message: &str) -> Result<(), String> {
        self.ensure_revision_available()?;
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        self.state.playback_generation = playback_generation;
        self.state.status = "faulted".into();
        self.state.error = Some(MaterialErrorView {
            code: code.into(),
            message: material_safe_message(code).into(),
        });
        let _ = message;
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }
    pub fn retain_backend_error<E: Error + 'static>(&mut self, error: E) {
        self.backend_error = Some(Box::new(error));
    }
    pub fn set_presentation_fault(&mut self, code: &str) -> Result<(), String> {
        self.ensure_revision_available()?;
        self.state.status = "faulted".into();
        self.state.error = Some(MaterialErrorView {
            code: code.into(),
            message: "无法确认旧命中区域已清除，请显式重试清除".into(),
        });
        self.bump_revision();
        Ok(())
    }
    pub fn recover_presentation(&mut self, status: &str) -> Result<(), String> {
        self.ensure_revision_available()?;
        if !matches!(status, "playing" | "paused" | "stopped") {
            return Err("material_status_invalid".into());
        }
        self.state.status = status.into();
        self.state.error = None;
        self.bump_revision();
        Ok(())
    }
    pub fn install(&mut self, pack: MaterialPack) -> Result<(), String> {
        self.ensure_revision_available()?;
        let (animator, reader) = create_package_animator(&pack)?;
        let resource_generation = next(self.state.resource_generation)?;
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        self.stop_package()?;
        let mut animator = animator;
        if let Err(error) = animator.request_action(pack.default_action()) {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_start_failed", "");
            return Err("material_start_failed".into());
        }
        let snapshot = animator.snapshot();
        let ticket = snapshot.ticket.ok_or("material_start_failed")?;
        if snapshot.action.as_deref() != Some(pack.default_action()) {
            return Err("material_start_not_active".into());
        }
        self.pack = Some(pack.clone());
        self.animator = Some(animator);
        self.reader = Some(reader);
        self.active_ticket = Some(ticket);
        self.frame_signature = None;
        self.state.resource_generation = resource_generation;
        self.state.playback_generation = playback_generation;
        self.state.source = "package".into();
        self.state.pack = Some(PackView {
            id: pack.id().into(),
            label: pack.label().into(),
            version: pack.version(),
        });
        self.state.actions = pack
            .actions()
            .iter()
            .map(|action| ActionView {
                id: action.id.clone(),
                label: action.label.clone(),
            })
            .collect();
        self.state.action_id = Some(pack.default_action().into());
        self.state.status = "playing".into();
        self.state.loading = false;
        self.state.decoded_bytes = pack.decoded_bytes();
        self.state.frame_count = pack.frame_count();
        self.state.error = None;
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }

    pub fn play(&mut self, action_id: &str) -> Result<(), String> {
        self.ensure_revision_available()?;
        let pack = self.pack.as_ref().ok_or("no_material_package")?;
        if pack.action(action_id).is_none() {
            return Err("unknown_material_action".into());
        }
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        if let Err(error) = self
            .animator
            .as_mut()
            .ok_or("material_animator_unavailable")?
            .stop()
        {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_stop_failed", "");
            return Err("material_stop_failed".into());
        }
        let animator = self
            .animator
            .as_mut()
            .ok_or("material_animator_unavailable")?;
        if let Err(error) = animator.request_action(action_id) {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_start_failed", "");
            return Err("material_start_failed".into());
        }
        let snapshot = animator.snapshot();
        if snapshot.action.as_deref() != Some(action_id) {
            self.mark_fault(
                "material_start_failed",
                "requested action has no active playback ticket",
            );
            return Err("material_start_failed".into());
        }
        let ticket = snapshot.ticket.ok_or("material_playback_failed")?;
        if self.active_ticket.as_ref() != Some(&ticket) {
            self.state.playback_generation = playback_generation;
            self.active_ticket = Some(ticket);
        }
        self.state.action_id = snapshot.action;
        self.state.status = if snapshot.paused { "paused" } else { "playing" }.into();
        self.state.error = None;
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.ensure_revision_available()?;
        if self.state.status != "playing" {
            return Err("material_pause_unavailable".into());
        }
        let presentation_sequence = next(self.presentation_sequence)?;
        if let Err(error) = self.animator.as_mut().ok_or("no_material_package")?.pause() {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_pause_failed", "");
            return Err("material_pause_failed".into());
        }
        self.state.status = "paused".into();
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }
    pub fn resume(&mut self) -> Result<(), String> {
        self.ensure_revision_available()?;
        if self.state.status != "paused" {
            return Err("material_resume_unavailable".into());
        }
        let _ = next(next(self.state.playback_generation)?)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        if let Err(error) = self
            .animator
            .as_mut()
            .ok_or("no_material_package")?
            .resume()
        {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_resume_failed", "");
            return Err("material_resume_failed".into());
        }
        self.sync_active_playback()?;
        self.state.status = "playing".into();
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.ensure_revision_available()?;
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        self.stop_package()?;
        self.state.playback_generation = playback_generation;
        self.active_ticket = None;
        self.state.action_id = None;
        self.state.status = "stopped".into();
        self.state.error = None;
        self.frame_signature = None;
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }
    pub fn unload(&mut self) -> Result<(), String> {
        self.ensure_revision_available()?;
        let resource_generation = next(self.state.resource_generation)?;
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        self.stop_package()?;
        self.animator = None;
        self.pack = None;
        self.reader = None;
        self.frame_signature = None;
        self.active_ticket = None;
        self.state.resource_generation = resource_generation;
        self.state.playback_generation = playback_generation;
        self.state.source = "empty".into();
        self.state.pack = None;
        self.state.actions.clear();
        self.state.action_id = None;
        self.state.status = "stopped".into();
        self.state.decoded_bytes = 0;
        self.state.frame_count = 0;
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }
    pub fn builtin(&mut self) -> Result<(), String> {
        self.ensure_revision_available()?;
        let resource_generation = next(self.state.resource_generation)?;
        let playback_generation = next(self.state.playback_generation)?;
        let presentation_sequence = next(self.presentation_sequence)?;
        self.stop_package()?;
        self.animator = None;
        self.pack = None;
        self.reader = None;
        self.frame_signature = None;
        self.active_ticket = None;
        self.state.resource_generation = resource_generation;
        self.state.playback_generation = playback_generation;
        self.state.source = "builtin".into();
        self.state.pack = None;
        self.state.actions = vec![ActionView {
            id: "idle".into(),
            label: "Idle".into(),
        }];
        self.state.action_id = None;
        self.state.status = "stopped".into();
        self.state.decoded_bytes = 0;
        self.state.frame_count = 4;
        self.state.actions = vec![ActionView {
            id: "idle".into(),
            label: "Idle".into(),
        }];
        self.presentation_sequence = presentation_sequence;
        self.bump_revision();
        Ok(())
    }

    pub fn advance(&mut self, delta_ms: u64) -> Result<(), String> {
        if self.state.source != "package" || self.state.status != "playing" {
            return Ok(());
        }
        next(next(self.state.revision)?)?;
        next(next(self.state.playback_generation)?)?;
        let _ = next(self.presentation_sequence)?;
        if let Err(error) = self
            .animator
            .as_mut()
            .ok_or("material_animator_unavailable")?
            .advance(delta_ms)
        {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_advance_failed", "");
            return Err("material_advance_failed".into());
        }
        let progress = self
            .animator
            .as_mut()
            .ok_or("material_animator_unavailable")?
            .playback_progress();
        let position = match progress {
            Ok(position) => position,
            Err(error) => {
                self.backend_error = Some(Box::new(error));
                self.mark_fault("material_progress_failed", "");
                return Err("material_progress_failed".into());
            }
        };
        self.sync_active_playback()?;
        if let Some(position) = position {
            self.state.status = if position.paused { "paused" } else { "playing" }.into();
            if let Some((clip_id, view)) = self.current_frame() {
                let signature = (clip_id, view.frame_index, self.state.status.clone());
                if self.frame_signature.as_ref() != Some(&signature) {
                    self.frame_signature = Some(signature);
                    self.bump_presentation()?;
                }
            }
        } else if self.state.status == "playing" {
            self.state.status = "stopped".into();
            self.state.action_id = None;
            self.frame_signature = None;
            self.bump_presentation()?;
            self.bump_revision();
        }
        Ok(())
    }

    pub fn current_frame(&self) -> Option<(String, ReadOnlyFrameView)> {
        let animator = self.animator.as_ref()?;
        let snapshot = animator.snapshot();
        let ticket = snapshot.ticket.as_ref()?;
        let view = self.reader.as_ref()?.current_frame(ticket).ok()?;
        Some((ticket.clip().to_owned(), view))
    }

    pub fn frame_snapshot(&self) -> FrameSnapshot {
        let (clip_id, frame) = self.current_frame().unwrap_or_else(|| {
            (
                String::new(),
                ReadOnlyFrameView {
                    width: 0,
                    height: 0,
                    rgba: std::sync::Arc::from([]),
                    frame_index: 0,
                },
            )
        });
        let empty = self.current_frame().is_none();
        FrameSnapshot {
            resource_generation: self.state.resource_generation,
            playback_generation: self.state.playback_generation,
            presentation_sequence: self.presentation_sequence,
            source: if empty { "empty" } else { "package" }.into(),
            clip_id: (!empty).then_some(clip_id),
            frame_index: (!empty).then_some(frame.frame_index as u8),
            width: frame.width,
            height: frame.height,
            paused: self.state.status != "playing",
            faulted: self.state.status == "faulted",
        }
    }

    pub fn frame_pixels(&self, triple: (u64, u64, u64)) -> Result<Vec<u8>, String> {
        if triple != self.frame_triple() || self.state.source != "package" {
            return Err("stale_material_frame".into());
        }
        let (_, frame) = self.current_frame().ok_or("material_frame_unavailable")?;
        let expected_len = (frame.width as usize)
            .checked_mul(frame.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("material_frame_dimensions_invalid")?;
        if expected_len > 1024 * 1024 || frame.rgba.len() != expected_len {
            return Err("material_frame_too_large".into());
        }
        Ok(frame.rgba.to_vec())
    }

    pub fn release_for_shutdown(&mut self) -> Result<(), String> {
        self.stop_package()?;
        self.animator = None;
        self.pack = None;
        self.reader = None;
        self.active_ticket = None;
        self.frame_signature = None;
        self.state.source = "empty".into();
        self.state.pack = None;
        self.state.actions.clear();
        self.state.action_id = None;
        self.state.status = "stopped".into();
        self.state.loading = false;
        self.state.decoded_bytes = 0;
        self.state.frame_count = 0;
        Ok(())
    }

    fn stop_package(&mut self) -> Result<(), String> {
        let stop_result = self.animator.as_mut().map(Animator::stop);
        if let Some(Err(error)) = stop_result {
            self.backend_error = Some(Box::new(error));
            self.mark_fault("material_stop_failed", "");
            return Err("material_stop_failed".into());
        }
        self.active_ticket = None;
        Ok(())
    }
    fn sync_active_playback(&mut self) -> Result<(), String> {
        let snapshot = self
            .animator
            .as_ref()
            .ok_or("material_animator_unavailable")?
            .snapshot();
        if snapshot.ticket != self.active_ticket {
            self.state.playback_generation = next(self.state.playback_generation)?;
            self.active_ticket = snapshot.ticket.clone();
            self.state.action_id = snapshot.action;
        }
        Ok(())
    }
    fn mark_fault(&mut self, code: &str, _backend_error: &str) {
        self.state.status = "faulted".into();
        self.state.error = Some(MaterialErrorView {
            code: code.into(),
            message: material_safe_message(code).into(),
        });
        self.bump_revision();
    }
    pub fn ensure_revision_available(&self) -> Result<(), String> {
        next(self.state.revision).map(|_| ())
    }
    pub fn preflight_command(&self, command: &MaterialCommand) -> Result<(), String> {
        let revision_steps = match command {
            // Include recovery, cancellation, the operation, and a possible
            // native-mask failure projection before permitting side effects.
            MaterialCommand::Stop { .. } | MaterialCommand::Unload { .. } => 4,
            MaterialCommand::Load { .. } | MaterialCommand::Builtin { .. } => 3,
            MaterialCommand::Play { .. } => 2,
            MaterialCommand::Pause { .. } | MaterialCommand::Resume { .. } => 1,
        };
        let mut revision = self.state.revision;
        for _ in 0..revision_steps {
            revision = next(revision)?;
        }
        match command {
            MaterialCommand::Load { .. } => {
                next(self.state.resource_generation)?;
                next(self.state.playback_generation)?;
                next(self.presentation_sequence)?;
            }
            MaterialCommand::Play { .. } | MaterialCommand::Stop { .. } => {
                next(self.state.playback_generation)?;
                next(self.presentation_sequence)?;
            }
            MaterialCommand::Resume { .. } => {
                next(next(self.state.playback_generation)?)?;
                next(self.presentation_sequence)?;
            }
            MaterialCommand::Pause { .. } => {
                next(self.presentation_sequence)?;
            }
            MaterialCommand::Unload { .. } => {
                next(self.state.resource_generation)?;
                next(self.state.playback_generation)?;
                next(self.presentation_sequence)?;
            }
            MaterialCommand::Builtin { .. } => {
                next(self.state.resource_generation)?;
                next(next(self.state.playback_generation)?)?;
                next(next(self.presentation_sequence)?)?;
            }
        }
        Ok(())
    }
    fn bump_revision(&mut self) {
        self.state.revision += 1;
    }
    fn bump_presentation(&mut self) -> Result<(), String> {
        self.presentation_sequence = next(self.presentation_sequence)?;
        Ok(())
    }
}

fn next(value: u64) -> Result<u64, String> {
    value
        .checked_add(1)
        .filter(|next| *next <= MAX_JS_INTEGER)
        .ok_or_else(|| "material_generation_exhausted".into())
}

fn material_safe_message(code: &str) -> &'static str {
    match code {
        "material_stop_failed" => "无法确认素材播放已停止",
        "material_start_failed" => "素材播放启动失败",
        "material_pause_failed" => "无法确认素材暂停状态",
        "material_resume_failed" => "无法确认素材恢复状态",
        "material_advance_failed" | "material_progress_failed" => "素材播放通道异常，需确认停止",
        _ => "素材播放状态异常",
    }
}

fn create_package_animator(
    pack: &MaterialPack,
) -> Result<(Animator<SequenceClipPlayer>, SequenceFrameReader), String> {
    let clips = pack
        .clips()
        .map(|clip| {
            AnimationClip::new(&clip.id, pack.version(), clip.duration_ms)
                .map_err(|_| "invalid_material_clip".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut actions = Vec::with_capacity(pack.actions().len());
    for (index, action) in pack.actions().iter().enumerate() {
        let entry = format!("n{index}");
        let mode = match action.mode {
            ActionMode::Once => PlaybackMode::Once,
            ActionMode::Loop => PlaybackMode::Loop,
        };
        actions.push(RuntimeAction {
            id: action.id.clone(),
            entry: entry.clone(),
            exit: None,
            priority: 1,
            preemption: PreemptionPolicy::Immediate,
            nodes: vec![AnimationNode {
                id: entry,
                clip: action.clip_id.clone(),
                mode,
                edges: Vec::new(),
            }],
        });
    }
    let definition = AnimationDefinition::new(AnimationSpec {
        version: pack.version(),
        clips,
        actions,
        parameters: Vec::new(),
        triggers: Vec::new(),
        bindings: Vec::new(),
    })
    .map_err(|_| "invalid_material_animation".to_owned())?;
    let player = SequenceClipPlayer::new(pack.clone());
    let reader = player.frame_reader();
    let animator = Animator::new(definition, player);
    Ok((animator, reader))
}

#[cfg(test)]
mod preflight_tests {
    use super::*;

    #[test]
    fn recovery_preflight_keeps_room_for_the_final_native_failure_projection() {
        let mut accepted = 0;
        for remaining in 0..=4 {
            let mut host = MaterialHost::default();
            host.state.revision = MAX_JS_INTEGER - remaining;
            host.state.status = "faulted".into();
            let command = MaterialCommand::Stop {
                expected_resource_generation: 1,
                expected_playback_generation: 1,
            };
            let before = host.state.revision;
            if host.preflight_command(&command).is_err() {
                assert_eq!(host.state.revision, before);
                continue;
            }
            accepted += 1;
            // Confirmed native recovery, candidate cancellation, confirmed stop,
            // and a later native transition failure each publish an outcome.
            let result = host
                .recover_presentation("stopped")
                .and_then(|()| host.set_loading(false))
                .and_then(|()| host.stop())
                .and_then(|()| host.set_presentation_fault("material_hit_mask_clear_failed"));
            assert!(
                result.is_ok(),
                "accepted recovery exhausted its outcome revision budget: {remaining}"
            );
            assert!(host.state.revision <= MAX_JS_INTEGER);
        }
        assert!(accepted > 0);
    }
}
