use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

use super::decode::{decode_gif_cancellable, decode_png_cancellable};
use super::path::{canonical_package_root, read_asset_cancellable};

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_CLIPS: usize = 32;
pub const MAX_ACTIONS: usize = 32;
pub const MAX_FRAMES_PER_CLIP: usize = 256;
pub const MAX_TOTAL_FRAMES: usize = 512;
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PACKAGE_ENCODED_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PACKAGE_PIXEL_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_FRAME_DIMENSION: u32 = 512;
pub const MAX_FRAME_DURATION_MS: u64 = 600_000;
pub const MAX_CLIP_DURATION_MS: u64 = 3_600_000;

#[derive(Debug)]
pub enum MaterialError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(&'static str),
    Decode(Box<dyn Error + Send + Sync>),
    /// Package loading observed its cooperative cancellation flag.
    Cancelled,
}

impl fmt::Display for MaterialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "material I/O failed: {error}"),
            Self::Json(error) => write!(f, "material manifest is invalid: {error}"),
            Self::Invalid(reason) => write!(f, "material package rejected: {reason}"),
            Self::Decode(error) => write!(f, "material image decode failed: {error}"),
            Self::Cancelled => f.write_str("material load was cancelled"),
        }
    }
}

impl Error for MaterialError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Decode(error) => Some(error.as_ref()),
            Self::Invalid(_) | Self::Cancelled => None,
        }
    }
}

impl From<std::io::Error> for MaterialError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for MaterialError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    format_version: u32,
    id: String,
    version: u64,
    label: String,
    clips: Vec<ClipManifest>,
    actions: Vec<ActionManifest>,
    default_action: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields, rename_all = "camelCase")]
enum ClipManifest {
    Sequence {
        id: String,
        frames: Vec<SequenceFrame>,
    },
    Gif {
        id: String,
        path: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SequenceFrame {
    path: String,
    duration_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ActionManifest {
    id: String,
    label: String,
    clip: String,
    mode: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionMode {
    Once,
    Loop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialAction {
    pub id: String,
    pub label: String,
    pub clip_id: String,
    pub mode: ActionMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialClipInfo {
    pub id: String,
    pub duration_ms: u64,
    pub frame_count: usize,
}

#[derive(Clone, Debug)]
pub(super) struct DecodedMaterialFrame {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub(super) struct DecodedClip {
    pub info: MaterialClipInfo,
    pub frames: Vec<DecodedMaterialFrame>,
}

#[derive(Clone, Debug)]
struct PackData {
    id: String,
    version: u64,
    label: String,
    default_action: String,
    actions: Vec<MaterialAction>,
    clips: BTreeMap<String, DecodedClip>,
}

/// A fully validated package. Clones share immutable decoded pixels.
#[derive(Clone, Debug)]
pub struct MaterialPack(Arc<PackData>);

impl MaterialPack {
    pub fn load(directory: impl AsRef<Path>) -> Result<Self, MaterialError> {
        Self::load_with_resident_pixels(directory, 0)
    }

    /// Loads a package and checks `cancelled` between bounded read and decode stages.
    pub fn load_cancellable(
        directory: impl AsRef<Path>,
        cancelled: &AtomicBool,
    ) -> Result<Self, MaterialError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(MaterialError::Cancelled);
        }
        Self::load_internal(directory.as_ref(), 0, Some(cancelled))
    }

    /// Loads a candidate while accounting for pixels retained by the current package.
    pub fn load_with_resident_pixels(
        directory: impl AsRef<Path>,
        resident_pixel_bytes: usize,
    ) -> Result<Self, MaterialError> {
        Self::load_internal(directory.as_ref(), resident_pixel_bytes, None)
    }

    fn load_internal(
        directory: &Path,
        resident_pixel_bytes: usize,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, MaterialError> {
        let check_cancelled = || {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                Err(MaterialError::Cancelled)
            } else {
                Ok(())
            }
        };
        check_cancelled()?;
        if resident_pixel_bytes > 64 * 1024 * 1024 {
            return Err(MaterialError::Invalid(
                "resident pixel budget already exceeds package limit",
            ));
        }
        let root = canonical_package_root(directory)?;
        let manifest_bytes =
            read_asset_cancellable(&root, "manifest.json", MAX_MANIFEST_BYTES, cancelled)?;
        check_cancelled()?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        validate_manifest(&manifest)?;
        let mut encoded_total = 0usize;
        let mut pixel_total = 0usize;
        let mut frame_total = 0usize;
        let mut clips = BTreeMap::new();
        for clip in &manifest.clips {
            check_cancelled()?;
            let decoded = match clip {
                ClipManifest::Sequence { id, frames } => {
                    if frame_total
                        .checked_add(frames.len())
                        .is_none_or(|count| count > MAX_TOTAL_FRAMES)
                    {
                        return Err(MaterialError::Invalid("package frame limit exceeded"));
                    }
                    let mut result = Vec::with_capacity(frames.len());
                    let mut duration = 0u64;
                    for frame in frames {
                        check_cancelled()?;
                        let bytes =
                            read_counted(&root, &frame.path, &mut encoded_total, cancelled)?;
                        let remaining = (MAX_PACKAGE_PIXEL_BYTES - pixel_total)
                            .min(64 * 1024 * 1024 - resident_pixel_bytes - pixel_total);
                        let decoded = decode_png_cancellable(&bytes, remaining, cancelled)
                            .map_err(|error| {
                                if error.0 == "cancelled" {
                                    MaterialError::Cancelled
                                } else {
                                    MaterialError::Decode(Box::new(error))
                                }
                            })?;
                        check_cancelled()?;
                        let pixels = validate_frame_size(decoded.width, decoded.height)?;
                        reserve_decoded(&mut pixel_total, pixels, &mut frame_total)?;
                        duration = duration
                            .checked_add(frame.duration_ms)
                            .ok_or(MaterialError::Invalid("clip duration overflow"))?;
                        result.push(DecodedMaterialFrame {
                            width: decoded.width,
                            height: decoded.height,
                            duration_ms: frame.duration_ms,
                            rgba: Arc::from(decoded.rgba),
                        });
                    }
                    DecodedClip {
                        info: MaterialClipInfo {
                            id: id.clone(),
                            duration_ms: duration,
                            frame_count: result.len(),
                        },
                        frames: result,
                    }
                }
                ClipManifest::Gif { id, path } => {
                    if frame_total >= MAX_TOTAL_FRAMES {
                        return Err(MaterialError::Invalid("package frame limit exceeded"));
                    }
                    let bytes = read_counted(&root, path, &mut encoded_total, cancelled)?;
                    let max_frames = MAX_TOTAL_FRAMES
                        .saturating_sub(frame_total)
                        .min(MAX_FRAMES_PER_CLIP);
                    let max_pixels = (MAX_PACKAGE_PIXEL_BYTES - pixel_total)
                        .min(64 * 1024 * 1024 - resident_pixel_bytes - pixel_total);
                    let frames = decode_gif_cancellable(
                        &bytes,
                        max_frames,
                        max_pixels,
                        MAX_CLIP_DURATION_MS,
                        cancelled,
                    )
                    .map_err(|error| {
                        if error.0 == "cancelled" {
                            MaterialError::Cancelled
                        } else {
                            MaterialError::Decode(Box::new(error))
                        }
                    })?;
                    check_cancelled()?;
                    if frames.len() > MAX_FRAMES_PER_CLIP {
                        return Err(MaterialError::Invalid("clip frame limit exceeded"));
                    }
                    let mut duration = 0u64;
                    let mut converted = Vec::with_capacity(frames.len());
                    for frame in frames {
                        let pixels = validate_frame_size(frame.width, frame.height)?;
                        reserve_decoded(&mut pixel_total, pixels, &mut frame_total)?;
                        duration = duration
                            .checked_add(frame.duration_ms)
                            .ok_or(MaterialError::Invalid("clip duration overflow"))?;
                        converted.push(DecodedMaterialFrame {
                            width: frame.width,
                            height: frame.height,
                            duration_ms: frame.duration_ms,
                            rgba: Arc::from(frame.rgba),
                        });
                    }
                    DecodedClip {
                        info: MaterialClipInfo {
                            id: id.clone(),
                            duration_ms: duration,
                            frame_count: converted.len(),
                        },
                        frames: converted,
                    }
                }
            };
            if decoded.info.duration_ms > MAX_CLIP_DURATION_MS {
                return Err(MaterialError::Invalid("clip duration limit exceeded"));
            }
            clips.insert(decoded.info.id.clone(), decoded);
        }
        Ok(Self(Arc::new(PackData {
            id: manifest.id,
            version: manifest.version,
            label: manifest.label,
            default_action: manifest.default_action,
            actions: manifest
                .actions
                .into_iter()
                .map(|action| MaterialAction {
                    id: action.id,
                    label: action.label,
                    clip_id: action.clip,
                    mode: if action.mode == "once" {
                        ActionMode::Once
                    } else {
                        ActionMode::Loop
                    },
                })
                .collect(),
            clips,
        })))
    }

    pub fn id(&self) -> &str {
        &self.0.id
    }
    pub fn version(&self) -> u64 {
        self.0.version
    }
    pub fn label(&self) -> &str {
        &self.0.label
    }
    pub fn default_action(&self) -> &str {
        &self.0.default_action
    }
    /// Returns the total retained RGBA byte count for all decoded frames.
    pub fn decoded_bytes(&self) -> usize {
        self.0
            .clips
            .values()
            .flat_map(|clip| &clip.frames)
            .map(|frame| frame.rgba.len())
            .sum()
    }
    /// Returns the total number of decoded frames in the package.
    pub fn frame_count(&self) -> usize {
        self.0.clips.values().map(|clip| clip.frames.len()).sum()
    }
    pub fn actions(&self) -> &[MaterialAction] {
        &self.0.actions
    }
    pub fn clips(&self) -> impl Iterator<Item = &MaterialClipInfo> {
        self.0.clips.values().map(|clip| &clip.info)
    }
    pub fn clip(&self, id: &str) -> Option<&MaterialClipInfo> {
        self.0.clips.get(id).map(|clip| &clip.info)
    }
    pub fn action(&self, id: &str) -> Option<&MaterialAction> {
        self.0.actions.iter().find(|action| action.id == id)
    }
    pub fn pixel_bytes(&self) -> usize {
        self.0
            .clips
            .values()
            .flat_map(|clip| &clip.frames)
            .map(|frame| frame.rgba.len())
            .sum()
    }
    pub(super) fn decoded_clip(&self, id: &str) -> Option<&DecodedClip> {
        self.0.clips.get(id)
    }
}

fn read_counted(
    root: &Path,
    path: &str,
    total: &mut usize,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<u8>, MaterialError> {
    let remaining = MAX_PACKAGE_ENCODED_BYTES
        .checked_sub(*total)
        .ok_or(MaterialError::Invalid("encoded byte limit exceeded"))?;
    let bytes = read_asset_cancellable(root, path, MAX_FILE_BYTES.min(remaining), cancelled)?;
    *total = total
        .checked_add(bytes.len())
        .ok_or(MaterialError::Invalid("encoded byte count overflow"))?;
    Ok(bytes)
}

fn reserve_decoded(
    total: &mut usize,
    bytes: usize,
    frames: &mut usize,
) -> Result<(), MaterialError> {
    *frames = frames
        .checked_add(1)
        .ok_or(MaterialError::Invalid("frame count overflow"))?;
    if *frames > MAX_TOTAL_FRAMES {
        return Err(MaterialError::Invalid("package frame limit exceeded"));
    }
    *total = total
        .checked_add(bytes)
        .ok_or(MaterialError::Invalid("pixel byte count overflow"))?;
    if *total > MAX_PACKAGE_PIXEL_BYTES {
        return Err(MaterialError::Invalid(
            "package decoded pixel budget exceeded",
        ));
    }
    Ok(())
}

fn validate_frame_size(width: u32, height: u32) -> Result<usize, MaterialError> {
    if width == 0 || height == 0 || width > MAX_FRAME_DIMENSION || height > MAX_FRAME_DIMENSION {
        return Err(MaterialError::Invalid(
            "frame dimensions are outside the supported range",
        ));
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(MaterialError::Invalid("frame byte count overflow"))
}

fn validate_manifest(manifest: &Manifest) -> Result<(), MaterialError> {
    if manifest.format_version != 1 {
        return Err(MaterialError::Invalid("unknown manifest format version"));
    }
    validate_id(&manifest.id)?;
    if manifest.version == 0 || manifest.version > 9_007_199_254_740_991 {
        return Err(MaterialError::Invalid(
            "version is not a positive JavaScript safe integer",
        ));
    }
    validate_label(&manifest.label)?;
    if manifest.clips.is_empty() || manifest.clips.len() > MAX_CLIPS {
        return Err(MaterialError::Invalid(
            "clip count is outside the supported range",
        ));
    }
    if manifest.actions.is_empty() || manifest.actions.len() > MAX_ACTIONS {
        return Err(MaterialError::Invalid(
            "action count is outside the supported range",
        ));
    }
    let mut clip_ids = BTreeSet::new();
    let mut minimum_total_frames = 0usize;
    for clip in &manifest.clips {
        let (id, count) = match clip {
            ClipManifest::Sequence { id, frames } => (id, frames.len()),
            ClipManifest::Gif { id, .. } => (id, 1),
        };
        minimum_total_frames = minimum_total_frames
            .checked_add(count)
            .ok_or(MaterialError::Invalid("frame count overflow"))?;
        if minimum_total_frames > MAX_TOTAL_FRAMES {
            return Err(MaterialError::Invalid("package frame limit exceeded"));
        }
        validate_id(id)?;
        if !clip_ids.insert(id) {
            return Err(MaterialError::Invalid("duplicate clip id"));
        }
        if count == 0 || count > MAX_FRAMES_PER_CLIP {
            return Err(MaterialError::Invalid(
                "clip frame count is outside the supported range",
            ));
        }
        if let ClipManifest::Sequence { frames, .. } = clip {
            let mut duration = 0u64;
            for frame in frames {
                validate_path_text(&frame.path)?;
                if frame.duration_ms == 0 || frame.duration_ms > MAX_FRAME_DURATION_MS {
                    return Err(MaterialError::Invalid(
                        "frame duration is outside the supported range",
                    ));
                }
                duration = duration
                    .checked_add(frame.duration_ms)
                    .ok_or(MaterialError::Invalid("clip duration overflow"))?;
            }
            if duration > MAX_CLIP_DURATION_MS {
                return Err(MaterialError::Invalid("clip duration limit exceeded"));
            }
        }
        if let ClipManifest::Gif { path, .. } = clip {
            validate_path_text(path)?;
        }
    }
    let mut action_ids = BTreeSet::new();
    for action in &manifest.actions {
        validate_id(&action.id)?;
        validate_label(&action.label)?;
        if !action_ids.insert(&action.id) {
            return Err(MaterialError::Invalid("duplicate action id"));
        }
        if !clip_ids.contains(&action.clip) {
            return Err(MaterialError::Invalid("action references an unknown clip"));
        }
        if action.mode != "once" && action.mode != "loop" {
            return Err(MaterialError::Invalid("unknown action playback mode"));
        }
    }
    if !action_ids.contains(&manifest.default_action) {
        return Err(MaterialError::Invalid("default action does not exist"));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), MaterialError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(MaterialError::Invalid(
            "id must be 1..64 ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<(), MaterialError> {
    let count = label.chars().count();
    if count == 0 || count > 64 || label.chars().any(char::is_control) {
        return Err(MaterialError::Invalid(
            "label must contain 1..64 non-control Unicode scalars",
        ));
    }
    Ok(())
}

fn validate_path_text(path: &str) -> Result<(), MaterialError> {
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path.starts_with('/')
    {
        return Err(MaterialError::Invalid(
            "asset path is not a safe package-relative path",
        ));
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(MaterialError::Invalid(
            "asset path contains an unsafe component",
        ));
    }
    Ok(())
}
