# Live Preview V1 — Acceptance and Performance Baseline (Phase 12D–12E)

## 1. Purpose

Phase 12D does not add features. It answers three questions before Phase 12
is allowed to close and Phase 13 (conceal) begins:

1. Is V1 semantic styling correct, stable, and source-safe in the real
   editor?
2. What is actually expensive: the `tree-sitter` full analysis, or the
   `GtkTextTag` apply / GTK reflow?
3. Does Phase 13 genuinely need incremental parsing, viewport-only tag
   application, or single-flight scheduling — or none of the above?

The runtime landed in Phase 12C on the explicit premise "no benchmark
exists yet, so use a fixed 200ms debounce + stale ticket + exact-source
guard and nothing fancier." This document is that benchmark. The rule for
this phase was: **measure, then decide — do not optimize for "probably
faster."**

Phase 12D's answer to question 2 turned out to need one more round of
decomposition before it was actionable (§5.3); that round identified a
specific algorithmic defect, and Phase 12E (§5.4) fixed exactly that
defect and re-measured. Both phases' results are recorded in this one
document since they're one continuous measure→decompose→fix→re-measure
arc over the same baseline harness, not two independent efforts.

## 2. Tested commits / environment

| | |
|---|---|
| Phase 12D (before) commit | `568aba4644ecfa9d88023b739867934506593996` (`fix(gtk): guard stale live preview debounce callbacks`) |
| Phase 12E (after) commit | `3e19a0fbc2808bb078b349489d08945473805692` (`perf(gtk): index live preview source coordinates`) |
| rustc | 1.97.1 (8bab26f4f 2026-07-14) |
| cargo | 1.97.1 |
| CPU | AMD Ryzen AI 9 HX 370 w/ Radeon 890M (24 logical CPUs) |
| Kernel | 7.1.6-1-cachyos |
| GTK4 | 4.22.4 |
| GtkSourceView | 5.20.0 |
| Display protocol | Wayland (native, no XWayland) |
| Build profile | `--release` |

§5.1–5.3 (`analyze()`, `apply_plan()` totals, and the decomposition) were
measured at the "before" commit; §5.4 (the SourceIndex fix and its
re-measurement) at the "after" commit. All numbers below are from a
single machine, single run per commit. They are engineering evidence for
a design decision, not a cross-platform SLA.

## 3. Fixture construction

`crates/cloudstack-gtk/src/live_preview/baseline.rs` embeds one ~1 KiB
representative Markdown chunk (`FIXTURE_CHUNK`) mixing everything V1
classifies plus content that previously required its own coordinate-
conversion test fixtures:

- ATX headings (H1, H2)
- a paragraph with `**strong**`, `*emphasis*`, `~~strikethrough~~`, an
  inline `[link](...)`, and `` `inline code` ``
- a block quote carrying strong text, Chinese text, an emoji (😀), and an
  explicit combining-mark sequence (`e` + U+0301 COMBINING ACUTE ACCENT —
  not the precomposed `é`, matching `coordinates.rs`'s own test fixture)
- a fenced Rust code block
- a list with a mixed Chinese/ASCII item

Larger fixtures repeat this chunk whole (never truncate it, so every
repeat stays syntactically complete) until the target size is reached.
Actual byte sizes, not the nominal 10 KiB/100 KiB/1 MiB targets, are what's
recorded below, per the instruction not to force an exact byte count.

The same chunk was also written out to real files in a test project
(`~/Develop/WebTest/notes/live-preview-baseline-{10kib,100kib,1mib}.md`)
for the manual acceptance pass in §6, so the manual numbers and the
microbenchmark numbers describe the same documents.

## 4. Measurement methodology

`analyze()` and `apply_plan()` are measured **separately**, because they
run on different threads in production and a combined number can't tell
you which one to fix:

- `analyze()`: pure CPU work, runs on a `gio::spawn_blocking` worker
  thread in production, never on the GTK main thread.
- `apply_plan()`: runs synchronously on the GTK main thread once the
  worker's result lands. This is the number that can actually freeze the
  UI.

For each fixture size:

| Fixture | Warmup | Samples |
|---|---:|---:|
| ~10 KiB | 5 | 100 |
| ~100 KiB | 5 | 50 |
| ~1 MiB | 3 | 20 |

`std::time::Instant` around each call, `std::hint::black_box` on inputs/
outputs to prevent the optimizer from eliding the call. No timing
assertions anywhere — the numbers are printed (`--nocapture`), not
asserted, and the test itself is `#[ignore]`d so it never runs in CI.

`apply_plan` benchmarking reproduces the real editor's buffer setup order
closely (`window.rs` `build_window()`): Markdown language set on the
`sourceview::Buffer`, then `LivePreviewTags::install` (so tag priorities
land above GtkSourceView's own syntax tags, exactly as production does),
then the source text, then a precomputed `DecorationPlan`. It explicitly
does **not** capture the later Pango/GSK layout/paint cost — `apply_plan`
returns once tag table mutations are issued, not once a frame has
actually been drawn. That remaining cost is what §6's manual pass is for.

Reproduce with:

```
cargo test -p cloudstack-gtk --release --bin cloudstack \
    live_preview::baseline -- --ignored --nocapture
```

## 5. Results

### 5.1 `analyze()`

| Fixture | Actual size | Spans | min | p50 | p95 | max |
|---|---:|---:|---:|---:|---:|---:|
| ~10 KiB | 10,368 B | 270 | 5.6 ms | 5.9 ms | 12.7 ms | 13.4 ms |
| ~100 KiB | 102,528 B | 2,670 | 57.9 ms | 62.1 ms | 105.3 ms | 119.3 ms |
| ~1 MiB | 1,048,704 B | 27,310 | 672.7 ms | 716.0 ms | 908.8 ms | 924.4 ms |

Roughly linear in size (10x size → ~10x time, both size steps), and this
entire cost lives on a background thread — it never blocks the GTK main
loop by itself.

### 5.2 `apply_plan()`

| Fixture | min | p50 | p95 | max |
|---|---:|---:|---:|---:|
| ~10 KiB | 1.4 ms | 2.2 ms | 2.3 ms | 2.4 ms |
| ~100 KiB | 103.1 ms | 113.5 ms | 143.9 ms | 155.3 ms |
| ~1 MiB | 11.69 s | 12.43 s | 12.86 s | 13.22 s |

This is **not** linear. 10 KiB → 100 KiB is a 10x size increase but a
~50x apply-time increase. 100 KiB → 1 MiB is another ~10x size increase
but a **~110x** apply-time increase. `apply_plan` runs on the GTK main
thread — the 1 MiB row means a single semantic-styling update can block
the entire window, synchronously, for twelve-plus seconds.

### 5.3 `apply_plan()` decomposition

`apply_plan` was split into its three internal phases and each measured
independently, against the same fixtures, to find out which part of it
is actually responsible for §5.2's super-linear cost: `validate_plan`
(pure, GTK-free consistency check), resolving every span's
`SourcePoint`s to `GtkTextIter`s, and the `GtkTextTag` clear+apply
mutation itself. A fourth measurement, raw `resolve_point()` calls with
no GTK involved at all (same call count as inside `validate_plan`),
isolates the specific mechanism under suspicion: `resolve_point`
rescans the source from the beginning of the document on every call.

| Fixture | `validate_plan` | raw `resolve_point` ×2/span | `SourcePoint`→`TextIter` | tag clear+apply | (sum) | measured `apply_plan` |
|---|---:|---:|---:|---:|---:|---:|
| ~10 KiB | 681 µs | 632 µs | 609 µs | 270 µs | ~1.6 ms | 1.8 ms |
| ~100 KiB | 54.7 ms | 54.6 ms | 56.0 ms | 3.1 ms | ~113.8 ms | 108.2 ms |
| ~1 MiB | 6.17 s | 6.16 s | 6.02 s | 41.8 ms | ~12.23 s | 12.39 s |

The sum of the (non-overlapping) phases — `validate_plan` +
`SourcePoint`→`TextIter` + tag clear+apply — reproduces the measured
`apply_plan` total almost exactly at every size. `validate_plan` and the
`SourcePoint`→`TextIter` loop are each individually almost identical to
the raw, GTK-free `resolve_point` number at the same size, which means
GTK's own iterator construction (`iter_at_line`/`set_line_index`) adds
essentially nothing on top of the coordinate scan. Tag clear+apply, by
contrast, stays cheap at every size tested — 41.8 ms even for 27,310
ranges at ~1 MiB, comfortably under the original 50 ms target for that
size.

This is direct, quantitative evidence, not a correlation: essentially
the entire `apply_plan` cost is `resolve_point`'s repeated
from-the-start-of-document scan, called twice inside `validate_plan`
and twice more inside the per-span `iter_at_point` resolution loop —
four scans per `StyleSpan`, each restarting from byte 0. `GtkTextTag`
mutation was never the bottleneck.

### 5.4 SourceIndex correction results (Phase 12E)

Phase 12E added `SourceIndex` (`coordinates.rs`): a per-source-snapshot
line-start table built once in O(n), giving `resolve_point` an O(1)
row lookup instead of the O(row) rescan §5.3 identified. `validate_plan`
and `apply_plan` now build one `SourceIndex` per call and share it
across every span, instead of each span independently rescanning from
byte 0. Re-measured with the same baseline harness, same fixtures, at
the "after" commit from §2:

| Fixture | `apply_plan` before | `apply_plan` after | Speedup |
|---|---:|---:|---:|
| ~10 KiB | 2.2 ms | 0.38 ms | ~6× |
| ~100 KiB | 113.5 ms | 4.2 ms | ~27× |
| ~1 MiB | 12.43 s | 68.9 ms | ~180× |

Decomposition, before vs. after, at the two sizes where it matters:

| Fixture | Phase | Before | After |
|---|---|---:|---:|
| ~100 KiB | `validate_plan` | 54.7 ms | 29.0 µs |
| ~100 KiB | coordinate resolution ×2/span | 54.6 ms | 27.7 µs |
| ~100 KiB | `SourcePoint`→`TextIter` | 56.0 ms | 926 µs |
| ~100 KiB | tag clear+apply | 3.1 ms | 3.0 ms |
| ~1 MiB | `validate_plan` | 6.17 s | 290 µs |
| ~1 MiB | coordinate resolution ×2/span | 6.16 s | 276 µs |
| ~1 MiB | `SourcePoint`→`TextIter` | 6.02 s | 10.0 ms |
| ~1 MiB | tag clear+apply | 41.8 ms | 38.1 ms |

Tag clear+apply is essentially unchanged, as expected — that code
wasn't touched. `analyze()` is likewise unchanged (still ~5.8ms / ~61ms
/ ~821ms p50 at 10 KiB/100 KiB/1 MiB), since Phase 12E never touched it
either.

One number this table doesn't fully account for: at ~1 MiB, summing the
three non-overlapping "after" phases (0.29 ms + 10.0 ms + 38.1 ms ≈
48.4 ms) doesn't quite reach the measured `apply_plan` total of 68.9 ms
— about 20 ms unattributed. Plausible sources include the full
`buffer.text()` source-equality read/copy, `resolved` vector
construction, per-span tag lookup, and other adapter-level overhead not
captured by any of the four decomposition measurements, plus ordinary
variance between separately-run benchmark segments. This residual is
recorded as **unattributed adapter/source-check overhead**, not claimed
as fully accounted for — it does not change the conclusion (§9): even
unattributed, 68.9 ms at 1 MiB is not a problem, and 100 KiB's full
`apply_plan` (4.2 ms) is not worth decomposing further to chase it down.

Manually re-confirmed on the real ~1 MiB fixture, per §6's original
acceptance list: the compositor-level "not responding" freeze no longer
reproduces on open, typing, backspace, Undo/Redo, scrolling, or search.
A separate, pre-existing editor↔render-preview scroll-sync lag was
still observed — but also on smaller documents, confirming it's
unrelated to this fix (see §8).

## 6. Manual acceptance matrix

Everything below except the two large-document rows and the source-
fidelity check was already exercised and confirmed working across the
two prior Phase 12C manual smoke-test rounds on this exact codebase (the
initial wiring round, and the debounce stale-ticket-guard fix round);
12D added zero production code, so those were not re-run from scratch.
New verification for 12D focused on what 12D actually changed the
picture on: real behavior at the sizes just measured, and confirming
Live Preview doesn't touch disk.

| Scenario | Result |
|---|---|
| Clean document styles immediately, stays clean | ✅ (12C rounds) |
| H1–H6 scale/spacing, marker still visible | ✅ (12C rounds) |
| strong/emphasis/strikethrough nested styling | ✅ (12C rounds) |
| inline/code block background, light+dark | ✅ (12C rounds) |
| Link: label-only underline, not URL/brackets | ✅ (12C rounds) |
| Rapid typing, no crash/flicker | ✅ (12C rounds) |
| Type then Undo — final styling matches final source | ✅ (12C rounds) |
| Rapid document switching, no stale-style contamination | ✅ (12C rounds) |
| Draft recovery restyles immediately | ✅ (12C rounds) |
| Delete article / close workspace clears styling | ✅ (12C rounds) |
| SearchContext + selection stay above semantic styling | ✅ (12C rounds) |
| **~10 KiB real typing/backspace/undo/search/scroll** | ✅ no perceptible issue |
| **~100 KiB real typing/backspace/undo/search/scroll** | ✅ **no perceptible stutter** — debounce absorbs the ~113ms apply cost; feels identical to a small document |
| **~1 MiB real typing/scroll** | ❌ **confirmed severe at this (pre-12E) commit** — see §5.4 for the post-fix re-test: opening the file froze the window into a compositor-level "not responding" state; typing afterward froze it again; once the semantic styling eventually landed it was visibly not tracking input (rendering lagged behind typing); scrolling was desynced between the editor and the render-preview pane — the two panes didn't move together, and the right pane's motion visibly lagged the left by a beat |
| **Source fidelity**: open a clean article, wait past styling completion, do not edit, check `git diff` | ✅ **empty diff**, file mtime unchanged — Live Preview does not write to disk |

The 1 MiB manual result is a direct, worse-than-microbenchmark
confirmation of §5.2: users don't experience "apply_plan took 12
seconds" in isolation, they experience the whole window becoming
unresponsive, more than once, with no feedback that anything is
happening.

The 100 KiB manual result is the one genuine discrepancy with the raw
number: `apply_plan` at ~113ms p50 sounds bad against a 16ms target. The
200ms debounce does not make that cost go away — it does not "absorb"
it in any real sense, it only guarantees the 113ms happens after the
user has stopped typing rather than mid-keystroke. That synchronous
main-thread cost is real and unchanged. What the manual pass shows is
narrower: at this specific size, occurring only after input has already
paused, it was not perceptible as a stutter. This matches the
instruction to trust real UI observation over a microbenchmark when they
disagree — the number and the underlying cost are both real, but at
100 KiB specifically it is currently acceptable in practice.

## 7. Performance interpretation

Mapping this onto the four scenarios from the 12D brief:

This is **Situation A** (analysis fast, apply slow) — but more extreme
than a borderline case, and §5.3's decomposition settles exactly which
part of "apply slow" is at fault. `analyze()` stays linear and
comfortably under budget even at 1 MiB (and it's off the main thread
regardless). `apply_plan()` is the confirmed main-thread bottleneck, and
its internal cost is now accounted for: the two coordinate-resolution
phases (`validate_plan` and the `SourcePoint`→`TextIter` loop) each
individually match the raw `resolve_point` cost at the same size, and
together they reproduce essentially the entire measured `apply_plan`
total. `GtkTextTag` clear+apply is cheap throughout — 41.8 ms at 1 MiB
for 27,310 ranges. Span counts scaled linearly with size (270 → 2,670 →
27,310); the super-linear growth in apply time tracks `resolve_point`'s
repeated from-the-start-of-document scan, not tag mutation.

Per the 12D decision framework, this rules out incremental tree-sitter
parsing as the fix (analysis was never the bottleneck), and it now also
rules out `GtkTextTag` mutation cost or a large span count as the fix
target. The evidence points at exactly one mechanism: `resolve_point`'s
linear rescan. Fixing coordinate resolution — not reducing how much of
the buffer gets tagged — is Phase 12E's task.

**Phase 12E result (§5.4):** the super-linear behavior came from
repeatedly resolving every tree-sitter `Point` by scanning the source
from its first line, once per resolution call, four times per
`StyleSpan`. `GtkTextTag` mutation itself remained approximately linear
and comparatively inexpensive throughout (41.8ms → 38.1ms at 1 MiB,
essentially unchanged by the fix, because it was never what needed
fixing). Indexing that one scan — not reducing how much of the buffer
gets tagged, not touching `GtkTextTag` mutation at all — resolved the
entire measured bottleneck: `apply_plan` at 1 MiB dropped from 12.43s to
68.9ms (~180×), and the real compositor-level freeze no longer
reproduces.

## 8. Observed visual issues / open gaps

- **1 MiB `apply_plan` freeze (confirmed, severe; RESOLVED in Phase
  12E)**: see §5.2/§5.4/§6. This was the actionable finding of Phase
  12D and the target Phase 12E fixed; manually re-confirmed gone on the
  same fixture.
- **Editor/render-preview scroll desync (open, out of scope for Phase
  12)**: initially observed only at 1 MiB, during the same session as
  the freeze, and recorded then as possibly related. Phase 12E's manual
  re-test found it **also on smaller documents**, unrelated to document
  size and unaffected by the `SourceIndex` fix — this rules out
  `apply_plan`/Live Preview tag application as the cause. It most
  likely belongs to the separate render-preview pane's scroll-sync
  (`preview.rs`), not anything Phase 12 built. Tracked as its own
  follow-up in `docs/todo-reliability.md` (P2); not a Phase 12 blocker
  and not addressed by this document's fix.
- No other visual issues found. H1–H6 spacing, nested strong/emphasis/
  strikethrough, code backgrounds in both themes, and link label-only
  underlining all matched the Phase 12A/12B design as already confirmed
  in the 12C rounds.

## 9. Decision

**Incremental parsing (tree-sitter `InputEdit`): NOT JUSTIFIED.**
Reason: `analyze()` is roughly linear, runs entirely off the main
thread, and stays well under any perceptible budget at realistic
document sizes (6ms at 10 KiB, 62ms at 100 KiB). Even its worst measured
case (1 MiB, p95 909ms) never blocks the UI by itself. There is no
evidence analysis cost is a real-world problem for this editor's actual
document sizes.

**Main-thread `apply_plan` scaling correction: REQUIRED — DONE (Phase
12E).**
Reason: `apply_plan` was the confirmed, measured, and manually-observed
bottleneck. It was super-linear in size (100 KiB→1 MiB was a 10x size
increase for a ~110x time increase), it runs on the GTK main thread, and
it produced a real, reproducible multi-second "not responding" freeze at
1 MiB — worse in the real app than the raw number alone suggested, since
it also visibly desynced from user input and from the scroll position
after recovering. §5.4 confirms the fix: `apply_plan` at 1 MiB dropped
from 12.43s to 68.9ms (~180×), and the freeze is manually confirmed gone.

**Source coordinate indexing: IMPLEMENTED.**
Reason: §5.3's decomposition accounted for essentially the entire
super-linear `apply_plan` cost — `resolve_point()` repeatedly scanned
from the beginning of the source for every endpoint it resolved, twice
inside `validate_plan`, twice more inside the per-span `iter_at_point`
loop. §5.4's `SourceIndex` (O(n) build, O(1) row lookup) fixed exactly
that mechanism: `validate_plan` and coordinate resolution both dropped
from seconds to microseconds at 1 MiB (6.17s → 290µs, 6.16s → 276µs),
while everything downstream of it (tag mutation, `analyze()`) stayed
unchanged, confirming the index was a sufficient fix on its own.

**Viewport-only tag application: NOT JUSTIFIED.**
Reason: `GtkTextTag` clear+apply itself measured only 41.8ms before the
fix and 38.1ms after, even for 27,310 ranges at ~1 MiB — comfortably
under the original 50ms target for that size, and unaffected by
`SourceIndex` because it was never the cost that needed fixing.
Reducing span count by limiting tag application to the visible range
would have solved a cost that was never the bottleneck. Not deferred
pending further data — the "after" numbers directly confirm indexing
alone was sufficient, so this is a closed no.

**Dirty-range decoration diff: NOT JUSTIFIED.**
Reason: this candidate was never separately measured (it wasn't part of
§4's original three-scenario framework), but the same §5.4 evidence
rules it out for the same reason as viewport scoping: full
`GtkTextTag` re-application after `SourceIndex` costs 4.2ms at 100 KiB
and 68.9ms at 1 MiB. Diffing to avoid re-applying unchanged ranges would
add real bookkeeping complexity to save time that isn't currently a
problem.

**Single-flight analysis: DEFERRED — only if huge-file CPU pileup is
actually observed.**
Reason: at 10 KiB and 100 KiB, `analyze()` (6ms / 62ms p50) stays
comfortably under the 200ms debounce, so continuous typing cannot cause
worker pileup at those sizes — there is nothing to fix today. It only
becomes a real risk at documents approaching 1 MiB, where `analyze()`
(716–821ms p50) exceeds the debounce window. Documents that large are
now actually editable (§5.4), so this is no longer moot the way it was
in the original 12D analysis — but it remains unmeasured, since nothing
in this baseline exercises rapid-typing worker concurrency directly.
Revisit only if real-world use surfaces worker pileup on very large
documents; do not implement pre-emptively against a scenario that
hasn't been observed.

## 10. Phase 13 entry conditions

Per the process this phase was run under ("measure → document → review →
decide; if the result shows something must be optimized, open a
dedicated targeted phase for it before continuing — otherwise Phase 12
closes here"): Phase 12D found something that had to be optimized,
identified exactly which mechanism (§5.3), and Phase 12E fixed that
mechanism and re-measured (§5.4).

**Phase 12 is complete.** The confirmed main-thread blocker has been
removed, the 1 MiB "not responding" failure no longer reproduces, and no
additional pre-Phase-13 optimization is justified by the current
evidence (§9). The one open item found along the way — an editor/
render-preview scroll-sync lag — is confirmed unrelated to Live Preview
or this fix (present on documents of every size tested, unaffected by
`SourceIndex`) and is tracked separately in `docs/todo-reliability.md`
(P2), not blocking Phase 13.

Phase 13 (line-level conceal with cursor-line reveal) can begin.
