//! Phase 12D: a small, reusable manual performance baseline for Live
//! Preview V1 -- not a Criterion benchmark, not a CI gate. It exists so
//! Phase 13/14 can re-run the exact same measurement later and compare
//! numbers, instead of re-deriving a throwaway harness from scratch each
//! time (as Phase 11's spikes did).
//!
//! This module is only compiled under `#[cfg(test)]` (see
//! `live_preview::mod`) and its one test is `#[ignore]`d: it needs a real
//! display to construct a `sourceview::Buffer`, must be run with
//! `--release` for the numbers to mean anything, and its timings are
//! machine-specific evidence for a human decision, not an automated pass/
//! fail assertion. Run it explicitly:
//!
//! ```text
//! cargo test -p cloudstack-gtk --release --lib \
//!     live_preview::baseline -- --ignored --nocapture
//! ```
//!
//! Results and the resulting engineering decision are recorded in
//! `docs/LIVE_PREVIEW_V1_BASELINE.md`, not in this file.

use std::hint::black_box;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use sourceview::prelude::*;

use super::adapter::{apply_plan, validate_plan};
use super::analysis::{analyze, DecorationPlan};
use super::coordinates::{iter_at_point, resolve_point};
use super::tags::LivePreviewTags;

/// ~1 KiB of representative Markdown: headings, paragraph text mixing
/// strong/emphasis/strikethrough/inline code/a link, a block quote
/// carrying Chinese text, an emoji, and an explicit combining-mark
/// sequence (`e` + U+0301 COMBINING ACUTE ACCENT, not the precomposed
/// `é` -- this repo's coordinate tests already use that exact sequence
/// for combining-mark fixtures), a fenced code block, and a list.
/// Repeated verbatim to build larger fixtures rather than truncated, so
/// every repeat is still syntactically complete Markdown.
const FIXTURE_CHUNK: &str = "\
# Heading One

Paragraph with **strong**, *emphasis*, ~~strikethrough~~, a
[link](https://example.com/live-preview), and `inline code` mixed in.

> Quote carrying **strong** text, 中文内容, an emoji 😀, and a combining
> mark: e\u{0301}.

## Heading Two

```rust
fn example() {
    println!(\"hello, live preview\");
}
```

- list item one
- list item two, 中文项
- list item three

";

/// Repeats `FIXTURE_CHUNK` (never truncates it) until the result reaches
/// at least `target_bytes`. Callers record the actual length rather than
/// assuming it lands exactly on `target_bytes` -- see the module doc on
/// `manual_live_preview_performance_baseline`.
fn build_fixture(target_bytes: usize) -> String {
    let chunk_len = FIXTURE_CHUNK.len();
    let repeats = target_bytes.div_ceil(chunk_len).max(1);
    FIXTURE_CHUNK.repeat(repeats)
}

struct Timings {
    min: Duration,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

impl std::fmt::Display for Timings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "min={:?} p50={:?} p95={:?} max={:?}",
            self.min, self.p50, self.p95, self.max
        )
    }
}

fn summarize(mut samples: Vec<Duration>) -> Timings {
    samples.sort_unstable();
    let len = samples.len();
    let p95_index = ((len as f64) * 0.95).ceil() as usize;
    Timings {
        min: samples[0],
        p50: samples[len / 2],
        p95: samples[p95_index.saturating_sub(1).min(len - 1)],
        max: samples[len - 1],
    }
}

/// Measures `analyze()` alone -- the tree-sitter full-parse cost that, in
/// production, runs on a `gio::spawn_blocking` worker thread, never on
/// the GTK main thread.
fn bench_analyze(source: &str, warmup: usize, samples: usize) -> (Timings, usize) {
    let mut generation = 0u64;
    for _ in 0..warmup {
        generation += 1;
        let _ = black_box(analyze(black_box(source), 1, generation));
    }
    let mut durations = Vec::with_capacity(samples);
    let mut span_count = 0;
    for _ in 0..samples {
        generation += 1;
        let start = Instant::now();
        let plan = black_box(analyze(black_box(source), 1, generation));
        durations.push(start.elapsed());
        span_count = plan.styles.len();
    }
    (summarize(durations), span_count)
}

/// Measures `apply_plan()` alone, against a buffer already holding
/// `source` and a plan already known to match it -- isolates the GTK tag
/// table mutation / range resolution cost that runs on the GTK main
/// thread, from the `analyze()` cost measured separately above.
///
/// This does NOT include the Pango/GSK layout and paint cost that GTK
/// schedules after tags change -- `apply_plan` returns once the tag table
/// mutations are issued, not once the next frame has actually been drawn.
/// The manual UI acceptance pass in `docs/LIVE_PREVIEW_V1_BASELINE.md`
/// covers that remaining cost; this number alone cannot rule out a
/// render-time hitch that doesn't show up here.
fn bench_apply(
    buffer: &sourceview::Buffer,
    source: &str,
    tags: &LivePreviewTags,
    plan: &DecorationPlan,
    warmup: usize,
    samples: usize,
) -> Timings {
    for _ in 0..warmup {
        apply_plan(buffer, source, tags, plan).expect("apply_plan warmup call must succeed");
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        apply_plan(buffer, source, tags, plan).expect("apply_plan sample call must succeed");
        durations.push(start.elapsed());
    }
    summarize(durations)
}

/// Phase 12E step 1: measures `validate_plan()` alone -- the pure,
/// GTK-free consistency check that runs first inside `apply_plan`.
/// Internally calls `resolve_point` twice per span (once per end of the
/// range, via `validate_span`), so this number is not independent of
/// `bench_raw_resolve_point` below; comparing the two shows how much of
/// validate's own cost is that repeated scan versus its other per-span
/// checks (bounds, char-boundary, heading-level).
fn bench_validate_plan(
    source: &str,
    plan: &DecorationPlan,
    warmup: usize,
    samples: usize,
) -> Timings {
    for _ in 0..warmup {
        assert!(black_box(validate_plan(black_box(source), black_box(plan))));
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let ok = black_box(validate_plan(black_box(source), black_box(plan)));
        durations.push(start.elapsed());
        assert!(ok);
    }
    summarize(durations)
}

/// Phase 12E step 1: measures just the repeated `resolve_point` scan
/// that both `validate_plan` (via `validate_span`) and the resolve loop
/// below (via `iter_at_point`) perform -- one full pass over every
/// span's start and end point, no GTK involved at all. Isolates the
/// specific hypothesis under review: does `resolve_point`'s
/// from-the-start-of-document rescan (`source.split('\n')`, restarted
/// for every call) dominate, independent of anything GTK does.
fn bench_raw_resolve_point(
    source: &str,
    plan: &DecorationPlan,
    warmup: usize,
    samples: usize,
) -> Timings {
    for _ in 0..warmup {
        for span in &plan.styles {
            let _ = black_box(resolve_point(black_box(source), span.range.start.point));
            let _ = black_box(resolve_point(black_box(source), span.range.end.point));
        }
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        for span in &plan.styles {
            let _ = black_box(resolve_point(black_box(source), span.range.start.point));
            let _ = black_box(resolve_point(black_box(source), span.range.end.point));
        }
        durations.push(start.elapsed());
    }
    summarize(durations)
}

/// Phase 12E step 1: measures the per-span `SourcePoint -> GtkTextIter`
/// resolution loop exactly as `apply_plan` runs it (its phase 3: build
/// the full `(tag, start, end)` list before mutating any tag) --
/// includes both `resolve_point`'s scan (via `iter_at_point`) and GTK's
/// own `iter_at_line`/`set_line_index` cost, without the validation or
/// tag-mutation phases around it. Comparing this against
/// `bench_raw_resolve_point` isolates GTK's own iterator-construction
/// cost from the coordinate scan.
fn bench_resolve_all_iters(
    buffer: &sourceview::Buffer,
    source: &str,
    plan: &DecorationPlan,
    warmup: usize,
    samples: usize,
) -> Timings {
    let text_buffer: &gtk::TextBuffer = buffer.upcast_ref();
    for _ in 0..warmup {
        for span in &plan.styles {
            let _ = black_box(iter_at_point(text_buffer, source, span.range.start.point));
            let _ = black_box(iter_at_point(text_buffer, source, span.range.end.point));
        }
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        for span in &plan.styles {
            let _ = black_box(iter_at_point(text_buffer, source, span.range.start.point));
            let _ = black_box(iter_at_point(text_buffer, source, span.range.end.point));
        }
        durations.push(start.elapsed());
    }
    summarize(durations)
}

/// Phase 12E step 1: measures the tag-table mutation phase alone
/// (`apply_plan`'s phase 4) -- clearing CloudStack's previously-applied
/// tags and re-applying the new set, against already-resolved iterators
/// built once outside the timed loop. Isolates GTK's own `TextTag`
/// mutation cost from any coordinate-resolution cost measured above.
fn bench_tag_clear_and_apply(
    buffer: &sourceview::Buffer,
    tags: &LivePreviewTags,
    resolved: &[(&gtk::TextTag, gtk::TextIter, gtk::TextIter)],
    warmup: usize,
    samples: usize,
) -> Timings {
    for _ in 0..warmup {
        tags.clear(buffer);
        for (tag, start, end) in resolved {
            buffer.apply_tag(*tag, start, end);
        }
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start_time = Instant::now();
        tags.clear(buffer);
        for (tag, start, end) in resolved {
            buffer.apply_tag(*tag, start, end);
        }
        durations.push(start_time.elapsed());
    }
    summarize(durations)
}

#[test]
#[ignore = "manual release-mode Live Preview performance baseline; requires display"]
fn manual_live_preview_performance_baseline() {
    gtk::init().expect("gtk init for baseline harness");

    let plans: [(&str, usize, usize, usize); 3] = [
        ("~10 KiB", 10 * 1024, 5, 100),
        ("~100 KiB", 100 * 1024, 5, 50),
        ("~1 MiB", 1024 * 1024, 3, 20),
    ];

    for (label, target_bytes, warmup, samples) in plans {
        let source = build_fixture(target_bytes);
        println!("=== {label} (actual {} bytes) ===", source.len());

        let (analyze_timings, span_count) = bench_analyze(&source, warmup, samples);
        println!("  analyze():    {analyze_timings}  spans={span_count}");

        // Reproduces the real editor's buffer setup order (window.rs
        // build_window()) closely enough for apply_plan's cost to be
        // representative: Markdown language on the buffer, then
        // LivePreviewTags::install (so tag priorities land above
        // GtkSourceView's own syntax tags, exactly as production does),
        // then the source text, then a precomputed plan.
        let buffer = sourceview::Buffer::builder()
            .enable_undo(true)
            .highlight_matching_brackets(true)
            .highlight_syntax(true)
            .implicit_trailing_newline(false)
            .build();
        if let Some(language) = sourceview::LanguageManager::default().language("markdown") {
            buffer.set_language(Some(&language));
        }
        let tags = LivePreviewTags::install(&buffer);
        buffer.set_text(&source);
        let plan = analyze(&source, 1, 1);

        let apply_timings = bench_apply(&buffer, &source, &tags, &plan, warmup, samples);
        println!("  apply_plan(): {apply_timings}");

        // Phase 12E step 1: decompose apply_plan's internal cost before
        // assuming which part of it needs fixing (see
        // docs/LIVE_PREVIEW_V1_BASELINE.md §7/§9).
        let validate_timings = bench_validate_plan(&source, &plan, warmup, samples);
        println!("    validate_plan():             {validate_timings}");

        let raw_resolve_timings = bench_raw_resolve_point(&source, &plan, warmup, samples);
        println!("    raw resolve_point() x2/span: {raw_resolve_timings}");

        let resolve_iters_timings =
            bench_resolve_all_iters(&buffer, &source, &plan, warmup, samples);
        println!("    SourcePoint -> TextIter:     {resolve_iters_timings}");

        let text_buffer: &gtk::TextBuffer = buffer.upcast_ref();
        let resolved: Vec<(&gtk::TextTag, gtk::TextIter, gtk::TextIter)> = plan
            .styles
            .iter()
            .map(|span| {
                let start = iter_at_point(text_buffer, &source, span.range.start.point)
                    .expect("fixture span must resolve to a start iter");
                let end = iter_at_point(text_buffer, &source, span.range.end.point)
                    .expect("fixture span must resolve to an end iter");
                let tag = tags
                    .tag_for(span.kind)
                    .expect("fixture span kind must have an installed tag");
                (tag, start, end)
            })
            .collect();
        let clear_apply_timings =
            bench_tag_clear_and_apply(&buffer, &tags, &resolved, warmup, samples);
        println!("    tag clear+apply:             {clear_apply_timings}");
    }
}
