//! Non-authoritative tree-sitter Markdown analysis: parses a source
//! snapshot and produces `StyleSpan`s for semantic styling. `tree-sitter-md`
//! is an editor-decoration parser, not CloudStack's Markdown authority —
//! `cloudstack-renderer` remains that (`docs/LIVE_PREVIEW_DESIGN.md` §4.4).
//!
//! Node kinds below were read directly out of `tree-sitter-md` 0.5's
//! `node-types.json` for both the block and inline grammars, not guessed.
//! V1 scope only: the eight `StyleKind` variants below, full-document
//! parsing, no incremental `InputEdit`, no concealment, no overlays.

use tree_sitter::Node;
use tree_sitter_md::MarkdownCursor;

use super::coordinates::{SourcePosition, SourceRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleKind {
    Heading(u8),
    Strong,
    Emphasis,
    Strikethrough,
    InlineCode,
    CodeBlock,
    BlockQuote,
    Link,
}

/// `range` is a half-open UTF-8 byte range `[start, end)` into the analyzed
/// source string.
///
/// For block-level kinds (`Heading`, `CodeBlock`, `BlockQuote`), `range`
/// follows the tree-sitter block node's own source extent as-is, which
/// includes the line's terminating `\n` (confirmed against real parses,
/// not assumed) — it is **not** normalized to a "visible characters only"
/// range. This is intentional parser/source geometry, not something to
/// "fix" at a later layer: a `GtkTextTag` legitimately wants paragraph
/// spacing/background to extend through the line break, so an adapter
/// consuming this span should apply it as given rather than trimming a
/// trailing `\n` it finds surprising.
///
/// `Link` is the one kind where `range` deliberately does **not** cover
/// the whole matched node: for `[label](dest)` /`[label][ref]` forms it
/// covers only the visible label (`link_text`), not the destination or
/// surrounding punctuation, since that's the substring V1 actually wants
/// to underline/color. Autolinks (`<https://…>`) have no separate label,
/// so `range` there is the whole autolink node — see `style_range` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleSpan {
    pub kind: StyleKind,
    pub range: SourceRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationPlan {
    pub document_epoch: u64,
    pub generation: u64,
    pub source_len: usize,
    pub styles: Vec<StyleSpan>,
}

/// Parses `source` as Markdown and produces a full-document decoration
/// plan. `document_epoch`/`generation` are carried through unchanged —
/// this function does no staleness checking of its own; callers compare
/// them against current state before applying the result.
///
/// Parsing failure (timeout/cancellation — not applicable here since
/// neither is configured, but `MarkdownParser::parse` can still return
/// `None`) degrades to an empty style list rather than panicking, per the
/// fail-visible invariant: no styling is a safe fallback, source text is
/// still fully readable either way.
pub fn analyze(source: &str, document_epoch: u64, generation: u64) -> DecorationPlan {
    let mut styles = Vec::new();

    let mut parser = tree_sitter_md::MarkdownParser::default();
    if let Some(tree) = parser.parse(source.as_bytes(), None) {
        let mut cursor = tree.walk();
        visit_all(&mut cursor, &mut |node| {
            if let Some(kind) = classify(node) {
                styles.push(StyleSpan {
                    kind,
                    range: style_range(node, kind),
                });
            }
        });
    }

    styles.sort_by_key(|span| span.range.start.byte);
    DecorationPlan {
        document_epoch,
        generation,
        source_len: source.len(),
        styles,
    }
}

fn node_range(node: Node<'_>) -> SourceRange {
    SourceRange {
        start: SourcePosition {
            byte: node.start_byte(),
            point: node.start_position(),
        },
        end: SourcePosition {
            byte: node.end_byte(),
            point: node.end_position(),
        },
    }
}

/// The range a `StyleSpan` should actually cover for `kind` at `node`.
/// Every kind except `Link` just uses the node's own extent. A link node
/// (`inline_link`/`shortcut_link`/`full_reference_link`/
/// `collapsed_reference_link`) spans its whole `[label](dest)`-shaped
/// source, brackets and destination included, but the substring V1 wants
/// to underline/color is only the visible label — the named `link_text`
/// child, which per `node-types.json` does not include the surrounding
/// `[`/`]` punctuation tokens. Autolinks (`uri_autolink`/`email_autolink`,
/// e.g. `<https://example.com>`) have no separate label child — the whole
/// node is already exactly the visible text, so it's used as-is.
fn style_range(node: Node<'_>, kind: StyleKind) -> SourceRange {
    if kind == StyleKind::Link {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "link_text" {
                return node_range(child);
            }
        }
    }
    node_range(node)
}

/// Classifies one node's `kind()` into a `StyleKind`, or `None` if this
/// node isn't styled in V1 (lists, tasks, tables, images, HTML blocks,
/// parser `ERROR` nodes, and everything else all fall through here —
/// unclassified content simply keeps rendering as plain source text, which
/// is the correct fail-visible behavior, not a gap to special-case).
fn classify(node: Node<'_>) -> Option<StyleKind> {
    match node.kind() {
        "atx_heading" => Some(StyleKind::Heading(atx_heading_level(node)?)),
        "strong_emphasis" => Some(StyleKind::Strong),
        "emphasis" => Some(StyleKind::Emphasis),
        // `~~text~~` parses as two nested `strikethrough` nodes (outer
        // `~~gone~~`, inner `~gone~` — an artifact of how the grammar
        // shares its rule between single- and double-tilde delimiters).
        // Only the outermost one should produce a span, or one `~~text~~`
        // would emit two near-duplicate overlapping spans.
        "strikethrough" if node.parent().is_some_and(|p| p.kind() == "strikethrough") => None,
        "strikethrough" => Some(StyleKind::Strikethrough),
        "code_span" => Some(StyleKind::InlineCode),
        "fenced_code_block" | "indented_code_block" => Some(StyleKind::CodeBlock),
        "block_quote" => Some(StyleKind::BlockQuote),
        "inline_link"
        | "shortcut_link"
        | "full_reference_link"
        | "collapsed_reference_link"
        | "uri_autolink"
        | "email_autolink" => Some(StyleKind::Link),
        _ => None,
    }
}

/// `atx_heading` doesn't carry a `level` field — the level is which of the
/// six `atx_h{1..6}_marker` children is present (`node-types.json`, block
/// grammar). Setext headings (`Heading\n===`) are not classified in V1:
/// they fall through `classify` as unstyled, which is a valid fail-visible
/// outcome, not a crash or a silent-wrong-answer.
fn atx_heading_level(node: Node<'_>) -> Option<u8> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "atx_h1_marker" => return Some(1),
            "atx_h2_marker" => return Some(2),
            "atx_h3_marker" => return Some(3),
            "atx_h4_marker" => return Some(4),
            "atx_h5_marker" => return Some(5),
            "atx_h6_marker" => return Some(6),
            _ => {}
        }
    }
    None
}

/// Depth-first walk over the full document. `MarkdownCursor` (unlike a
/// plain `tree_sitter::TreeCursor`) transparently descends into a block
/// node's associated inline tree when it reaches an `"inline"` or
/// `"pipe_table_cell"` node, so this one walk sees block and inline nodes
/// together without the caller needing to know which tree a node came from.
fn visit_all(cursor: &mut MarkdownCursor<'_>, visit: &mut impl FnMut(Node<'_>)) {
    visit(cursor.node());
    if cursor.goto_first_child() {
        loop {
            visit_all(cursor, visit);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(plan: &DecorationPlan, kind: StyleKind) -> Vec<&StyleSpan> {
        plan.styles
            .iter()
            .filter(|span| span.kind == kind)
            .collect()
    }

    fn slice<'a>(source: &'a str, span: &StyleSpan) -> &'a str {
        &source[span.range.start.byte..span.range.end.byte]
    }

    #[test]
    fn carries_epoch_generation_and_source_len_through_unchanged() {
        let plan = analyze("hello", 7, 42);
        assert_eq!(plan.document_epoch, 7);
        assert_eq!(plan.generation, 42);
        assert_eq!(plan.source_len, 5);
    }

    #[test]
    fn empty_source_produces_no_styles_and_does_not_panic() {
        let plan = analyze("", 0, 0);
        assert_eq!(plan.source_len, 0);
        assert!(plan.styles.is_empty());
    }

    #[test]
    fn classifies_atx_heading_levels_with_exact_byte_ranges() {
        // `atx_heading` 的 range 一路吃到行尾的 \n（不是只到可见文字结束）——
        // 这是从真实解析结果里发现的事实，不是假设。
        let source = "# One\n## Two\n### Three\n";
        let plan = analyze(source, 0, 0);
        let headings = find(&plan, StyleKind::Heading(1));
        assert_eq!(headings.len(), 1);
        assert_eq!(slice(source, headings[0]), "# One\n");
        assert_eq!(headings[0].range.start.byte, 0);
        assert_eq!(headings[0].range.end.byte, 6);

        let h2 = find(&plan, StyleKind::Heading(2));
        assert_eq!(slice(source, h2[0]), "## Two\n");

        let h3 = find(&plan, StyleKind::Heading(3));
        assert_eq!(slice(source, h3[0]), "### Three\n");
    }

    #[test]
    fn heading_content_is_still_classified_for_nested_inline_styles() {
        let source = "# Hello **world**\n";
        let plan = analyze(source, 0, 0);
        assert_eq!(find(&plan, StyleKind::Heading(1)).len(), 1);
        let strong = find(&plan, StyleKind::Strong);
        assert_eq!(strong.len(), 1);
        assert_eq!(slice(source, strong[0]), "**world**");
    }

    #[test]
    fn classifies_strong_emphasis_and_strikethrough_with_exact_ranges() {
        let source = "plain **strong** and *em* and ~~gone~~ text";
        let plan = analyze(source, 0, 0);

        let strong = find(&plan, StyleKind::Strong);
        assert_eq!(strong.len(), 1);
        assert_eq!(slice(source, strong[0]), "**strong**");

        let emphasis = find(&plan, StyleKind::Emphasis);
        assert_eq!(emphasis.len(), 1);
        assert_eq!(slice(source, emphasis[0]), "*em*");

        let strike = find(&plan, StyleKind::Strikethrough);
        assert_eq!(strike.len(), 1);
        assert_eq!(slice(source, strike[0]), "~~gone~~");
    }

    #[test]
    fn classifies_inline_code_with_exact_range() {
        let source = "call `foo(bar)` now";
        let plan = analyze(source, 0, 0);
        let code = find(&plan, StyleKind::InlineCode);
        assert_eq!(code.len(), 1);
        assert_eq!(slice(source, code[0]), "`foo(bar)`");
    }

    #[test]
    fn classifies_fenced_and_indented_code_blocks() {
        // 跟 heading 一样，fenced_code_block 的 range 一路吃到闭合围栏之后
        // 的换行符，覆盖整个源文本（含末尾 \n）。
        let source = "```rust\nfn main() {}\n```\n";
        let plan = analyze(source, 0, 0);
        let blocks = find(&plan, StyleKind::CodeBlock);
        assert_eq!(blocks.len(), 1);
        assert_eq!(slice(source, blocks[0]), source);

        let indented = "para\n\n    indented code\n";
        let plan = analyze(indented, 0, 0);
        let blocks = find(&plan, StyleKind::CodeBlock);
        assert_eq!(blocks.len(), 1);
        assert!(slice(indented, blocks[0]).contains("indented code"));
    }

    #[test]
    fn classifies_block_quote_with_exact_range() {
        // 一个空行把 blockquote 跟后面的段落隔开——没有空行的话，紧跟着的
        // "not quoted" 会被 CommonMark 的 lazy continuation 规则并进同一个
        // blockquote 段落里，"not quoted" 反而也会被判定成引用内容的一部分。
        // 这条测试特意避开那种情况，只验证 blockquote 自身的 range。
        let source = "> quoted line\n\nnot quoted\n";
        let plan = analyze(source, 0, 0);
        let quotes = find(&plan, StyleKind::BlockQuote);
        assert_eq!(quotes.len(), 1);
        assert_eq!(slice(source, quotes[0]), "> quoted line\n");
    }

    #[test]
    fn block_quote_lazy_continuation_absorbs_the_following_unmarked_line() {
        // 记录这个反直觉但正确的 CommonMark 行为，而不是假装它不存在：
        // 紧跟在 blockquote 段落后面、没有空行分隔、也没有 "> " 前缀的一行，
        // 会被解析成同一个 blockquote 段落的延续，而不是独立的正文。
        let source = "> quoted line\nnot quoted\n";
        let plan = analyze(source, 0, 0);
        let quotes = find(&plan, StyleKind::BlockQuote);
        assert_eq!(quotes.len(), 1);
        assert_eq!(slice(source, quotes[0]), source);
    }

    #[test]
    fn classifies_links_across_all_link_forms() {
        // 每种带 label 的链接形式（inline/shortcut/full reference/collapsed
        // reference）range 都只覆盖可见 label，不含方括号、目标 URL 或引用
        // 标记；autolink 没有独立 label，range 就是整个节点。
        let source = "[inline](https://example.com) and [shortcut] and \
            [full][ref] and [collapsed][] and <https://bare.example>\n\n\
            [shortcut]: https://ref.example\n\
            [ref]: https://ref.example\n\
            [collapsed]: https://ref.example\n";
        let plan = analyze(source, 0, 0);
        let links = find(&plan, StyleKind::Link);
        assert_eq!(links.len(), 5);
        let texts: Vec<&str> = links.iter().map(|span| slice(source, span)).collect();
        assert!(texts.contains(&"inline"), "{texts:?}");
        assert!(texts.contains(&"shortcut"), "{texts:?}");
        assert!(texts.contains(&"full"), "{texts:?}");
        assert!(texts.contains(&"collapsed"), "{texts:?}");
        assert!(texts.contains(&"<https://bare.example>"), "{texts:?}");
    }

    #[test]
    fn images_are_not_classified_as_links() {
        let source = "![alt](pic.png)\n";
        let plan = analyze(source, 0, 0);
        assert!(find(&plan, StyleKind::Link).is_empty());
    }

    #[test]
    fn tasks_and_nested_lists_are_valid_coordinate_fixtures_without_producing_spans() {
        let source = "- outer\n  - [ ] nested task\n  - [x] **done** task\n";
        let plan = analyze(source, 0, 0);
        // 列表和任务标记本身在 V1 不产出 span，但里面的内联样式必须仍然
        // 被正确定位——这就是把它们当"坐标 fixture"用的意思。
        let strong = find(&plan, StyleKind::Strong);
        assert_eq!(strong.len(), 1);
        assert_eq!(slice(source, strong[0]), "**done**");
    }

    #[test]
    fn incomplete_and_malformed_markdown_does_not_panic_and_stays_fail_visible() {
        for source in [
            "**unterminated bold",
            "[bad link(",
            "> ",
            "```\nunterminated fence",
            "# \n",
            "***",
        ] {
            let plan = analyze(source, 0, 0);
            assert_eq!(plan.source_len, source.len());
            // 每个 span 的 byte range 都必须落在 source 边界内——不管解析
            // 出多少错误节点，都不应该产出越界的坐标。
            for span in &plan.styles {
                assert!(span.range.start.byte <= span.range.end.byte);
                assert!(span.range.end.byte <= source.len());
            }
        }
    }

    #[test]
    fn every_span_start_position_resolves_to_its_start_byte() {
        // 用 §2 的 resolve_point 交叉验证：analyze() 产出的每个 span 起点
        // 都必须是一个真实、可以从 tree-sitter Point 精确解回同一个 byte
        // offset 的位置——这条测试把 §2（坐标）和 §4（分析）两部分接在
        // 一起验证，而不是分开假设两边永远一致。
        let source = "# 标题 😀\n\n> quote with **bold** and `code`\n\n\
                       - [ ] task with a [link](https://example.com)\n";
        let plan = analyze(source, 0, 0);
        assert!(!plan.styles.is_empty());
        for span in &plan.styles {
            let resolved = super::super::coordinates::resolve_point(source, span.range.start.point);
            assert_eq!(
                resolved,
                Some(span.range.start.byte),
                "span kind={:?} point={:?}",
                span.kind,
                span.range.start.point
            );
        }
    }

    #[test]
    fn ascii_and_unicode_sources_produce_in_bounds_spans() {
        for source in [
            "plain ascii **bold** text",
            "中文 **粗体** 文本",
            "emoji 😀 **bold 😀** text",
            "combining e\u{0301} **b\u{0301}old** text",
            "zwj 👨\u{200D}👩\u{200D}👧\u{200D}👦 **bold** text",
        ] {
            let plan = analyze(source, 0, 0);
            for span in &plan.styles {
                assert!(source.is_char_boundary(span.range.start.byte));
                assert!(source.is_char_boundary(span.range.end.byte));
            }
        }
    }
}
