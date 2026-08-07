//! Non-authoritative tree-sitter Markdown analysis: parses a source
//! snapshot and produces `StyleSpan`s for semantic styling, plus (Phase
//! 13A) `ConcealSpan`s for the ranges V1 has proven safe to hide.
//! `tree-sitter-md` is an editor-decoration parser, not CloudStack's
//! Markdown authority — `cloudstack-renderer` remains that
//! (`docs/LIVE_PREVIEW_DESIGN.md` §4.4).
//!
//! Node kinds below were read directly out of `tree-sitter-md` 0.5's
//! `node-types.json` for both the block and inline grammars, and confirmed
//! against real parse-tree dumps for every construct below (a throwaway
//! debug tool, not committed) — not guessed. Phase 13A's conceal
//! derivation is still full-document, non-incremental, and produces plans
//! only; nothing in this module touches GTK, creates a
//! `GtkTextChildAnchor`, or applies concealment — that is a later phase's
//! job, once this module has proven which ranges are even safe to offer.

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

/// Which whitelisted Markdown syntax construct a `ConcealSpan` covers.
/// Phase 13A's whitelist is deliberately narrow — see `collect_conceals`'s
/// doc comment for exactly what is and isn't covered, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcealKind {
    HeadingMarker,
    StrongDelimiter,
    EmphasisDelimiter,
    StrikethroughDelimiter,
    InlineCodeDelimiter,
    LinkSyntax,
}

/// A `SourceRange` of raw Markdown syntax (a delimiter, a marker, link
/// bracket/destination punctuation) that Phase 13A has proven safe to
/// hide — not yet hidden by anything in this module. `StyleSpan` answers
/// "what should be styled"; `ConcealSpan` answers a completely different
/// question, "what markup could be hidden" — deliberately two separate
/// types rather than one `StyleSpan` with an optional conceal sub-range,
/// since e.g. a heading's `StyleSpan` legitimately covers the whole line
/// including its trailing `\n` (so a `GtkTextTag` can extend background/
/// spacing through the line break), while its conceal candidate covers
/// only the marker and the whitespace before the visible text — two
/// genuinely different coordinate contracts on the same construct, not
/// one range with an optional prefix.
///
/// Every `ConcealSpan` this module ever produces satisfies, by
/// construction: non-empty, half-open `[start, end)`, in bounds, UTF-8
/// boundary safe at both ends, `Point`/byte consistent (both are read
/// from the same tree-sitter node, which guarantees this itself), and
/// single-line (`start.point.row == end.point.row`, with no `\n` byte
/// inside it) — see `is_valid_conceal_span` and `push_delimiter_pair`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcealSpan {
    pub kind: ConcealKind,
    pub range: SourceRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationPlan {
    pub document_epoch: u64,
    pub generation: u64,
    pub source_len: usize,
    pub styles: Vec<StyleSpan>,
    pub conceals: Vec<ConcealSpan>,
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
    let mut conceals = Vec::new();

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
            collect_conceals(node, source, &mut conceals);
        });
    }

    styles.sort_by_key(|span| span.range.start.byte);

    conceals.sort_by_key(|span| (span.range.start.byte, span.range.end.byte));
    conceals.dedup();

    DecorationPlan {
        document_epoch,
        generation,
        source_len: source.len(),
        styles,
        conceals,
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

fn node_start(node: Node<'_>) -> SourcePosition {
    SourcePosition {
        byte: node.start_byte(),
        point: node.start_position(),
    }
}

fn node_end(node: Node<'_>) -> SourcePosition {
    SourcePosition {
        byte: node.end_byte(),
        point: node.end_position(),
    }
}

/// Dispatches one visited node to its `ConcealSpan` extractor, if its kind
/// is on the Phase 13A V1 whitelist. Everything else — autolinks, images,
/// fenced/indented code-block markers, blockquote/list/task markers,
/// reference definitions, tables/HTML/math, setext headings, and any
/// parser `ERROR`/incomplete node — produces no conceal candidate at all,
/// on purpose: V1 would rather show a little more raw Markdown than hide
/// something it isn't certain is safe (`docs/LIVE_PREVIEW_SPIKES.md`'s
/// fail-visible invariant, carried into concealment).
///
/// Every extractor below only ever reads node boundaries tree-sitter
/// itself already computed — never a second line-oriented scan of
/// `source`, and never a guess at grammar shape not confirmed against a
/// real parse tree (see this module's doc comment).
fn collect_conceals(node: Node<'_>, source: &str, conceals: &mut Vec<ConcealSpan>) {
    match node.kind() {
        "atx_heading" => conceal_heading_marker(node, source, conceals),
        "strong_emphasis" => {
            conceal_strong_or_emphasis(node, ConcealKind::StrongDelimiter, 2, source, conceals);
        }
        "emphasis" => {
            conceal_strong_or_emphasis(node, ConcealKind::EmphasisDelimiter, 1, source, conceals);
        }
        // Same nested-node grammar artifact `classify` already accounts
        // for (see its doc comment) — only the outermost `strikethrough`
        // produces a conceal candidate, or `~~gone~~` would get double
        // treatment from both this node and its own nested child.
        "strikethrough" if node.parent().is_some_and(|p| p.kind() == "strikethrough") => {}
        "strikethrough" => conceal_strikethrough(node, source, conceals),
        "code_span" => conceal_code_span(node, source, conceals),
        // Only `inline_link` -- `[label](dest)` -- is self-contained: its
        // destination lives inside the same node, so tree-sitter's syntax
        // recognition alone is enough to know it's a real link.
        // `shortcut_link`/`full_reference_link`/`collapsed_reference_link`
        // instead depend on a `link_reference_definition` elsewhere in the
        // document to actually resolve -- and tree-sitter-md classifies
        // them by shape alone, with no cross-check that a matching
        // definition exists (confirmed against a real parse of
        // `"[label][missing]"`, which produces a `full_reference_link`
        // node identical in shape to a resolved one). CloudStack's
        // semantic authority for whether an unresolved reference is even
        // a link at all is `cloudstack-renderer` (pulldown-cmark), not
        // this module -- concealing down to just the label here could
        // show clean link text for something the real preview renders as
        // literal `[label][missing]`. Deferred until analysis has a way
        // to confirm a reference actually resolves; conceal only what a
        // single tree-sitter node can prove safe.
        "inline_link" => conceal_link_syntax(node, source, conceals),
        _ => {}
    }
}

fn is_atx_marker(kind: &str) -> bool {
    matches!(
        kind,
        "atx_h1_marker"
            | "atx_h2_marker"
            | "atx_h3_marker"
            | "atx_h4_marker"
            | "atx_h5_marker"
            | "atx_h6_marker"
    )
}

/// Conceals the marker and the whitespace immediately before the visible
/// heading text: `node.start` (always the marker's own start — the marker
/// is always `atx_heading`'s first child) through the `inline` content
/// child's start, if one exists. A heading with no visible text at all
/// (`"###\n"`, `"### \n"`) has no `inline` child to anchor on; rather than
/// guess how much trailing whitespace to also hide by inspecting `source`
/// text (which would be exactly the "second line-oriented parser" this
/// module avoids), that case conceals only the marker itself and leaves
/// any trailing whitespace visible — safe, just not maximally tidy, and
/// tidiness for a heading with no text isn't a case worth the risk for.
///
/// Deliberately does not conceal a closing sequence (`"### Heading ###"`'s
/// trailing `"###"`): that closing run is not a distinct node at all — it
/// shows up as anonymous, unnamed `"#"` tokens inside the `inline`
/// content, indistinguishable in kind from a literal `#` character
/// appearing anywhere else in the heading text (confirmed against a real
/// parse of `"### C# Tips"`, where the `#` in `C#` produces the exact same
/// anonymous token). Concealing it would require re-deriving CommonMark's
/// own closing-sequence rule (preceded and followed only by whitespace)
/// by inspecting `source` — the parser has not "clearly recognized" a
/// closing marker in the sense that would make that safe.
fn conceal_heading_marker(node: Node<'_>, source: &str, conceals: &mut Vec<ConcealSpan>) {
    let mut cursor = node.walk();
    let Some(marker) = node
        .children(&mut cursor)
        .find(|child| is_atx_marker(child.kind()))
    else {
        return;
    };

    let mut cursor = node.walk();
    let end = node
        .children(&mut cursor)
        .find(|child| child.kind() == "inline")
        .map_or_else(|| node_end(marker), node_start);

    let span = ConcealSpan {
        kind: ConcealKind::HeadingMarker,
        range: SourceRange {
            start: node_start(marker),
            end,
        },
    };
    if is_valid_conceal_span(source, &span) {
        conceals.push(span);
    }
}

/// Shared extractor for `strong_emphasis` (`delimiter_count = 2`: `**`/
/// `__`) and `emphasis` (`delimiter_count = 1`: `*`/`_`). Both grammars
/// represent their delimiters as `delimiter_count` individual, single-byte
/// `emphasis_delimiter` children at the very start of the node and
/// `delimiter_count` more at the very end (confirmed against real parses
/// of `"**bold**"`/`"__bold__"`/`"*italic*"`/`"_italic_"`, including with
/// escaped/nested content between them, e.g. `"**esc\*aped**"` and
/// `"**a *b* c**"` — the escaped/nested content sits as its own non-
/// delimiter child in between and is never touched here) — never the
/// whole node, so content stays fully visible.
fn conceal_strong_or_emphasis(
    node: Node<'_>,
    kind: ConcealKind,
    delimiter_count: usize,
    source: &str,
    conceals: &mut Vec<ConcealSpan>,
) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    if children.len() < delimiter_count * 2 {
        return;
    }
    let opening = &children[..delimiter_count];
    let closing = &children[children.len() - delimiter_count..];
    let (Some(opening_span), Some(closing_span)) = (
        merged_delimiter_span(kind, opening),
        merged_delimiter_span(kind, closing),
    ) else {
        return;
    };
    push_delimiter_pair(conceals, source, opening_span, closing_span);
}

/// Merges a run of `nodes` (expected to all be `emphasis_delimiter` and
/// byte-contiguous with each other) into one `ConcealSpan`. Returns `None`
/// — no conceal produced, fail-visible — if any node in the run isn't
/// actually an `emphasis_delimiter`, or if the run isn't contiguous;
/// either would mean the real parse didn't match the shape this extractor
/// assumes, and guessing past that is exactly what this module avoids.
fn merged_delimiter_span(kind: ConcealKind, nodes: &[Node<'_>]) -> Option<ConcealSpan> {
    let first = *nodes.first()?;
    let last = *nodes.last()?;
    if nodes.iter().any(|node| node.kind() != "emphasis_delimiter") {
        return None;
    }
    for pair in nodes.windows(2) {
        if pair[0].end_byte() != pair[1].start_byte() {
            return None;
        }
    }
    Some(ConcealSpan {
        kind,
        range: SourceRange {
            start: node_start(first),
            end: node_end(last),
        },
    })
}

/// `~~text~~` parses as two nested `strikethrough` nodes (see `classify`'s
/// doc comment): the outer node's own direct children are one
/// `emphasis_delimiter` (a single `~`), the nested `strikethrough` node,
/// then one more `emphasis_delimiter` (the closing single `~`) — the
/// *other* tilde on each side belongs to the nested node's own children.
/// To conceal the full two-tilde delimiter on each side, this reaches one
/// level into that nested node and merges its outermost delimiter with
/// the outer node's own one, rather than conceal only one tilde per side
/// (leaving the other visible) or conceal the nested node's delimiters a
/// second time (which `collect_conceals`'s dispatch already skips).
///
/// A single-tilde `~text~` node (also valid strikethrough in this
/// grammar, confirmed against a real parse — it produces a flat
/// `strikethrough` node with no nested child) has no such nested node;
/// that case conceals the outer node's own one-tilde delimiters directly,
/// same as `emphasis`.
fn conceal_strikethrough(node: Node<'_>, source: &str, conceals: &mut Vec<ConcealSpan>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    let (Some(outer_open), Some(outer_close)) =
        (children.first().copied(), children.last().copied())
    else {
        return;
    };
    if outer_open.kind() != "emphasis_delimiter" || outer_close.kind() != "emphasis_delimiter" {
        return;
    }

    let nested = children
        .iter()
        .copied()
        .find(|child| child.kind() == "strikethrough");

    let (open_end_node, close_start_node) = match nested {
        Some(inner) => {
            let mut inner_cursor = inner.walk();
            let inner_children: Vec<Node<'_>> = inner.children(&mut inner_cursor).collect();
            let (Some(inner_open), Some(inner_close)) = (
                inner_children.first().copied(),
                inner_children.last().copied(),
            ) else {
                return;
            };
            if inner_open.kind() != "emphasis_delimiter"
                || inner_close.kind() != "emphasis_delimiter"
                || outer_open.end_byte() != inner_open.start_byte()
                || inner_close.end_byte() != outer_close.start_byte()
            {
                return;
            }
            (inner_open, inner_close)
        }
        None => (outer_open, outer_close),
    };

    let opening = ConcealSpan {
        kind: ConcealKind::StrikethroughDelimiter,
        range: SourceRange {
            start: node_start(outer_open),
            end: node_end(open_end_node),
        },
    };
    let closing = ConcealSpan {
        kind: ConcealKind::StrikethroughDelimiter,
        range: SourceRange {
            start: node_start(close_start_node),
            end: node_end(outer_close),
        },
    };
    push_delimiter_pair(conceals, source, opening, closing);
}

/// `code_span`'s first and last children are always `code_span_delimiter`
/// runs of matching backtick count — CommonMark's own code-span rule (an
/// opening/closing fence must match in length, or it isn't a code span at
/// all) means the grammar itself only ever produces a `code_span` node
/// when this already holds, confirmed for both single- and multi-backtick
/// fences (`` `code` ``, `` ``code ` inside`` ``) — but this still checks
/// rather than trusting it blindly, per this module's "never trust
/// grammar shape without confirming it" rule.
fn conceal_code_span(node: Node<'_>, source: &str, conceals: &mut Vec<ConcealSpan>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    if children.len() < 2 {
        return;
    }
    let first = children[0];
    let last = children[children.len() - 1];
    if first.kind() != "code_span_delimiter" || last.kind() != "code_span_delimiter" {
        return;
    }
    if first.end_byte() > last.start_byte() {
        return;
    }
    if first.end_byte() - first.start_byte() != last.end_byte() - last.start_byte() {
        return;
    }

    let opening = ConcealSpan {
        kind: ConcealKind::InlineCodeDelimiter,
        range: node_range(first),
    };
    let closing = ConcealSpan {
        kind: ConcealKind::InlineCodeDelimiter,
        range: node_range(last),
    };
    push_delimiter_pair(conceals, source, opening, closing);
}

/// Conceals the syntax around `inline_link`'s visible `link_text` — the
/// same child `style_range` already finds for `StyleKind::Link` — on both
/// sides: `node.start` through `link_text.start` (the opening `[`), and
/// `link_text.end` through `node.end` (the closing `]` plus the
/// `(destination)`). `collect_conceals` only dispatches `"inline_link"`
/// here — the other three labeled forms need a reference definition
/// elsewhere in the document to resolve, which this module can't confirm
/// (see `collect_conceals`'s doc comment). Autolinks
/// (`uri_autolink`/`email_autolink`) have no `link_text` child at all and
/// so never reach this function either way — they have no separate
/// label, so hiding their syntax would hide the only visible text they
/// have. Images are never classified as links at all (`classify` doesn't
/// match `"image"`), so this never runs for them either.
fn conceal_link_syntax(node: Node<'_>, source: &str, conceals: &mut Vec<ConcealSpan>) {
    let mut cursor = node.walk();
    let Some(link_text) = node
        .children(&mut cursor)
        .find(|child| child.kind() == "link_text")
    else {
        return;
    };

    let prefix = ConcealSpan {
        kind: ConcealKind::LinkSyntax,
        range: SourceRange {
            start: node_start(node),
            end: node_start(link_text),
        },
    };
    let suffix = ConcealSpan {
        kind: ConcealKind::LinkSyntax,
        range: SourceRange {
            start: node_end(link_text),
            end: node_end(node),
        },
    };
    push_delimiter_pair(conceals, source, prefix, suffix);
}

/// The full per-`ConcealSpan` contract: non-empty, half-open, in bounds,
/// UTF-8 boundary safe at both ends, and single-line with no `\n` byte
/// inside it. `Point`/byte consistency isn't independently re-derived
/// here (that would mean scanning `source` again — exactly the cost
/// `docs/LIVE_PREVIEW_V1_BASELINE.md` §5.3/§9 measured and fixed in Phase
/// 12E); every span's `Point` and byte offset both come from the same
/// tree-sitter node, which guarantees they already agree — verified by
/// this module's own tests cross-checking against `coordinates::
/// resolve_point`, not re-checked on every `analyze()` call.
fn is_valid_conceal_span(source: &str, span: &ConcealSpan) -> bool {
    let start = span.range.start.byte;
    let end = span.range.end.byte;
    if start >= end || end > source.len() {
        return false;
    }
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return false;
    }
    if span.range.start.point.row != span.range.end.point.row {
        return false;
    }
    !source[start..end].contains('\n')
}

/// Validates and pushes an opening/closing (or prefix/suffix) conceal
/// pair together, or neither. Beyond each span's own validity
/// (`is_valid_conceal_span`), this also requires both sides to land on
/// the *same* line as each other: a construct whose opening and closing
/// markers straddle a line break (`"**a\nb**"`) stays fully raw in Phase
/// 13A V1, rather than concealing one side without the other — see this
/// module's doc comment and the Phase 13A design rationale (selection
/// crossing a hidden region, deletion boundaries, wrapped lines, and
/// mid-edit incomplete states are all meaningfully simpler when this is a
/// hard rule applied to the whole construct, not a per-span exception).
fn push_delimiter_pair(
    conceals: &mut Vec<ConcealSpan>,
    source: &str,
    first: ConcealSpan,
    second: ConcealSpan,
) {
    if !is_valid_conceal_span(source, &first) || !is_valid_conceal_span(source, &second) {
        return;
    }
    if first.range.start.point.row != second.range.start.point.row {
        return;
    }
    conceals.push(first);
    conceals.push(second);
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

    fn find_conceals(plan: &DecorationPlan, kind: ConcealKind) -> Vec<&ConcealSpan> {
        plan.conceals
            .iter()
            .filter(|span| span.kind == kind)
            .collect()
    }

    fn conceal_slice<'a>(source: &'a str, span: &ConcealSpan) -> &'a str {
        &source[span.range.start.byte..span.range.end.byte]
    }

    /// Applies the union of `conceals`' byte ranges to `source` and returns
    /// what's left — the "visible projection" a real conceal-rendering
    /// layer would show. Overlapping/adjacent ranges are merged correctly
    /// (a later phase's job is rendering, not this module's, but nothing
    /// stops two conceal candidates from legitimately overlapping — e.g.
    /// nested emphasis in `***both***` — so tests need to be able to ask
    /// "what does the union actually hide" without assuming disjointness).
    fn visible_after_conceal(source: &str, conceals: &[ConcealSpan]) -> String {
        let mut ranges: Vec<(usize, usize)> = conceals
            .iter()
            .map(|span| (span.range.start.byte, span.range.end.byte))
            .collect();
        ranges.sort_unstable();

        let mut visible = String::new();
        let mut cursor = 0usize;
        for (start, end) in ranges {
            let start = start.max(cursor);
            if start > cursor {
                visible.push_str(&source[cursor..start]);
            }
            cursor = cursor.max(end);
        }
        visible.push_str(&source[cursor..]);
        visible
    }

    /// Every `ConcealSpan` in `plan` must satisfy the full Phase 13A
    /// contract (item 6): non-empty, half-open, in bounds, UTF-8 boundary
    /// safe, `Point`/byte consistent (cross-checked here against
    /// `coordinates::resolve_point`, the same technique
    /// `every_span_start_position_resolves_to_its_start_byte` already uses
    /// for `StyleSpan`), and single-line. Called at the end of every
    /// conceal-producing test below rather than duplicated inline.
    fn assert_all_conceals_are_contractually_valid(source: &str, plan: &DecorationPlan) {
        for span in &plan.conceals {
            let start = span.range.start.byte;
            let end = span.range.end.byte;
            assert!(start < end, "empty or reversed span: {span:?}");
            assert!(end <= source.len(), "out of bounds: {span:?}");
            assert!(source.is_char_boundary(start), "start mid-char: {span:?}");
            assert!(source.is_char_boundary(end), "end mid-char: {span:?}");
            assert_eq!(
                span.range.start.point.row, span.range.end.point.row,
                "conceal span crosses a line: {span:?}"
            );
            assert!(
                !source[start..end].contains('\n'),
                "conceal span contains a newline: {span:?}"
            );
            assert_eq!(
                super::super::coordinates::resolve_point(source, span.range.start.point),
                Some(start),
                "start Point/byte disagreement: {span:?}"
            );
            assert_eq!(
                super::super::coordinates::resolve_point(source, span.range.end.point),
                Some(end),
                "end Point/byte disagreement: {span:?}"
            );
        }
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

    // ---- Phase 13A: conceal range derivation ----

    #[test]
    fn conceals_atx_heading_markers_at_every_level() {
        let source = "# One\n## Two\n###### Six\n";
        let plan = analyze(source, 0, 0);
        let headings = find_conceals(&plan, ConcealKind::HeadingMarker);
        assert_eq!(headings.len(), 3);
        let slices: Vec<&str> = headings
            .iter()
            .map(|span| conceal_slice(source, span))
            .collect();
        assert!(slices.contains(&"# "), "{slices:?}");
        assert!(slices.contains(&"## "), "{slices:?}");
        assert!(slices.contains(&"###### "), "{slices:?}");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "One\nTwo\nSix\n"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn does_not_conceal_heading_closing_marker() {
        // The trailing "###" is not a distinct node -- it's anonymous "#"
        // tokens indistinguishable from a literal "#" in the heading text
        // (confirmed against "### C# Tips"'s real parse). Only the
        // opening marker+separator is ever concealed.
        let source = "### Heading ###\n";
        let plan = analyze(source, 0, 0);
        let headings = find_conceals(&plan, ConcealKind::HeadingMarker);
        assert_eq!(headings.len(), 1);
        assert_eq!(conceal_slice(source, headings[0]), "### ");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "Heading ###\n"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn conceals_heading_marker_without_a_final_newline() {
        // A heading with no trailing "\n" (end of document) still parses
        // as a real atx_heading -- confirmed against a real parse of
        // "# One\n### Two" (a bare "###" alone at EOF with nothing after
        // it does NOT parse as atx_heading at all, a different case this
        // test isn't making a claim about).
        let source = "# One\n### Two";
        let plan = analyze(source, 0, 0);
        let headings = find_conceals(&plan, ConcealKind::HeadingMarker);
        assert_eq!(headings.len(), 2);
        assert_eq!(visible_after_conceal(source, &plan.conceals), "One\nTwo");
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn indented_atx_marker_conceals_leading_indentation_too() {
        // CommonMark allows up to 3 leading spaces before an ATX marker.
        // A real parse confirms the marker node itself starts at the
        // indentation, not after it ("  ##" is one atx_h2_marker node,
        // not two spaces followed by a marker) -- so marker.start..
        // inline.start naturally conceals the indentation along with the
        // marker, pinned here as the intended behavior rather than an
        // accidental side effect of that node shape.
        let source = "  ## Heading\n";
        let plan = analyze(source, 0, 0);
        let headings = find_conceals(&plan, ConcealKind::HeadingMarker);
        assert_eq!(headings.len(), 1);
        assert_eq!(conceal_slice(source, headings[0]), "  ## ");
        assert_eq!(visible_after_conceal(source, &plan.conceals), "Heading\n");
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn heading_with_no_visible_text_conceals_only_the_marker() {
        // No `inline` child exists to anchor the conceal end on, so only
        // the marker itself is concealed -- any trailing whitespace stays
        // visible. Pinned as intended (safe, just not maximally tidy for
        // this rare degenerate case), not a range-derivation gap: see
        // `conceal_heading_marker`'s doc comment.
        let bare = "###\n";
        let plan = analyze(bare, 0, 0);
        let headings = find_conceals(&plan, ConcealKind::HeadingMarker);
        assert_eq!(headings.len(), 1);
        assert_eq!(conceal_slice(bare, headings[0]), "###");
        assert_eq!(visible_after_conceal(bare, &plan.conceals), "\n");

        let trailing_spaces = "###   \n";
        let plan = analyze(trailing_spaces, 0, 0);
        let headings = find_conceals(&plan, ConcealKind::HeadingMarker);
        assert_eq!(headings.len(), 1);
        assert_eq!(conceal_slice(trailing_spaces, headings[0]), "###");
        assert_eq!(
            visible_after_conceal(trailing_spaces, &plan.conceals),
            "   \n"
        );
    }

    #[test]
    fn conceals_strong_delimiters_both_asterisk_and_underscore_forms() {
        let source = "**bold** and __also__";
        let plan = analyze(source, 0, 0);
        let strong = find_conceals(&plan, ConcealKind::StrongDelimiter);
        assert_eq!(strong.len(), 4);
        let slices: Vec<&str> = strong
            .iter()
            .map(|span| conceal_slice(source, span))
            .collect();
        assert_eq!(slices, vec!["**", "**", "__", "__"], "{slices:?}");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "bold and also"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn conceals_emphasis_delimiters_both_asterisk_and_underscore_forms() {
        let source = "*italic* and _also_";
        let plan = analyze(source, 0, 0);
        let emphasis = find_conceals(&plan, ConcealKind::EmphasisDelimiter);
        assert_eq!(emphasis.len(), 4);
        let slices: Vec<&str> = emphasis
            .iter()
            .map(|span| conceal_slice(source, span))
            .collect();
        assert_eq!(slices, vec!["*", "*", "_", "_"], "{slices:?}");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "italic and also"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn conceals_strikethrough_delimiters_double_and_single_tilde_forms() {
        let source = "~~gone~~ and ~single~";
        let plan = analyze(source, 0, 0);
        let strike = find_conceals(&plan, ConcealKind::StrikethroughDelimiter);
        assert_eq!(strike.len(), 4);
        let slices: Vec<&str> = strike
            .iter()
            .map(|span| conceal_slice(source, span))
            .collect();
        assert_eq!(slices, vec!["~~", "~~", "~", "~"], "{slices:?}");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "gone and single"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn strikethrough_conceal_does_not_double_hide_the_nested_grammar_node() {
        // Regression guard for the exact bug the doc comment warns about:
        // ~~gone~~ must produce exactly two conceal spans (one "~~" each
        // side), not four from also processing the nested strikethrough
        // node's own delimiters a second time.
        let source = "~~gone~~";
        let plan = analyze(source, 0, 0);
        assert_eq!(
            find_conceals(&plan, ConcealKind::StrikethroughDelimiter).len(),
            2
        );
    }

    #[test]
    fn conceals_single_backtick_inline_code() {
        let source = "call `foo(bar)` now";
        let plan = analyze(source, 0, 0);
        let code = find_conceals(&plan, ConcealKind::InlineCodeDelimiter);
        assert_eq!(code.len(), 2);
        assert_eq!(conceal_slice(source, code[0]), "`");
        assert_eq!(conceal_slice(source, code[1]), "`");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "call foo(bar) now"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn conceals_variable_length_backtick_fences() {
        // The fence is never assumed to be exactly one backtick -- a
        // stray single backtick inside the content forces a two-backtick
        // (or longer) fence, and the conceal must match whatever length
        // the real fence actually is.
        let source = "``code ` inside`` and ```also ` ```";
        let plan = analyze(source, 0, 0);
        let code = find_conceals(&plan, ConcealKind::InlineCodeDelimiter);
        assert_eq!(code.len(), 4);
        let slices: Vec<&str> = code
            .iter()
            .map(|span| conceal_slice(source, span))
            .collect();
        assert_eq!(slices, vec!["``", "``", "```", "```"], "{slices:?}");
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "code ` inside and also ` "
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn conceals_inline_link_syntax_and_preserves_only_the_label() {
        let source = "[label](https://example.com)";
        let plan = analyze(source, 0, 0);
        let link = find_conceals(&plan, ConcealKind::LinkSyntax);
        assert_eq!(link.len(), 2);
        assert_eq!(conceal_slice(source, link[0]), "[");
        assert_eq!(conceal_slice(source, link[1]), "](https://example.com)");
        assert_eq!(visible_after_conceal(source, &plan.conceals), "label");
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    // `full_reference_link`/`collapsed_reference_link`/`shortcut_link` all
    // depend on a `link_reference_definition` elsewhere in the document to
    // actually resolve, and tree-sitter-md classifies them by syntactic
    // shape alone -- confirmed against a real parse of `"[label][missing]"`
    // (no matching definition anywhere), which still produces a
    // `full_reference_link` node identical in shape to a resolved one, and
    // likewise for `"[missing][]"`/`"[shortcut]"`. Since `cloudstack-
    // renderer` (pulldown-cmark), not this module, is the authority on
    // whether an unresolved reference is even a link at all, none of these
    // three forms are concealed in Phase 13A regardless of whether a
    // matching definition exists — see `collect_conceals`'s doc comment.
    // Every fixture below, resolved or not, must stay fully raw.

    #[test]
    fn does_not_conceal_full_reference_link_even_when_resolved() {
        let source = "[label][ref]\n\n[ref]: https://example.com\n";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::LinkSyntax).is_empty());
        assert_eq!(
            visible_after_conceal(source, &plan.conceals).lines().next(),
            Some("[label][ref]")
        );
    }

    #[test]
    fn does_not_conceal_full_reference_link_when_unresolved() {
        let source = "[label][missing]\n";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::LinkSyntax).is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
    }

    #[test]
    fn does_not_conceal_collapsed_reference_link_even_when_resolved() {
        let source = "[label][]\n\n[label]: https://example.com\n";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::LinkSyntax).is_empty());
        assert_eq!(
            visible_after_conceal(source, &plan.conceals).lines().next(),
            Some("[label][]")
        );
    }

    #[test]
    fn does_not_conceal_collapsed_reference_link_when_unresolved() {
        let source = "[missing][]\n";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::LinkSyntax).is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
    }

    #[test]
    fn does_not_conceal_shortcut_link_when_unresolved() {
        let source = "[shortcut]\n";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::LinkSyntax).is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
    }

    #[test]
    fn does_not_conceal_autolinks() {
        let source = "<https://example.com> and <mailto:a@example.com>";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::LinkSyntax).is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
    }

    #[test]
    fn does_not_conceal_images() {
        let source = "before ![alt](pic.png) after";
        let plan = analyze(source, 0, 0);
        assert!(plan.conceals.is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
    }

    #[test]
    fn does_not_conceal_block_quote_list_or_task_markers() {
        let source = "> quoted\n\n- item\n- [ ] task\n- [x] done\n";
        let plan = analyze(source, 0, 0);
        assert!(plan.conceals.is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
    }

    #[test]
    fn does_not_conceal_fenced_or_indented_code_block_markers() {
        let fenced = "```rust\nfn main() {}\n```\n";
        let plan = analyze(fenced, 0, 0);
        assert!(plan.conceals.is_empty());

        let indented = "para\n\n    indented code\n";
        let plan = analyze(indented, 0, 0);
        assert!(plan.conceals.is_empty());
    }

    #[test]
    fn conceals_nested_heading_strong_and_emphasis_together() {
        let source = "# Hello **world** and *there*\n";
        let plan = analyze(source, 0, 0);
        assert_eq!(find_conceals(&plan, ConcealKind::HeadingMarker).len(), 1);
        assert_eq!(find_conceals(&plan, ConcealKind::StrongDelimiter).len(), 2);
        assert_eq!(
            find_conceals(&plan, ConcealKind::EmphasisDelimiter).len(),
            2
        );
        assert_eq!(
            visible_after_conceal(source, &plan.conceals),
            "Hello world and there\n"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn nested_emphasis_inside_strong_leaves_the_right_text_visible() {
        // "**a *b* c**": the outer strong and inner emphasis are
        // independently visited and independently concealed -- this is
        // exactly the kind of legitimate overlap/adjacency
        // visible_after_conceal exists to handle correctly.
        let source = "**a *b* c**";
        let plan = analyze(source, 0, 0);
        assert_eq!(visible_after_conceal(source, &plan.conceals), "a b c");
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn triple_marker_emphasis_strong_nesting_hides_completely_or_not_at_all() {
        // "***both***" is emphasis wrapping strong_emphasis (or vice
        // versa, grammar-dependent) -- either fully concealing to "both"
        // or leaving everything raw is acceptable; a partially-broken
        // result like "*both" or "both**" is not.
        let source = "***both***";
        let plan = analyze(source, 0, 0);
        let visible = visible_after_conceal(source, &plan.conceals);
        assert!(
            visible == "both" || visible == source,
            "partially-broken conceal result: {visible:?}"
        );
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn escaped_delimiters_stay_raw() {
        // The escaped "\*" in the middle is backslash_escape, not
        // emphasis_delimiter -- it must never end up inside a conceal
        // range, and the real delimiter pair on each side is still found
        // correctly despite it sitting in between.
        let source = "**esc\\*aped**";
        let plan = analyze(source, 0, 0);
        let strong = find_conceals(&plan, ConcealKind::StrongDelimiter);
        assert_eq!(strong.len(), 2);
        assert_eq!(visible_after_conceal(source, &plan.conceals), "esc\\*aped");
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn incomplete_markers_produce_no_conceals() {
        for source in [
            "**unterminated bold",
            "[bad link(",
            "> ",
            "```\nunterminated fence",
            "***",
            "#Heading",
        ] {
            let plan = analyze(source, 0, 0);
            assert!(
                plan.conceals.is_empty(),
                "source={source:?} conceals={:?}",
                plan.conceals
            );
        }
    }

    #[test]
    fn multiline_delimiter_pairs_stay_fully_raw() {
        // The opening "**" and closing "**" individually never contain a
        // "\n" -- but they land on different lines from each other, and
        // Phase 13A V1 requires a construct's whole delimiter pair to
        // share one line, not just each half internally. No StrongDelimiter
        // conceal should be produced at all for this source.
        let source = "**a\nb**";
        let plan = analyze(source, 0, 0);
        assert!(find_conceals(&plan, ConcealKind::StrongDelimiter).is_empty());
        assert_eq!(visible_after_conceal(source, &plan.conceals), source);
        // The StyleSpan for the same construct is completely unaffected by
        // this -- V1 styling behavior does not change in Phase 13A.
        assert_eq!(find(&plan, StyleKind::Strong).len(), 1);
    }

    #[test]
    fn conceal_ranges_are_unicode_safe() {
        for source in [
            "# 中文标题\n",
            "**中文粗体**",
            "*emoji 😀 emphasis*",
            "~~combining e\u{0301} strike~~",
            "`code with 中文`",
            "[中文标签](https://example.com)",
        ] {
            let plan = analyze(source, 0, 0);
            assert!(!plan.conceals.is_empty(), "source={source:?}");
            assert_all_conceals_are_contractually_valid(source, &plan);
        }
    }

    #[test]
    fn conceal_ranges_are_safe_without_a_final_newline() {
        let source = "**bold** and *em* and `code`";
        let plan = analyze(source, 0, 0);
        assert!(!plan.conceals.is_empty());
        assert_all_conceals_are_contractually_valid(source, &plan);
    }

    #[test]
    fn conceals_are_sorted_and_deduplicated() {
        let source = "# Heading\n\n**a** *b* ~~c~~ `d` [e](f)\n";
        let plan = analyze(source, 0, 0);
        let mut sorted = plan.conceals.clone();
        sorted.sort_by_key(|span| (span.range.start.byte, span.range.end.byte));
        assert_eq!(plan.conceals, sorted);
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(sorted, deduped);
    }
}
