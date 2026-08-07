//! Coordinate conversion between tree-sitter's byte/point model and
//! `GtkTextIter`. This is the Phase 11.1 spike contract
//! (`docs/LIVE_PREVIEW_SPIKES.md` §2), moved to production code unchanged,
//! with its resolution cost fixed in Phase 12E per
//! `docs/LIVE_PREVIEW_V1_BASELINE.md` §5.3/§9: `SourceIndex` replaces the
//! original from-the-start-of-document rescan with an O(1) row lookup,
//! after that rescan was measured to be essentially the entire cost of
//! `apply_plan` on large documents.
//!
//! `resolve_point`/`SourceIndex::resolve_point` are pure Rust — no GTK
//! types, no `gtk::init()`, no display — so they stay fully testable in
//! CI, which cannot construct a real `gtk::TextBuffer` (see the spike
//! doc's CI finding). `iter_at_point`/`SourceIndex::iter_at_point` are
//! thin wrappers that only touch GTK once resolution has already
//! validated the position.

use tree_sitter::Point;

/// A source position expressed two ways: a global UTF-8 byte offset (for
/// slicing/hashing the source string and future `InputEdit` support), and a
/// tree-sitter `Point` (row + UTF-8 byte column within that row, for
/// locating a `GtkTextIter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub byte: usize,
    pub point: Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

/// A line-start index over one source snapshot, letting `Point -> byte`
/// resolution be O(1) instead of rescanning from the beginning of the
/// document on every call. Built once (O(n)) and reused across every span
/// in a `DecorationPlan` — this is the fix for the bottleneck Phase 12D
/// measured (`docs/LIVE_PREVIEW_V1_BASELINE.md` §5.3): callers resolving
/// many points against the same source (`validate_plan`, `apply_plan`)
/// must build one `SourceIndex` and call its methods directly, not go
/// through the free functions below in a loop.
///
/// Intentionally minimal: CloudStack only ever needs UTF-8 row/byte-column
/// lookup (tree-sitter's own coordinate model), not the UTF-16/UTF-32
/// conversions an LSP-facing line-index abstraction would need — so this
/// stays a small internal type instead of an external dependency.
#[derive(Debug, Clone)]
pub struct SourceIndex {
    /// `line_starts[row]` is the global byte offset where `row` begins.
    /// A source ending in `\n` gets one extra trailing entry for the
    /// resulting empty trailing row — the same semantics
    /// `source.split('\n')` always had, preserved deliberately (see the
    /// `source_index_*` tests below).
    line_starts: Box<[usize]>,
}

impl SourceIndex {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (byte, value) in source.bytes().enumerate() {
            if value == b'\n' {
                line_starts.push(byte + 1);
            }
        }
        Self {
            line_starts: line_starts.into_boxed_slice(),
        }
    }

    /// Does `point` land on a real UTF-8 character boundary in `source`?
    /// No GTK involved. `point.column` follows tree-sitter's convention: a
    /// UTF-8 byte offset from the start of the row, not a character count.
    /// Returns the resolved global byte offset on success. O(1): the row
    /// lookup is a slice index instead of a scan.
    pub fn resolve_point(&self, source: &str, point: Point) -> Option<usize> {
        let line_start = *self.line_starts.get(point.row)?;
        let line_end = self
            .line_starts
            .get(point.row + 1)
            .map_or(source.len(), |&next| next - 1);
        let line_len = line_end.checked_sub(line_start)?;
        if point.column > line_len {
            return None;
        }
        let byte = line_start.checked_add(point.column)?;
        source.is_char_boundary(byte).then_some(byte)
    }

    /// Converts a `Point` into a `GtkTextIter`. Requires `gtk::init()` to
    /// have already succeeded (a live display) and `buffer`'s text to be
    /// exactly `source` — callers must additionally confirm that against
    /// document_epoch/generation/source hash before trusting the result;
    /// this function does not re-derive that itself.
    ///
    /// Never trusts GTK's own bounds handling: `resolve_point` is always
    /// checked first. A raw `gtk_text_buffer_get_iter_at_line_index` call
    /// with a byte offset that lands mid-UTF-8-character does not return
    /// `None` — it emits a `Gtk-WARNING` and silently clamps to a
    /// different position instead (confirmed in the Phase 11.1 spike).
    /// Skipping the `resolve_point` check here would reintroduce that
    /// failure mode.
    pub fn iter_at_point(
        &self,
        buffer: &gtk::TextBuffer,
        source: &str,
        point: Point,
    ) -> Option<gtk::TextIter> {
        use gtk::prelude::*;

        self.resolve_point(source, point)?;
        let row = i32::try_from(point.row).ok()?;
        let column = i32::try_from(point.column).ok()?;
        let mut iter = buffer.iter_at_line(row)?;
        iter.set_line_index(column);
        Some(iter)
    }
}

/// Convenience wrapper for a single, one-off lookup (tests, callers that
/// only ever resolve one point against a given source). Builds a fresh
/// `SourceIndex` internally, so it costs the same O(n) a single
/// `SourceIndex::new` call would — callers resolving *many* points
/// against the same source must build one `SourceIndex` themselves and
/// call its methods directly instead of calling this in a loop, or they
/// reintroduce the exact repeated-rescan cost this type exists to avoid.
///
/// Not called by production code (`validate_plan`/`apply_plan` build
/// their own `SourceIndex` directly) — only by this module's own tests
/// and the Phase 12D/12E baseline harness, which is exactly the
/// single-lookup use case this wrapper exists for.
#[allow(dead_code)]
pub fn resolve_point(source: &str, point: Point) -> Option<usize> {
    SourceIndex::new(source).resolve_point(source, point)
}

/// Convenience wrapper — see `resolve_point`'s doc comment; the same
/// "build one `SourceIndex`, don't call this in a per-span loop" caveat
/// applies. Used by the Phase 12D/12E baseline harness for untimed setup
/// code, not by production code.
#[allow(dead_code)]
pub fn iter_at_point(
    buffer: &gtk::TextBuffer,
    source: &str,
    point: Point,
) -> Option<gtk::TextIter> {
    SourceIndex::new(source).iter_at_point(buffer, source, point)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(row: usize, column: usize) -> Point {
        Point { row, column }
    }

    #[test]
    fn resolves_ascii() {
        let source = "hello\nworld";
        assert_eq!(resolve_point(source, point(0, 0)), Some(0));
        assert_eq!(resolve_point(source, point(1, 3)), Some(9));
        assert_eq!(source[9..].chars().next(), Some('l'));
    }

    #[test]
    fn resolves_chinese_characters() {
        let source = "中文\nab";
        assert_eq!(
            source[resolve_point(source, point(0, 0)).unwrap()..]
                .chars()
                .next(),
            Some('中')
        );
        assert_eq!(
            source[resolve_point(source, point(0, 3)).unwrap()..]
                .chars()
                .next(),
            Some('文')
        );
        assert_eq!(
            source[resolve_point(source, point(1, 0)).unwrap()..]
                .chars()
                .next(),
            Some('a')
        );
    }

    #[test]
    fn resolves_emoji() {
        // 😀 U+1F600 编码成 4 字节。
        let source = "😀x";
        assert_eq!(
            source[resolve_point(source, point(0, 0)).unwrap()..]
                .chars()
                .next(),
            Some('😀')
        );
        assert_eq!(resolve_point(source, point(0, 4)), Some(4));
        assert_eq!(source[4..].chars().next(), Some('x'));
    }

    #[test]
    fn resolves_combining_marks() {
        // "e" + COMBINING ACUTE ACCENT（U+0301，2 字节）。
        let source = "e\u{0301}b";
        assert_eq!(
            source[resolve_point(source, point(0, 0)).unwrap()..]
                .chars()
                .next(),
            Some('e')
        );
        assert_eq!(
            source[resolve_point(source, point(0, 1)).unwrap()..]
                .chars()
                .next(),
            Some('\u{0301}')
        );
        assert_eq!(
            source[resolve_point(source, point(0, 3)).unwrap()..]
                .chars()
                .next(),
            Some('b')
        );
        // 字节 2 落在 U+0301 的两字节编码中间。
        assert_eq!(resolve_point(source, point(0, 2)), None);
    }

    #[test]
    fn resolves_zwj_emoji_sequence() {
        // 👨‍👩‍👧‍👦：man + ZWJ + woman + ZWJ + girl + ZWJ + boy，一个视觉上的
        // grapheme，七个 Unicode 标量值，全部落在多字节 UTF-8 序列里。
        let source = "👨\u{200D}👩\u{200D}👧\u{200D}👦x";
        let first = resolve_point(source, point(0, 0)).unwrap();
        assert_eq!(source[first..].chars().next(), Some('👨'));
        // 第一个字符（U+1F468）是 4 字节；字节 1..4 都落在字符中间。
        assert_eq!(resolve_point(source, point(0, 1)), None);
        assert_eq!(resolve_point(source, point(0, 2)), None);
        assert_eq!(resolve_point(source, point(0, 3)), None);
        // 整个序列之后的 "x" 必须仍然可以正确定位。
        let x_offset = source.len() - 1;
        assert_eq!(resolve_point(source, point(0, x_offset)), Some(x_offset));
        assert_eq!(source[x_offset..].chars().next(), Some('x'));
    }

    #[test]
    fn resolves_empty_lines() {
        let source = "a\n\nb";
        assert_eq!(resolve_point(source, point(0, 0)), Some(0));
        // row 1 是空行；它唯一合法的 column 是 0。
        assert_eq!(resolve_point(source, point(1, 0)), Some(2));
        assert_eq!(resolve_point(source, point(1, 1)), None);
        assert_eq!(resolve_point(source, point(2, 0)), Some(3));
    }

    #[test]
    fn resolves_final_line_without_newline() {
        let source = "a\nb";
        assert_eq!(resolve_point(source, point(1, 0)), Some(2));
        assert_eq!(resolve_point(source, point(1, 1)), Some(3));
        // 没有第三行——末尾没有换行符不代表还有一个尾随的空行。
        assert_eq!(resolve_point(source, point(2, 0)), None);
    }

    #[test]
    fn resolves_multiple_consecutive_empty_lines() {
        let source = "a\n\n\n\nb";
        assert_eq!(resolve_point(source, point(0, 0)), Some(0));
        assert_eq!(resolve_point(source, point(1, 0)), Some(2));
        assert_eq!(resolve_point(source, point(2, 0)), Some(3));
        assert_eq!(resolve_point(source, point(3, 0)), Some(4));
        assert_eq!(resolve_point(source, point(4, 0)), Some(5));
        assert_eq!(source[5..].chars().next(), Some('b'));
    }

    #[test]
    fn rejects_row_past_last_line() {
        assert_eq!(resolve_point("a\nb", point(5, 0)), None);
    }

    #[test]
    fn rejects_column_past_line_end() {
        assert_eq!(resolve_point("hello", point(0, 10)), None);
    }

    #[test]
    fn rejects_column_mid_character() {
        // "中" 是 3 字节；字节 1 和 2 都落在字符中间。
        assert_eq!(resolve_point("中", point(0, 1)), None);
        assert_eq!(resolve_point("中", point(0, 2)), None);
        assert_eq!(resolve_point("中", point(0, 3)), Some(3));
    }

    #[test]
    fn source_index_line_starts_are_correct() {
        let index = SourceIndex::new("a\nbc\nd");
        assert_eq!(&*index.line_starts, [0, 2, 5]);
    }

    #[test]
    fn source_index_trailing_newline_creates_a_trailing_empty_row() {
        let source = "a\n";
        let index = SourceIndex::new(source);
        assert_eq!(&*index.line_starts, [0, 2]);
        assert_eq!(index.resolve_point(source, point(1, 0)), Some(2));
        assert_eq!(index.resolve_point(source, point(2, 0)), None);
    }

    #[test]
    fn source_index_no_trailing_newline_has_no_trailing_row() {
        let source = "a";
        let index = SourceIndex::new(source);
        assert_eq!(&*index.line_starts, [0]);
        assert_eq!(index.resolve_point(source, point(1, 0)), None);
    }

    /// Direct `SourceIndex` coverage (not routed through the free-function
    /// convenience wrapper) against a representative slice of every
    /// fixture category above, proving `SourceIndex::resolve_point`
    /// itself — the path `validate_plan`/`apply_plan` actually call —
    /// matches the pre-Phase-12E contract exactly.
    #[test]
    fn source_index_matches_the_original_contract_on_existing_fixtures() {
        let cases: &[(&str, (usize, usize), Option<usize>)] = &[
            ("hello\nworld", (0, 0), Some(0)),
            ("hello\nworld", (1, 3), Some(9)),
            ("中文\nab", (0, 3), Some(3)),
            ("😀x", (0, 4), Some(4)),
            ("e\u{0301}b", (0, 1), Some(1)),
            ("e\u{0301}b", (0, 2), None),
            ("a\n\nb", (1, 0), Some(2)),
            ("a\n\nb", (1, 1), None),
            ("a\n\nb", (2, 0), Some(3)),
            ("a\nb", (2, 0), None),
            ("hello", (0, 10), None),
            ("中", (0, 1), None),
            ("中", (0, 3), Some(3)),
        ];
        for (source, (row, column), expected) in cases.iter().copied() {
            let index = SourceIndex::new(source);
            assert_eq!(
                index.resolve_point(source, point(row, column)),
                expected,
                "source={source:?} point=({row},{column})"
            );
        }
    }
}
