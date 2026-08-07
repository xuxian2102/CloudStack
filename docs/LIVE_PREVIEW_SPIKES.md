# Live Preview Technical Spikes — Phase 11 Conclusions

**Status:** Phase 11 complete. Three spikes, implemented and manually verified on the `spike/live-preview-foundations` branch (not merged — see §9). This document is the only artifact that carries forward to `main`.

**Scope:** Validate the riskiest technical assumptions in `docs/LIVE_PREVIEW_DESIGN.md` before writing any Phase 12 production code: can tree-sitter source positions be reliably converted to `GtkTextIter`s, can CloudStack's own `TextTag`s coexist with GtkSourceView's built-in machinery, and which checkbox-marker rendering strategy is viable.

---

## 1. Tested environment

- Native Wayland session (`WAYLAND_DISPLAY` present), GTK4 + libadwaita + GtkSourceView5, same versions pinned in `crates/cloudstack-gtk/Cargo.toml`.
- `tree-sitter = "0.26"`, `tree-sitter-md = { version = "0.5", features = ["parser"] }` — spike-only dependencies, added to `cloudstack-gtk`'s `Cargo.toml` on the spike branch only. **Not present on `main`.**
- All three spikes are plain Rust binaries/test modules in `crates/cloudstack-gtk/src/live_preview_spike*.rs` and `src/bin/live_preview_spike_{2,3}.rs`, none wired into `main.rs`'s real module tree beyond a `mod` declaration needed to compile them.
- Spike 1's automated tests ran both under a normal desktop session and with `env -u DISPLAY -u WAYLAND_DISPLAY` to simulate CI's headless container (see §9).
- Spikes 2 and 3 were verified two ways: programmatic assertions (`cargo test`) and manual interaction — a real human clicking, tabbing, and reading actual screenshots, not just code inspection. Spike 3 in particular went through several rounds of "looks right to me" that manual testing then disproved; see §5.

---

## 2. Point → TextIter conclusions

**Verdict: the conversion is reliable, but only if application code never trusts GTK's own boundary handling.**

The core function ended up split in two, and that split is itself a conclusion, not just an implementation detail:

```rust
// Pure Rust. No GTK, no display, no gtk::init(). Fully testable in CI.
fn resolve_point(source: &str, point: tree_sitter::Point) -> Option<usize>;

// Thin GTK-touching wrapper. Requires gtk::init() to have already
// succeeded (a live display).
fn iter_at_point(buffer: &gtk::TextBuffer, source: &str, point: tree_sitter::Point)
    -> Option<gtk::TextIter>;
```

`resolve_point` walks `source.split('\n')`, rejecting a point whose `row` doesn't exist or whose `column` (a **byte** offset, per tree-sitter's convention — not a character count) isn't `<=` the line's byte length and a genuine `str::is_char_boundary`. `iter_at_point` calls `resolve_point` first and only then touches `gtk::TextIter`/`iter_at_line`/`set_line_index`.

A property test parsed 8 Markdown fixtures (heading, quote, task list, nested list, fenced code, table, mixed Unicode, link+quote+bold) through a real `tree_sitter_md::MarkdownParser` and walked every node in both the block and inline trees, asserting `resolve_point(source, node.start_position()) == Some(node.start_byte())` for all of them. This is the strongest test in the whole spike: it's not testing hand-picked coordinates, it's testing whatever tree-sitter itself produces against the buffer.

### Findings

1. **CI has no display during `cargo test`.** `.github/workflows/linux.yml`'s "Test workspace" step runs in the Arch container before any compositor exists — Wayland/sway only gets started later, scoped to the dedicated "Smoke test native GTK application" step, which explicitly unsets `DISPLAY`/`WAYLAND_DISPLAY` for its own isolated purposes. `gtk::init()` fails cleanly (returns `Err`, doesn't panic) when no display is reachable — confirmed by running the exact same test binary with `env -u DISPLAY -u WAYLAND_DISPLAY`. **Consequence for Phase 12: any unit test that constructs a real `gtk::TextBuffer`/`TextIter` cannot run in the normal `cargo test --workspace` CI lane as currently structured.** The `resolve_point`/`iter_at_point` split exists specifically so the Unicode-boundary logic — the part that actually needs dense test coverage — never needs a display, and only a thin, rarely-changing wrapper does.

2. **Under the current libtest structure, do not call `gtk::init()` from multiple `#[test]` functions in one binary.** libtest spawns a fresh OS thread per `#[test]` function, even under `--test-threads=1` (sequential execution still means "one thread each, one after another," not "reuse the same thread"). GTK hard-panics ("Attempted to initialize GTK from two different threads") the second time `gtk::init()` runs on a different thread within the same process. This is an observed consequence of how libtest schedules tests today, not a permanent GTK API law — but until that changes, **every GTK-touching test in a given test binary must be consolidated behind a single `#[test]` entrypoint** that calls `gtk::init()` once and then calls plain (non-`#[test]`) helper functions for each actual assertion group. This is not a style preference — the alternative panics.

3. **GTK clamps invalid byte offsets instead of rejecting them.** Probed the raw C API directly (`buffer.iter_at_line_index(row, byte_index)`, bypassing `resolve_point`'s own validation) with three invalid inputs on a buffer containing `"中文\nab"`:

   | input | result |
   |---|---|
   | `(0, 1)` — mid-character (byte 1 is inside the 3-byte `中`) | prints a `Gtk-WARNING`, then **returns `Some`**, silently clamped elsewhere |
   | `(0, 100)` — past the line's byte length | returns `None` (correct) |
   | `(5, 0)` — row past the last line | returns `None` (correct) |

   Only the mid-character case misbehaves, but it's exactly the case a real bug (an off-by-one in a UTF-8-aware caller) would actually produce. This empirically confirms `docs/LIVE_PREVIEW_DESIGN.md` §4.3's "GTK's clamping behavior must not be treated as validation" — the pre-validation in `resolve_point` is load-bearing, not defensive boilerplate that could be trimmed later.

---

## 3. Unicode boundary rules

Fixture coverage that passed, all as pure `resolve_point` unit tests (no display needed):

- ASCII, multi-line.
- Chinese characters (3-byte UTF-8 sequences), including resolving to a mid-string character correctly and rejecting the two invalid mid-character byte offsets inside one.
- Emoji (`😀`, a 4-byte sequence) — resolves to the start correctly, and the byte immediately after resolves to the following ASCII character.
- A combining mark (`e` + U+0301 COMBINING ACUTE ACCENT, 2 bytes) — resolves the base character and the combining mark's own start correctly; the single byte *inside* the 2-byte combining mark's encoding is correctly rejected.
- A ZWJ family emoji sequence (`👨‍👩‍👧‍👦` — 4 emoji + 3 ZWJ joiners, all multi-byte) — resolves the first character's start; every byte offset inside that first 4-byte sequence is rejected; the plain ASCII character immediately after the whole cluster still resolves correctly.
- Empty lines, including multiple consecutive empty lines in a row (checks the running byte-offset accumulator doesn't drift across several zero-length lines).
- Final line without a trailing newline — and, symmetrically, a point one row past that (which must be rejected: absence of a trailing newline is not the same as an implicit trailing empty row).

No property-based/fuzz testing was done — all fixtures above are hand-picked, not generated. That's an acceptable gap for a spike; Phase 12's actual test suite should consider `proptest`/`quickcheck`-style random Unicode generation per `docs/LIVE_PREVIEW_DESIGN.md` §23.3, which this spike did not attempt.

---

## 4. TextTag priority table

**Verdict: coexistence works, but two of GTK's documented-sounding behaviors turned out to be wrong in practice — verify, don't assume, even for things that sound like they must be true.**

Built a real `sourceview::Buffer` with the built-in Markdown language + an `Adwaita`/`Adwaita-dark` style scheme, then added a CloudStack tag registry (`cloudstack-live-heading`, `-strong`, `-inline-code`, `-link`, `-conceal`) on top, exercised alongside a live `SearchContext` and a real text selection.

### Findings

1. **`set_language()` populates zero syntax tags by itself**, and pumping the GLib main context afterward doesn't help either — GtkSourceView only actually highlights a range that a *connected `GtkTextView`* is about to draw. A bare buffer with no view attached never gets highlighted, no matter how long you iterate the main loop. `buffer.ensure_highlight(start, end)` forces it synchronously with **no view or window required at all** — this is good news for Phase 12, but scoped correctly: `GtkSourceBuffer` is still a GTK object and must only ever be touched from the GTK main thread, same as any other GTK type (`docs/LIVE_PREVIEW_DESIGN.md`'s own architecture keeps tree-sitter analysis on a worker thread operating on a plain `String` snapshot, applying results back on the main thread — that boundary doesn't move). What this finding actually buys Phase 12: code running on the GTK main thread — for example a tag-inspection/testing pass, or applying an already-computed `DecorationPlan` — can force syntax-tag population without creating a throwaway `GtkTextView` just to prime GtkSourceView's own tags.

2. **New tag priorities are relative to the whole tag table, not to CloudStack's own tags.** GtkSourceView's syntax tags are already sitting in the table (added via `set_language`) before CloudStack ever adds anything, occupying priorities `0..table.size()`. Naively assigning CloudStack's own tags priorities `0..5` (as the first implementation attempt did) places them *below* syntax highlighting, inverting the design's stated intent that CloudStack semantic styling sits above GtkSource's own highlighting. The fix is to read `table.size()` before adding CloudStack's tags and offset every one of their priorities by that count. A test locks this in by installing tags in an order deliberately different from their intended priority order and asserting the final `.priority()` values still land where intended, offset above the pre-existing syntax-tag count.

3. **`GtkTextIter::tags()` is not priority-sorted**, despite documentation phrasing that reads as if it should be. Applying five tags to one range in priority order `[heading, strong, inline-code, link, conceal]` and then calling `iter.tags()` at that position returns them back in exactly that *application* order — even though `conceal` has the highest priority of the five and should conceptually come first. **Resolving "which tag wins a given property" must always sort the returned list by `.priority()` explicitly; `iter.tags()`'s own ordering cannot be used as a shortcut.**

4. **Text selection is not a `TextTag` at all.** `GtkTextView` draws it directly from the buffer's selection bound/insert marks; selecting text does not add anything to the tag table (confirmed: `tag_table.size()` before and after `select_range` is identical). This means selection can never conflict with CloudStack's tag priorities — it's always drawn on top, by construction, not by priority ordering.

5. **`SearchContext`'s match highlighting *is* a real tag**, added lazily — the tag table's size only grows once a `SearchContext` with `set_highlight(true)` actually has an active search. One honest gap: in manual screenshot review (both light and dark), the *second* occurrence of a searched term did not visibly stand out from the current-line-highlight background as clearly as expected. This was not conclusively resolved — it may be a genuine low-contrast issue in the default `Adwaita` scheme's search-match style, or it may just be how it reads in a static screenshot. **Flagged open, not claimed as either a confirmed bug or confirmed non-issue.**

6. **An unrelated real bug, caught along the way:** a `gtk::ScrolledWindow` built without explicit `vexpand`/`hexpand` takes only its minimum natural height inside a vertical `Box` — the rest of the window silently renders as blank background. This looked *exactly* like "buffer content past line 1 has gone missing" (a tag/highlighting bug) until bisected step by step down to the missing expand flags. Worth remembering for any future GTK layout code: a mostly-blank window under a small chunk of visible content is almost always an expand-property bug, not a content bug.

---

## 5. Checkbox approach decision

Compared three strategies from `docs/LIVE_PREVIEW_DESIGN.md` §18 on a fixed two-line task-list fixture (`- [ ] Open task` / `- [x] Finished task`), live-switchable in one window so all three could be compared without relaunching:

- **A — transparent marker + overlay checkbox.** `foreground-rgba` alpha 0 on the `[ ]`/`[x]` text (preserves layout width), a real `gtk::CheckButton` positioned via `TextView::add_overlay`/`move_overlay` directly on top.
- **B — invisible marker + gutter icon.** `invisible` on the marker text (collapses to zero width), checkbox moved into the reserved left margin instead.
- **C — visible styled marker, clickable, no overlay widget.** Marker stays visible with a background/weight tag; a `GestureClick` on the view hit-tests marker ranges directly and toggles via a minimal `MarkdownEdit`-style buffer replacement.

### A real crash, and what it actually took to fix it

Initial testing (by the maintainer, clicking through it live — not caught by automated tests or by me reviewing the code alone) hit a hard crash: `Gtk-CRITICAL **: _gtk_widget_get_parent (child) == widget' failed`, repeating rapidly. Two rounds of fixes based on my own (wrong) theories didn't resolve it:

- First theory: reentrancy — the checkbox's own `toggled` handler was synchronously rebuilding (and thus destroying) the very widget whose signal was still firing. Deferred the rebuild via `glib::idle_add_local_once`. **Did not fix it.**
- Second theory: the deferred rebuild still landed while the checkbox's own CSS check/uncheck transition animation was in flight, so a mid-animation frame tried to snapshot a child mid-teardown. Restructured so toggling never rebuilds overlays at all. **This got closer but a maintainer code review caught the actual root cause before this was fully verified.**

The real bug, identified by a maintainer review of the source rather than further guessing: `clear_overlays()` called `Widget::unparent()` **directly** on children that had been added via `TextView::add_overlay()`. GTK's own documentation is explicit that `Widget::unparent()` is only for a custom widget's own `dispose` implementation — application code must go through the owning container's removal API, here `TextView::remove()`. Calling `unparent()` directly let GTK's internal overlay-child bookkeeping and the widget's actual parent pointer disagree, which is what produced the assertion during later snapshot/animation frames. The CSS-transition framing from the second fix attempt was a plausible-sounding but **unverified and ultimately incorrect** just-so story — a caution about presenting a theory as a settled root cause before it's actually confirmed.

The fix that stuck, adopted from that review in full:
- The two checkboxes are created exactly once, after initial layout, and **never destroyed again**. Switching variants only toggles visibility, re-tags the marker range, and repositions — it does not create or remove any widget, so the broken removal path is eliminated rather than patched.
- Dropped an unnecessary and subtly wrong `buffer_to_window_coords()` conversion — `TextView::iter_location()` already returns buffer coordinates, which is what `add_overlay`/`move_overlay` expect directly per GTK's docs.
- No `RefCell` `RefMut` is held across GTK calls that might reentrantly need the same borrow.
- Toggling derives the target checked state from the checkbox's own `is_active()` (the actual source of truth after a real user click) rather than inverting whatever text was found in the buffer, which could desync widget and buffer state under concurrent edits.
- Buffer edits are wrapped in `begin_user_action`/`end_user_action` so one click is one undo step, matching `docs/LIVE_PREVIEW_DESIGN.md` §16's `MarkdownEdit` executor contract.
- Checkboxes are looked up by a stored line number, not by `Vec` index (index-equals-line-number is a real footgun the moment any line's marker fails to parse).
- The click-hit-test range was off by one (`<=` on the marker's end offset, which could false-hit the character immediately after `]`) — fixed to a proper half-open range.

After this fix, the maintainer re-tested extensively (repeated clicks, all three variants, switching variants after toggling) and confirmed no further crashes.

### Confirmed interaction findings

- **Mouse click**: works correctly for all three variants.
- **Arrow-key cursor movement across a marker**: no problems observed, any variant.
- **Tab keyboard navigation**: does **not** reach the checkbox overlay in variant A or B. Confirmed directly: clicking into the editor and pressing Tab repeatedly just inserts a tab character into the document. This is not a bug — `GtkTextView` consumes Tab natively for text indentation before it ever becomes a focus-traversal key, and overlay children added via `TextView::add_overlay()` are not part of the view's normal focus chain. **Default/native Tab traversal does not reach these overlays** — this is a property of GTK's default behavior, not an unfixable limitation (Tab could in principle be intercepted and focus moved manually), but doing that solely to make checkbox overlays Tab-reachable is not worth it. This matches `docs/LIVE_PREVIEW_DESIGN.md` §18.1's own fallback language almost exactly: *"the first production version may keep the overlay non-focusable and expose a keyboard command for toggling the task at the cursor"* — i.e. the design doc already anticipated not relying on Tab, and this spike confirms that anticipation was correct, not optional caution. The conclusion is unchanged: production should use a dedicated toggle shortcut, not Tab.
- **Variant A visual quality** (the only variant given close visual scrutiny, including zooming into a screenshot): the overlay `CheckButton` reads as a visually foreign widget sitting on top of the text rather than part of it, doesn't precisely align with the marker's own 3-character glyph box (the checkbox's internal padding doesn't match), and — the most concrete finding — its *unchecked* state has very low contrast against a light background (a thin, near-invisible outline), while the *checked* state (solid blue fill, white checkmark) is highly visible. Confirmed by zooming into a captured screenshot, not just eyeballing at normal size.
- **Variant B and C visual quality**: only confirmed as "works, no problem" by the maintainer — not given the same close scrutiny as A. This is an honest gap in this round of testing, not a claim that B and C have no issues.
- **Not tested this round**: high-contrast theme, an actual screen reader (Orca or otherwise) reading the checkbox's accessible name/state, and a dedicated non-Tab keyboard shortcut for toggling (none was built — spike scope was click + native Tab only).

### Decision

```text
Primary:   C — visible, styled marker; click-to-toggle; no overlay widget.
Deferred:  A — transparent marker + overlay GtkCheckButton. Revisit only if
           a later OverlayManager already exists for other reasons (V3 block
           images) and a native checkbox proves worth the added
           accessibility/layout complexity — it is not a safe fallback to
           reach for today: it still has the low-contrast unchecked state,
           the Tab gap, and the overlay lifecycle complexity, none of which
           are resolved.
Rejected:  B — inherits every downside A has (Tab gap, overlay lifecycle
           complexity, the same crash class this spike spent three rounds
           fixing) plus its own extra fragility (a hardcoded gutter offset,
           unverified reflow risk from the marker's width collapsing to
           zero), without a distinct, verified benefit over A having been
           established in this round of testing.
```

Rationale for C as the production default: its advantage isn't a claim that it looks best — that was never given the same close visual scrutiny as A — it's that C carries the least verified complexity and risk. It introduces no overlay-widget lifecycle, doesn't change marker width, doesn't depend on overlay positioning, mouse-toggle is confirmed working, the source edit stays a minimal `[ ] ↔ [x]` replacement, and it never implies to a keyboard user that a Tab-focusable native checkbox exists — it's an interaction enhancement on top of a Markdown marker, not a promise of native-widget affordances. A and B both carry an *unmet* keyboard-navigation implication (a sighted keyboard user tabbing through the document gets no indication a checkbox exists at all, since focus never lands on it and nothing changes visually). The keyboard story for *any* variant still needs a dedicated shortcut per the design doc's own §18.1 fallback plan — that cost applies equally to C, so it isn't a point in A's favor. A is deliberately labeled *deferred*, not *fallback*: nothing about its current state makes it a safer place to land if C turns out to have a problem.

---

## 6. Rejected alternatives

- **Positioning overlays from `TextView::connect_realize` directly** (spike 1's initial attempt, corrected before spike 1 was committed): `iter_location()` returned the identical rect (same `y`) for every line when called synchronously at realize time, because Pango/GtkTextView layout isn't finished yet. Fixed by scheduling the first positioning pass via `glib::idle_add_local_once` instead.
- **`Widget::unparent()` for removing `TextView` overlay children** — see §5. Wrong API for application code; `TextView::remove()` is correct, but the better fix was avoiding the removal entirely by never destroying the checkboxes.
- **Destroying and recreating overlay widgets on every state change** — see §5. Even once the removal-API bug was fixed, the create/destroy churn itself was unnecessary complexity for a fixed-size overlay set; persistent widgets that toggle visibility are simpler and avoid an entire bug class.
- **Inferring toggle target state by inverting buffer text** — see §5. Fragile the moment widget and buffer state could plausibly diverge (undo, external reload); deriving the target from the widget's own `is_active()` is the more robust source of truth.
- **Trusting `GtkTextIter::tags()`'s return order as priority order** — see §4. Confirmed wrong; must sort by `.priority()` explicitly.
- **Assuming `set_language()` alone is sufficient to get syntax tags for inspection/testing** — see §4. Requires `ensure_highlight()`.

---

## 7. Screenshots

Screenshots were captured during spikes 2 and 3 (via `grim` against the session's Wayland compositor) and reviewed directly — both to confirm visual correctness (heading/strong/inline-code/link styling, concealment, light and dark themes) and to catch the low-contrast unchecked-checkbox issue in §5, which was only visible after zooming into a capture. They were used as working artifacts for this review and are **not committed to the repository** — they're reproducible on demand:

```bash
cargo run -p cloudstack-gtk --bin live_preview_spike_2                  # light
SPIKE2_DARK=1 cargo run -p cloudstack-gtk --bin live_preview_spike_2    # dark
cargo run -p cloudstack-gtk --bin live_preview_spike_3                  # interactive, all 3 variants
SPIKE3_DARK=1 cargo run -p cloudstack-gtk --bin live_preview_spike_3
```

(Both binaries only exist on the `spike/live-preview-foundations` branch — see §9.)

---

## 8. Measured timings

**Not done in this round.** None of the three spikes included benchmarking — no `criterion`/`cargo bench` harness was built, and no timing numbers were collected for parsing, tag application, or overlay positioning. `docs/LIVE_PREVIEW_DESIGN.md` §20/§23.4 calls for benchmark-first optimization once real implementation work starts; this spike phase deliberately stayed scoped to "does the mechanism work and what are its rules," not "how fast is it." Flagging this explicitly rather than presenting the absence of numbers as "performance is fine."

---

## 9. Phase 12 implementation constraints

Carried forward as binding constraints on the real implementation, derived directly from §2–§5 above:

1. **Coordinate conversion must stay split**: pure byte/Unicode-boundary validation (`resolve_point`-shaped, no GTK types) separate from the thin GTK-touching iterator lookup. The pure half needs dense test coverage in the normal `cargo test` lane; the GTK half cannot run there without CI changes (adding a headless X server / Xvfb to the "Test workspace" step, or moving such tests to the existing Wayland smoke-test lane) — no such CI change has been made or proposed yet.
2. **Any test suite that constructs real GTK widgets must consolidate all such tests into one `#[test]` function per binary**, calling `gtk::init()` exactly once, with plain helper functions underneath for each assertion group.
3. **Never trust GTK's own bounds handling for byte-offset validity** — always pre-validate in Rust before passing a byte offset into `GtkTextIter`/`TextBuffer` APIs. This is now empirically demonstrated, not just a defensive-programming preference.
4. **New `TextTag` priorities must be computed as an offset above `table.size()` at the time CloudStack's own tags are added**, not as small integers starting at 0 — GtkSourceView's syntax tags already occupy that low range.
5. **Never rely on `GtkTextIter::tags()`'s return order to resolve tag precedence** — sort by `.priority()` explicitly wherever "which tag wins" matters.
6. **`ensure_highlight(start, end)` is the correct way to force GtkSourceView tag population for analysis/testing purposes**, without needing a live `GtkTextView`.
7. **Do not destroy and recreate an overlay merely because its presentation state changed** — reuse the existing widget while its source identity remains valid, per the crash history in §5. This is not a blanket "create every overlay once, forever" rule: `docs/LIVE_PREVIEW_DESIGN.md` §17.3's viewport virtualization for V3 (offscreen overlays, e.g. for hundreds of block images, must not be instantiated at all) still applies, and genuinely leaving the managed set — source disappeared, or evicted from the viewport cache — is a real removal, not a state change, and should happen. The rule this spike actually demonstrated is narrower: *state changes* (checked/unchecked, variant switch) should update an existing overlay in place; only *identity changes* (source gone, viewport eviction) should destroy it, and re-entering the viewport later can create a fresh one. Any such removal of a `TextView`-managed overlay child must go through `TextView::remove()`, never `Widget::unparent()` directly.
8. **The checkbox marker strategy for V2/V3 production work is C (visible, styled, click-to-toggle)**, per §5's decision, with A held as a documented fallback for a later revisit once `OverlayManager` exists for other reasons.
9. **Any interactive marker (checkbox or otherwise) needs a dedicated, non-Tab keyboard shortcut for activation** — Tab-to-focus does not work for anything overlaid on a `GtkTextView`, confirmed directly, not assumed. This applies regardless of which checkbox variant ships.
10. **High-contrast theme behavior and actual screen-reader output remain unverified** and should be checked before any interactive marker ships in a release, not assumed safe from this spike's testing alone.

---

## Branch disposition

Per the original plan, the `spike/live-preview-foundations` branch (containing `tree-sitter`/`tree-sitter-md` as dependencies and the three `live_preview_spike*` modules/binaries) is **not merged into `main`**. Only this document is intended to land on `main`, as a standalone `docs:` commit. The spike branch can be kept around for reference or deleted once this document is confirmed to capture everything needed from it.
