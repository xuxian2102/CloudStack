//! Live Preview foundations: converting tree-sitter source positions into
//! `GtkTextIter`s, running a non-authoritative tree-sitter Markdown
//! analysis pass over the editor buffer's source text, and rendering an
//! already-validated `DecorationPlan` onto a real `sourceview::Buffer` as
//! CloudStack-owned `TextTag`s. Validated by the Phase 11 spikes
//! (`docs/LIVE_PREVIEW_SPIKES.md`) before being written as production code
//! here. Not wired into the running editor yet — see that document's §9
//! for the constraints this module follows.
//!
//! `coordinates` no longer needs a dead-code allow: `adapter` (Phase 12B)
//! genuinely calls `resolve_point`/`iter_at_point` from production code, not
//! just from tests. `analysis` still carries one, and more narrowly than
//! before this phase: `adapter`'s production code consumes the *types*
//! `DecorationPlan`/`StyleSpan`/`StyleKind` (as `&DecorationPlan` parameters
//! it reads), but nothing outside tests calls `analyze()` itself yet, so
//! the analysis entry point and its private helpers remain genuinely
//! unreachable from `main()` until Phase 12C actually runs analysis against
//! a real buffer. `adapter`/`tags` also still carry their own allow, since
//! nothing in `cloudstack-gtk`'s `main()` reaches them yet either — Phase
//! 12C wires all of this into the real editor. Remove each allow (and add
//! the real call sites) as its module actually becomes reachable.

#[allow(dead_code)]
pub mod analysis;
pub mod coordinates;

#[allow(dead_code)]
pub mod adapter;
#[allow(dead_code)]
pub mod tags;
