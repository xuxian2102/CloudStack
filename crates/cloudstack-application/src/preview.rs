//! 实时预览的调度状态机：什么时候该启动哪个渲染任务、哪个渲染结果还有资格
//! 应用到界面。不依赖 GTK/WebKit——甚至不依赖 `cloudstack-renderer`，因为这
//! 里只协调"文本什么时候该渲染"，不做真正的 Markdown → HTML 转换（那仍然
//! 由 GTK 层持有的 `MarkdownRenderer` 完成）。
//!
//! 同时只允许一个渲染任务在飞（`active`）；飞行期间又来的新请求只保留最新
//! 一份到 `pending`，飞行完成后接着启动。`debounced` 单独跟踪"还没真正提交
//! 渲染、只是在等 GTK timer 到期"的那个票据，用来在 timer 到期时判断它是否
//! 已经被后续操作取代。
//!
//! **关键不变量**：`schedule()` 即使发现新正文和上一次成功应用的内容相同、
//! 不需要重新渲染，也必须无条件取消 `pending`/`debounced` 并推进
//! `generation`。否则会出现这种情况：预览显示 A → 用户输入 B（进入 debounce
//! 或已经在后台渲染）→ 用户快速撤销回 A → 因为 A 等于上次应用的内容就直接
//! 提前返回、什么都不做 → B 没有被取消或失效 → B 最终完成后覆盖掉本该保持
//! 是 A 的预览。`generation` 必须先推进，才能让"是否应用"的判断对 B 生效。

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewTicket {
    pub epoch: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRequest {
    pub ticket: PreviewTicket,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewAction {
    None,
    Start(PreviewRequest),
    Debounce {
        request: PreviewRequest,
        delay: Duration,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PreviewCompletion {
    pub apply_result: bool,
    pub next_request: Option<PreviewRequest>,
}

#[derive(Default)]
pub struct PreviewCoordinator {
    epoch: u64,
    generation: u64,
    /// 同时只允许一个渲染任务执行；这里只存票据，不存完整正文，避免
    /// coordinator 和 GTK 后台任务各自复制一份可能很大的正文。
    active: Option<PreviewTicket>,
    /// `active` 执行时，只保留最新的待执行请求。
    pending: Option<PreviewRequest>,
    /// GTK timer 持有实际 request；这里保存票据，用于在 timer 意外触发时
    /// 判断它是不是已经被取消/取代的旧请求。
    debounced: Option<PreviewTicket>,
    last_applied: Option<(u64, String)>,
}

impl PreviewCoordinator {
    /// 切换到另一篇文章/文档：换 epoch、丢弃"上次应用内容"的记忆（新文档的
    /// 内容跟旧文档的 last_applied 字符串相同也不该被当成"不需要渲染"），
    /// 然后按新内容立即调度一次渲染。
    pub fn set_document(&mut self, epoch: u64, source: String) -> PreviewAction {
        self.epoch = epoch;
        self.last_applied = None;
        self.schedule(source, true)
    }

    /// 关掉预览（没有打开的文档）：推进 generation 让任何飞行中的旧渲染结果
    /// 失效，清空 pending/debounced/last_applied。`active` 故意不touch——
    /// 对应的后台渲染任务仍在运行，会自己调用 `complete_render` 收尾（拿到
    /// 的结果会因为 epoch 不匹配而不被应用）。
    pub fn clear(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.debounced = None;
        self.last_applied = None;
    }

    pub fn schedule(&mut self, source: String, immediate: bool) -> PreviewAction {
        // 所有新调度都取代尚未启动的旧请求。
        self.pending = None;
        self.debounced = None;

        // 即使最后判断不需要重新渲染，也必须让正在运行/排队的旧请求失效，
        // 不能在这一步之前就因为内容相同而提前返回。
        self.generation = self.generation.wrapping_add(1);

        if self
            .last_applied
            .as_ref()
            .is_some_and(|(epoch, applied)| *epoch == self.epoch && applied == &source)
        {
            return PreviewAction::None;
        }

        let request = PreviewRequest {
            ticket: PreviewTicket {
                epoch: self.epoch,
                generation: self.generation,
            },
            source,
        };

        if immediate {
            self.enqueue(request)
        } else {
            let delay = debounce_duration(request.source.len());
            self.debounced = Some(request.ticket);
            PreviewAction::Debounce { request, delay }
        }
    }

    /// GTK 的 debounce timer 到期时调用。即使 GTK 已经 `SourceId::remove()`
    /// 过旧 timer，也不完全信任"不会被调用"——这里再核对一次票据仍然是当前
    /// 认可的那一个。
    pub fn debounce_elapsed(&mut self, request: PreviewRequest) -> Option<PreviewRequest> {
        if self.debounced != Some(request.ticket)
            || request.ticket.epoch != self.epoch
            || request.ticket.generation != self.generation
        {
            return None;
        }
        self.debounced = None;
        match self.enqueue(request) {
            PreviewAction::Start(request) => Some(request),
            PreviewAction::None => None,
            PreviewAction::Debounce { .. } => unreachable!("enqueue 不会返回 Debounce"),
        }
    }

    /// 后台渲染任务完成时调用一次，无论成功还是失败。只有成功且票据仍然是
    /// 当前 epoch/generation 时才应该把结果应用到界面；无论是否应用，都要
    /// 释放 `active` 并把排队的最新请求启动起来。
    pub fn complete_render(&mut self, request: PreviewRequest, success: bool) -> PreviewCompletion {
        debug_assert_eq!(
            self.active,
            Some(request.ticket),
            "complete_render 应该只用当前 active 的票据调用"
        );
        if self.active != Some(request.ticket) {
            return PreviewCompletion::default();
        }
        self.active = None;

        let apply_result = success
            && request.ticket.epoch == self.epoch
            && request.ticket.generation == self.generation;
        if apply_result {
            self.last_applied = Some((request.ticket.epoch, request.source));
        }

        let next_request = self.pending.take();
        if let Some(next) = &next_request {
            self.active = Some(next.ticket);
        }

        PreviewCompletion {
            apply_result,
            next_request,
        }
    }

    fn enqueue(&mut self, request: PreviewRequest) -> PreviewAction {
        self.pending = Some(request);
        if self.active.is_some() {
            PreviewAction::None
        } else {
            let request = self.pending.take().expect("刚刚存入的 pending 一定存在");
            self.active = Some(request.ticket);
            PreviewAction::Start(request)
        }
    }
}

pub fn debounce_duration(bytes: usize) -> Duration {
    if bytes > 500 * 1024 {
        Duration::from_millis(500)
    } else if bytes > 100 * 1024 {
        Duration::from_millis(350)
    } else {
        Duration::from_millis(200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(coordinator: &mut PreviewCoordinator, source: &str) -> PreviewRequest {
        match coordinator.schedule(source.to_string(), true) {
            PreviewAction::Start(request) => request,
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn pending_render_keeps_only_the_latest_request() {
        let mut coordinator = PreviewCoordinator::default();
        let first = start(&mut coordinator, "1");
        assert_eq!(coordinator.schedule("2".into(), true), PreviewAction::None);
        assert_eq!(coordinator.schedule("3".into(), true), PreviewAction::None);

        // `schedule("2"/"3", ..)` 各自都推进了一次 generation，所以 first 的
        // 结果这时候已经过期，不该被应用——但仍然必须正确交出排队里最新的
        // 那一个（"3"，中间的 "2" 被丢弃），而不是完全不管后续。
        let completion = coordinator.complete_render(first, true);
        assert!(
            !completion.apply_result,
            "first 完成时 generation 已经推进过，结果不该被应用"
        );
        let next = completion
            .next_request
            .expect("应该启动最新排队的请求，而不是中间那次");
        assert_eq!(next.source, "3");

        let completion = coordinator.complete_render(next, true);
        assert!(completion.apply_result);
        assert!(completion.next_request.is_none());
    }

    #[test]
    fn stale_generation_or_document_epoch_is_never_applied() {
        // generation 陈旧：新请求在旧请求还没完成时到达，generation 已推进。
        let mut coordinator = PreviewCoordinator::default();
        let first = start(&mut coordinator, "a");
        assert_eq!(coordinator.schedule("b".into(), true), PreviewAction::None);
        let completion = coordinator.complete_render(first, true);
        assert!(
            !completion.apply_result,
            "generation 已经推进，旧结果不应该被应用"
        );

        // epoch 陈旧：渲染还没完成时切到了另一篇文章。
        let mut coordinator = PreviewCoordinator::default();
        let first = start(&mut coordinator, "a");
        coordinator.clear(2);
        let completion = coordinator.complete_render(first, true);
        assert!(
            !completion.apply_result,
            "epoch 已经切换，旧文章的渲染结果不应该被应用"
        );
    }

    #[test]
    fn debounce_scales_with_document_size() {
        assert_eq!(debounce_duration(10), Duration::from_millis(200));
        assert_eq!(debounce_duration(101 * 1024), Duration::from_millis(350));
        assert_eq!(debounce_duration(501 * 1024), Duration::from_millis(500));
    }

    /// 回归测试：预览显示 A → 用户输入 B（进入 debounce，或者已经在后台渲染）
    /// → 用户快速撤销回 A → B 不能覆盖预览，无论它当时处于哪种状态。
    #[test]
    fn returning_to_last_applied_source_invalidates_newer_render() {
        // 场景一：B 还没真正开始渲染，只是在 debounce 里等着。
        let mut coordinator = PreviewCoordinator::default();
        let a = start(&mut coordinator, "A");
        let completion = coordinator.complete_render(a, true);
        assert!(completion.apply_result);

        let b_debounced = match coordinator.schedule("B".into(), false) {
            PreviewAction::Debounce { request, .. } => request,
            other => panic!("expected Debounce, got {other:?}"),
        };

        assert_eq!(
            coordinator.schedule("A".into(), true),
            PreviewAction::None,
            "内容跟 last_applied 相同，不需要重新渲染"
        );
        assert_eq!(
            coordinator.debounce_elapsed(b_debounced),
            None,
            "已经被撤销覆盖的 debounce 请求，即使 timer 意外触发也不能再启动渲染"
        );

        // 场景二：B 已经开始在后台渲染。
        let mut coordinator = PreviewCoordinator::default();
        let a = start(&mut coordinator, "A");
        let completion = coordinator.complete_render(a, true);
        assert!(completion.apply_result);

        let b_active = start(&mut coordinator, "B");
        assert_eq!(coordinator.schedule("A".into(), true), PreviewAction::None);
        let completion = coordinator.complete_render(b_active, true);
        assert!(
            !completion.apply_result,
            "撤销回 A 之后，仍在执行的旧 B 渲染结果不能覆盖预览"
        );
    }
}
