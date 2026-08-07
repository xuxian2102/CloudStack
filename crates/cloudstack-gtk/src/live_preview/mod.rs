//! Live Preview foundations: converting tree-sitter source positions into
//! `GtkTextIter`s, and running a non-authoritative tree-sitter Markdown
//! analysis pass over the editor buffer's source text. Validated by the
//! Phase 11 spikes (`docs/LIVE_PREVIEW_SPIKES.md`) before being written as
//! production code here. Not wired into the running editor yet — see that
//! document's §9 for the constraints this module follows.
//!
//! Each submodule below carries its own `#[allow(dead_code)]` rather than a
//! blanket allow here, and neither submodule is re-exported yet: nothing in
//! `cloudstack-gtk`'s `main()` reaches them (deliberately — Phase 12A scope
//! is analysis only, no wiring). This keeps the suppression scoped to
//! exactly the two modules that are dormant today, so lint coverage stays
//! intact for whatever Phase 12B adds alongside them (e.g. `tags.rs`,
//! `adapter.rs`). Remove each `#[allow(dead_code)]` and add the real
//! `pub use`/call sites once its module is actually wired in.

#[allow(dead_code)]
pub mod analysis;
#[allow(dead_code)]
pub mod coordinates;
