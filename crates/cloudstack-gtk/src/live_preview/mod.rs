//! Live Preview foundations: converting tree-sitter source positions into
//! `GtkTextIter`s, running a non-authoritative tree-sitter Markdown
//! analysis pass over the editor buffer's source text, rendering an
//! already-validated `DecorationPlan` onto a real `sourceview::Buffer` as
//! CloudStack-owned `TextTag`s, and wiring that into the real editor with a
//! debounce and stale-result rejection. Validated by the Phase 11 spikes
//! (`docs/LIVE_PREVIEW_SPIKES.md`) before being written as production code.

pub mod adapter;
pub mod analysis;
pub mod coordinates;
mod runtime;
pub mod tags;

pub use runtime::LivePreview;
