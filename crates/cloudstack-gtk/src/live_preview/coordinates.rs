//! Coordinate conversion between tree-sitter's byte/point model and
//! `GtkTextIter`. This is the Phase 11.1 spike contract
//! (`docs/LIVE_PREVIEW_SPIKES.md` §2), moved to production code unchanged.
//!
//! `resolve_point` is pure Rust — no GTK types, no `gtk::init()`, no
//! display — so it stays fully testable in CI, which cannot construct a
//! real `gtk::TextBuffer` (see the spike doc's CI finding). `iter_at_point`
//! is a thin wrapper that only touches GTK once `resolve_point` has already
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

/// Does `point` land on a real UTF-8 character boundary in `source`? No GTK
/// involved. `point.column` follows tree-sitter's convention: a UTF-8 byte
/// offset from the start of the row, not a character count. Returns the
/// resolved global byte offset on success.
pub fn resolve_point(source: &str, point: Point) -> Option<usize> {
    let mut offset = 0usize;
    for (row, line) in source.split('\n').enumerate() {
        if row == point.row {
            if point.column > line.len() || !line.is_char_boundary(point.column) {
                return None;
            }
            return Some(offset + point.column);
        }
        offset += line.len() + 1;
    }
    None
}

/// Converts a `Point` into a `GtkTextIter`. Requires `gtk::init()` to have
/// already succeeded (a live display) and `buffer`'s text to be exactly
/// `source` — callers must additionally confirm that against
/// document_epoch/generation/source hash before trusting the result; this
/// function does not re-derive that itself.
///
/// Never trusts GTK's own bounds handling: `resolve_point` is always
/// checked first. A raw `gtk_text_buffer_get_iter_at_line_index` call with a
/// byte offset that lands mid-UTF-8-character does not return `None` — it
/// emits a `Gtk-WARNING` and silently clamps to a different position instead
/// (confirmed in the Phase 11.1 spike). Skipping the `resolve_point` check
/// here would reintroduce that failure mode.
pub fn iter_at_point(
    buffer: &gtk::TextBuffer,
    source: &str,
    point: Point,
) -> Option<gtk::TextIter> {
    use gtk::prelude::*;

    resolve_point(source, point)?;
    let row = i32::try_from(point.row).ok()?;
    let column = i32::try_from(point.column).ok()?;
    let mut iter = buffer.iter_at_line(row)?;
    iter.set_line_index(column);
    Some(iter)
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
}
