//! Read-only, bounded material packages and a host-driven sequence player.
//!
//! Package I/O and image decoding stay in the Tauri host. Assets are immutable
//! after validation; each player owns independent cursor and ticket state.

mod decode;
pub(crate) mod host;
mod manifest;
mod path;
mod player;

pub use manifest::{ActionMode, MaterialAction, MaterialClipInfo, MaterialError, MaterialPack};
pub use player::{FrameView, ReadOnlyFrameView, SequenceClipPlayer, SequenceFrameReader};

#[cfg(test)]
mod tests;
