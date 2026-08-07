//! Wires Phase 12A analysis + 12B rendering into the real editor's
//! `sourceview::Buffer`, on GTK's main thread, with a fixed debounce and
//! stale-result rejection.
//!
//! `LivePreview` deliberately owns its own analysis identity — a local
//! `(document_epoch, generation)` ticket — instead of reusing
//! `WorkspaceSession::edit_generation`. That field's semantics are
//! "save/dirty identity": `mark_document_dirty()` advances it even for a
//! frontmatter-only edit that never touches the body buffer at all (see
//! `frontmatter::refresh` → `set_current_frontmatter` → the GTK
//! `mark_document_dirty()` wrapper). If Live Preview keyed its staleness
//! check off `edit_generation`, a frontmatter edit could invalidate an
//! in-flight body analysis without ever producing a replacement one — the
//! first analysis might never land until the user happens to also edit the
//! body. Live Preview's generation only ever advances for events that
//! actually change what needs analyzing (a new document, a body edit, or an
//! explicit clear), so it stays a GTK-local runtime concept, not a
//! `WorkspaceSession` field.
//!
//! No single-flight/pending-queue scheduling: a fixed 200ms debounce plus
//! ticket-staleness rejection plus `adapter::apply_plan`'s own exact-source
//! equality check are the three layers of stale-result protection, and
//! that's sufficient until a real benchmark shows otherwise (see
//! `docs/LIVE_PREVIEW_SPIKES.md` §8 — no such benchmark exists yet).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::{gio, glib};

use super::adapter::apply_plan;
use super::analysis::analyze;
use super::tags::LivePreviewTags;

const DEBOUNCE: Duration = Duration::from_millis(200);

/// Identifies one analysis request. Two tickets are the same request only
/// if both fields match — a document switch (new `document_epoch`) and a
/// body edit (new `generation`) each independently invalidate every
/// previously issued ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnalysisTicket {
    document_epoch: u64,
    generation: u64,
}

/// The pure, GTK-free half of `LivePreview`'s staleness bookkeeping.
/// Every method returns the ticket that identifies the request it just
/// authorized, so callers never have to separately ask "what's current"
/// right after changing it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AnalysisClock {
    document_epoch: u64,
    generation: u64,
}

impl AnalysisClock {
    fn current(self) -> AnalysisTicket {
        AnalysisTicket {
            document_epoch: self.document_epoch,
            generation: self.generation,
        }
    }

    /// A new document is being installed: adopt its epoch and advance the
    /// generation, invalidating every ticket issued for the previous
    /// document (even one with the same generation number by coincidence).
    fn set_document(&mut self, epoch: u64) -> AnalysisTicket {
        self.document_epoch = epoch;
        self.generation = self.generation.wrapping_add(1);
        self.current()
    }

    /// The body changed (or a caller otherwise wants a fresh analysis of
    /// the same document): keep the epoch, advance the generation.
    fn advance(&mut self) -> AnalysisTicket {
        self.generation = self.generation.wrapping_add(1);
        self.current()
    }

    /// No document is being analyzed right now: adopt the given epoch
    /// (matching whatever `WorkspaceSession` transition triggered the
    /// clear) and advance the generation so any in-flight request is
    /// rejected on completion.
    fn clear(&mut self, epoch: u64) -> AnalysisTicket {
        self.document_epoch = epoch;
        self.generation = self.generation.wrapping_add(1);
        self.current()
    }

    fn is_current(self, ticket: AnalysisTicket) -> bool {
        self.current() == ticket
    }
}

struct Inner {
    buffer: sourceview::Buffer,
    tags: LivePreviewTags,
    timeout: RefCell<Option<glib::SourceId>>,
    clock: Cell<AnalysisClock>,
}

#[derive(Clone)]
pub struct LivePreview {
    inner: Rc<Inner>,
}

impl LivePreview {
    /// Installs `LivePreviewTags` into `buffer`'s tag table exactly once —
    /// callers must not construct a second `LivePreview` for the same
    /// buffer, and must not call this again on theme switches or document
    /// loads (`docs/LIVE_PREVIEW_SPIKES.md` §5's install-once contract).
    pub fn new(buffer: &sourceview::Buffer) -> Self {
        let tags = LivePreviewTags::install(buffer);
        Self {
            inner: Rc::new(Inner {
                buffer: buffer.clone(),
                tags,
                timeout: RefCell::new(None),
                clock: Cell::new(AnalysisClock::default()),
            }),
        }
    }

    /// A new document has just been installed into the buffer. Cancels any
    /// pending debounce, clears whatever semantic tags belonged to the
    /// previous document immediately (no flash of stale styling while the
    /// new analysis runs), and starts analyzing `source` without waiting
    /// for the debounce — the caller already has the full text in hand
    /// from `install_document`/`set_text`, there's nothing to wait for.
    pub fn set_document(&self, epoch: u64, source: String) {
        Inner::cancel_timeout(&self.inner);
        let mut clock = self.inner.clock.get();
        let ticket = clock.set_document(epoch);
        self.inner.clock.set(clock);
        self.inner.tags.clear(&self.inner.buffer);
        Inner::start(&self.inner, source, ticket);
    }

    /// The buffer's body changed (or a draft was just restored into it).
    /// `immediate = true` skips the debounce — used for draft recovery,
    /// where the body just changed via a direct `set_text` outside the
    /// normal `changed` handler and there's no reason to wait 200ms before
    /// re-analyzing it.
    pub fn schedule(&self, source: String, immediate: bool) {
        Inner::cancel_timeout(&self.inner);
        let mut clock = self.inner.clock.get();
        let ticket = clock.advance();
        self.inner.clock.set(clock);

        if immediate {
            Inner::start(&self.inner, source, ticket);
            return;
        }

        let weak = Rc::downgrade(&self.inner);
        let source_id = glib::timeout_add_local_once(DEBOUNCE, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.timeout.borrow_mut().take();
            Inner::start(&inner, source, ticket);
        });
        *self.inner.timeout.borrow_mut() = Some(source_id);
    }

    /// No document is displayed (workspace closed, a workspace was just
    /// installed with nothing selected yet, or the current document was
    /// deleted). Cancels any pending/in-flight request's ability to apply
    /// its result and clears CloudStack's own semantic tags immediately.
    pub fn clear(&self, epoch: u64) {
        Inner::cancel_timeout(&self.inner);
        let mut clock = self.inner.clock.get();
        clock.clear(epoch);
        self.inner.clock.set(clock);
        self.inner.tags.clear(&self.inner.buffer);
    }
}

impl Inner {
    fn cancel_timeout(this: &Rc<Self>) {
        if let Some(timeout) = this.timeout.borrow_mut().take() {
            timeout.remove();
        }
    }

    /// Runs `analyze` on a GIO blocking-pool thread — `analyze` is pure CPU
    /// work over a plain `String`, not I/O, and returns a `DecorationPlan`
    /// rather than `Result<_, AppError>`, so `tasks::run` (built for
    /// fallible domain/I/O operations) is the wrong fit; this follows
    /// `preview.rs`'s own `spawn_future_local` + `spawn_blocking` pattern
    /// instead. No GTK/GObject value crosses the worker boundary — only the
    /// owned `String` and the two `u64`s in `ticket`.
    fn start(this: &Rc<Self>, source: String, ticket: AnalysisTicket) {
        let weak = Rc::downgrade(this);
        glib::spawn_future_local(async move {
            let analyzed = gio::spawn_blocking(move || {
                let plan = analyze(&source, ticket.document_epoch, ticket.generation);
                (source, plan)
            })
            .await;

            let Some(inner) = weak.upgrade() else {
                return;
            };
            // Stale results are silently dropped -- never clear tags here.
            // A newer request may already be in flight (or about to be, via
            // a still-pending debounce); clearing now would create a flash
            // of no styling that the newer request's own completion would
            // otherwise have avoided.
            if !inner.clock.get().is_current(ticket) {
                return;
            }

            match analyzed {
                Ok((analyzed_source, plan)) => {
                    if let Err(error) =
                        apply_plan(&inner.buffer, &analyzed_source, &inner.tags, &plan)
                    {
                        log::warn!("Live Preview 语义高亮应用失败：{error:?}");
                        inner.tags.clear(&inner.buffer);
                    }
                }
                Err(_) => {
                    log::warn!("Live Preview 后台分析异常终止");
                    inner.tags.clear(&inner.buffer);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_schedule_invalidates_older_ticket() {
        let mut clock = AnalysisClock::default();
        let first = clock.advance();
        let second = clock.advance();
        assert_ne!(first, second);
        assert!(!clock.is_current(first));
        assert!(clock.is_current(second));
    }

    #[test]
    fn document_switch_invalidates_previous_ticket() {
        let mut clock = AnalysisClock::default();
        let old = clock.advance();
        let new = clock.set_document(7);
        assert_ne!(old, new);
        assert!(!clock.is_current(old));
        assert!(clock.is_current(new));
        assert_eq!(new.document_epoch, 7);
    }

    #[test]
    fn clear_invalidates_previous_ticket() {
        let mut clock = AnalysisClock::default();
        let old = clock.advance();
        let cleared = clock.clear(3);
        assert_ne!(old, cleared);
        assert!(!clock.is_current(old));
        assert!(clock.is_current(cleared));
        assert_eq!(cleared.document_epoch, 3);
    }

    #[test]
    fn same_epoch_newer_generation_rejects_old_ticket() {
        let mut clock = AnalysisClock::default();
        clock.set_document(1);
        let old = clock.advance();
        let new = clock.advance();
        assert_eq!(old.document_epoch, new.document_epoch);
        assert_ne!(old.generation, new.generation);
        assert!(!clock.is_current(old));
        assert!(clock.is_current(new));
    }

    #[test]
    fn different_epoch_rejects_old_ticket_even_with_same_generation_number() {
        let mut clock = AnalysisClock::default();
        let old = clock.set_document(1);
        // `set_document` 自己也会推进 generation，所以这里手动构造一个
        // "generation 数字碰巧相同、但 epoch 不同" 的旧 ticket，验证只看
        // generation 数字是不够的——两个字段必须同时匹配。
        let impostor = AnalysisTicket {
            document_epoch: old.document_epoch.wrapping_add(1),
            generation: old.generation,
        };
        assert_ne!(impostor.document_epoch, clock.current().document_epoch);
        assert!(!clock.is_current(impostor));
    }

    /// 这条测试本身就是一句可执行的设计说明：Live Preview 的 generation
    /// 是它自己私有的分析请求身份，跟 WorkspaceSession::edit_generation
    /// （保存/dirty 身份，frontmatter-only 编辑也会推进它）没有任何关系，
    /// 也不需要那边的任何类型就能验证。
    #[test]
    fn live_preview_generation_is_independent_of_workspace_session_edit_generation() {
        let mut clock = AnalysisClock::default();
        let ticket = clock.set_document(1);
        // 模拟一次只改 frontmatter、不改正文的编辑：Live Preview 完全不知情，
        // 它的 clock 没有任何理由前进，之前发出的 ticket 应该继续有效。
        assert!(clock.is_current(ticket));
    }
}
