//! Renders an already-computed `DecorationPlan` onto a real
//! `sourceview::Buffer`, or rejects it outright and touches nothing.
//!
//! This module deliberately does not know about `document_epoch`/
//! `generation` staleness, `WorkspaceSession`, debouncing, or worker
//! threads — those are Phase 12C's job. The boundary is intentional:
//!
//! ```text
//! 12A analysis   -> produces a DecorationPlan (an identity, not yet trusted)
//! 12C coordinator -> decides whether a plan is current
//! 12B adapter     -> safely renders a plan already known to be current
//! ```

use gtk::prelude::*;

use super::analysis::{DecorationPlan, StyleKind, StyleSpan};
use super::coordinates::{iter_at_point, resolve_point};
use super::tags::LivePreviewTags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPlanError {
    /// `plan` is internally inconsistent (see `validate_plan`). Not
    /// expected to fire against real `analyze` output — checked as a
    /// boundary anyway, since a `DecorationPlan` is a value that can cross
    /// an untrusted boundary (Phase 12C's worker-thread result), not
    /// because `analyze` is assumed to be unreliable.
    InvalidPlan,
    /// `plan` was computed against different text than what `buffer`
    /// currently contains.
    SourceMismatch,
    /// A `SourceRange` inside an otherwise-valid plan could not be
    /// resolved into a `GtkTextIter`, or its `StyleKind` has no installed
    /// tag.
    InvalidGtkRange,
}

/// Pure, GTK-free validation that `plan` is internally consistent with
/// `source`. Every individual check here is something `analyze` should
/// already guarantee about its own output; this function exists as a
/// boundary that a later untrusted producer (a worker thread, in Phase
/// 12C) has to pass, not because today's `analyze` is expected to fail it.
pub fn validate_plan(source: &str, plan: &DecorationPlan) -> bool {
    plan.source_len == source.len() && plan.styles.iter().all(|span| validate_span(source, span))
}

fn validate_span(source: &str, span: &StyleSpan) -> bool {
    let start = span.range.start.byte;
    let end = span.range.end.byte;

    if start > end || end > source.len() {
        return false;
    }
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return false;
    }
    if resolve_point(source, span.range.start.point) != Some(start) {
        return false;
    }
    if resolve_point(source, span.range.end.point) != Some(end) {
        return false;
    }
    if let StyleKind::Heading(level) = span.kind {
        if !(1..=6).contains(&level) {
            return false;
        }
    }
    true
}

/// Renders `plan` onto `buffer`, replacing CloudStack's previous
/// decoration in one all-or-nothing step. Order matters:
///
/// 1. validate the whole plan (pure, no GTK touched yet);
/// 2. confirm `buffer`'s current text is exactly `source` — a plan
///    computed against stale text must never be applied, even if every
///    range inside it happens to still be individually valid against the
///    new text by coincidence;
/// 3. resolve every span's `SourceRange` into a `GtkTextIter` pair
///    *before* mutating any tag — if any single span fails to resolve,
///    return an error with the buffer's existing decoration completely
///    untouched;
/// 4. only once every span has resolved successfully: clear CloudStack's
///    own tags and apply the complete new set.
///
/// The buffer is never left in a partially-decorated state: it's either
/// still showing the previous (possibly empty) decoration, or fully
/// showing the new one — never a mix of the two.
pub fn apply_plan(
    buffer: &sourceview::Buffer,
    source: &str,
    tags: &LivePreviewTags,
    plan: &DecorationPlan,
) -> Result<(), ApplyPlanError> {
    if !validate_plan(source, plan) {
        return Err(ApplyPlanError::InvalidPlan);
    }

    let actual = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    if actual.as_str() != source {
        return Err(ApplyPlanError::SourceMismatch);
    }

    let text_buffer: &gtk::TextBuffer = buffer.upcast_ref();
    let mut resolved = Vec::with_capacity(plan.styles.len());
    for span in &plan.styles {
        let start = iter_at_point(text_buffer, source, span.range.start.point)
            .ok_or(ApplyPlanError::InvalidGtkRange)?;
        let end = iter_at_point(text_buffer, source, span.range.end.point)
            .ok_or(ApplyPlanError::InvalidGtkRange)?;
        let tag = tags
            .tag_for(span.kind)
            .ok_or(ApplyPlanError::InvalidGtkRange)?;
        resolved.push((tag, start, end));
    }

    tags.clear(buffer);
    for (tag, start, end) in resolved {
        buffer.apply_tag(tag, &start, &end);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_preview::analysis::analyze;
    use crate::live_preview::coordinates::{SourcePosition, SourceRange};
    use tree_sitter::Point;

    fn point(row: usize, column: usize) -> Point {
        Point { row, column }
    }

    fn span(kind: StyleKind, source: &str, start_byte: usize, end_byte: usize) -> StyleSpan {
        let start_point = point_for(source, start_byte);
        let end_point = point_for(source, end_byte);
        StyleSpan {
            kind,
            range: SourceRange {
                start: SourcePosition {
                    byte: start_byte,
                    point: start_point,
                },
                end: SourcePosition {
                    byte: end_byte,
                    point: end_point,
                },
            },
        }
    }

    /// Computes the `Point` a valid byte offset corresponds to, for
    /// building test fixtures — the inverse of `resolve_point`.
    fn point_for(source: &str, byte: usize) -> Point {
        let mut offset = 0usize;
        for (row, line) in source.split('\n').enumerate() {
            let line_end = offset + line.len();
            if byte <= line_end {
                return Point {
                    row,
                    column: byte - offset,
                };
            }
            offset = line_end + 1;
        }
        panic!("byte {byte} out of range for fixture {source:?}");
    }

    #[test]
    fn valid_plan_from_real_analysis_is_accepted() {
        let source = "# Heading\n\n**bold** text\n";
        let plan = analyze(source, 0, 0);
        assert!(validate_plan(source, &plan));
    }

    #[test]
    fn rejects_source_length_mismatch() {
        let source = "hello";
        let plan = analyze(source, 0, 0);
        assert!(!validate_plan("hello!", &plan));
        assert!(!validate_plan("hell", &plan));
    }

    #[test]
    fn rejects_range_past_end() {
        // end=100 越界；这个测试要走到的正是 validate_span 里"end >
        // source.len()"那条最早的检查，比 point 一致性检查更早短路，所以
        // 这里不用（也不能）先算出一个真实存在的 Point。
        let source = "hello";
        let plan = DecorationPlan {
            document_epoch: 0,
            generation: 0,
            source_len: source.len(),
            styles: vec![StyleSpan {
                kind: StyleKind::Strong,
                range: SourceRange {
                    start: SourcePosition {
                        byte: 0,
                        point: point(0, 0),
                    },
                    end: SourcePosition {
                        byte: 100,
                        point: point(0, 100),
                    },
                },
            }],
        };
        assert!(!validate_plan(source, &plan));
    }

    #[test]
    fn rejects_reversed_range() {
        let source = "hello";
        let mut plan = DecorationPlan {
            document_epoch: 0,
            generation: 0,
            source_len: source.len(),
            styles: vec![span(StyleKind::Strong, source, 0, 3)],
        };
        // 手动构造一个 start > end 的反向 range——真实 analyze() 永远不会
        // 产出这种东西，这里就是在验证"就算它出现了也会被拒绝"。
        let start = plan.styles[0].range.start;
        let end = plan.styles[0].range.end;
        plan.styles[0].range.start = end;
        plan.styles[0].range.end = start;
        assert!(!validate_plan(source, &plan));
    }

    #[test]
    fn rejects_mid_utf8_boundary() {
        let source = "中文";
        let mut plan = DecorationPlan {
            document_epoch: 0,
            generation: 0,
            source_len: source.len(),
            styles: vec![span(StyleKind::Strong, source, 0, 3)],
        };
        // "中" 是 3 字节；把 end 手动改到字符中间。
        plan.styles[0].range.end.byte = 1;
        assert!(!validate_plan(source, &plan));
    }

    #[test]
    fn rejects_point_byte_disagreement() {
        let source = "hello world";
        let mut plan = DecorationPlan {
            document_epoch: 0,
            generation: 0,
            source_len: source.len(),
            styles: vec![span(StyleKind::Strong, source, 0, 5)],
        };
        // point 和 byte 两个坐标现在互相矛盾。
        plan.styles[0].range.start.point = point(0, 6);
        assert!(!validate_plan(source, &plan));
    }

    #[test]
    fn rejects_invalid_heading_level() {
        let source = "hello";
        let mut plan = DecorationPlan {
            document_epoch: 0,
            generation: 0,
            source_len: source.len(),
            styles: vec![span(StyleKind::Heading(1), source, 0, 5)],
        };
        plan.styles[0].kind = StyleKind::Heading(0);
        assert!(!validate_plan(source, &plan));

        plan.styles[0].kind = StyleKind::Heading(7);
        assert!(!validate_plan(source, &plan));
    }

    #[test]
    fn allows_overlapping_heading_and_strong_spans() {
        // "# **bold heading**" — Heading 覆盖整行，Strong 覆盖里面的
        // "**bold heading**"，两者合法重叠。validate_plan 不应该有任何
        // "range 不能重叠"的规则。
        let source = "# **bold heading**\n";
        let plan = analyze(source, 0, 0);
        assert!(validate_plan(source, &plan));
        let has_heading = plan
            .styles
            .iter()
            .any(|s| matches!(s.kind, StyleKind::Heading(1)));
        let has_strong = plan.styles.iter().any(|s| s.kind == StyleKind::Strong);
        assert!(has_heading && has_strong);
    }

    #[test]
    fn allows_overlapping_block_quote_and_emphasis_spans() {
        let source = "> *quoted emphasis*\n";
        let plan = analyze(source, 0, 0);
        assert!(validate_plan(source, &plan));
        let has_quote = plan.styles.iter().any(|s| s.kind == StyleKind::BlockQuote);
        let has_emphasis = plan.styles.iter().any(|s| s.kind == StyleKind::Emphasis);
        assert!(has_quote && has_emphasis);
    }
}
