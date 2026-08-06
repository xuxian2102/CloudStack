use std::collections::VecDeque;
use std::path::PathBuf;

use cloudstack_core::model::{DraftDocument, PostDocument, ProjectContext};
use cloudstack_core::AppError;

use super::{discard_documents, save_documents, BatchSaveReport, DiscardReport, DraftStorage};

/// 单个排队操作的票据：完成回调必须带着它回来才能释放 `active`，防止重复
/// 或迟到的 completion 误清当前真正在跑的那一项、或者提前启动还没轮到的
/// 下一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftTicket(u64);

pub struct DraftTask {
    pub ticket: DraftTicket,
    pub operation: DraftOperation,
}

pub enum DraftAction {
    None,
    Execute(Box<DraftTask>),
}

pub enum DraftOperation {
    Write {
        storage: DraftStorage,
        context: ProjectContext,
        post_id: String,
        raw_frontmatter: Option<String>,
        body: String,
        base_revision: String,
    },
    Read {
        storage: DraftStorage,
        context: ProjectContext,
        document: PostDocument,
        epoch: u64,
    },
    Delete {
        storage: DraftStorage,
        context: ProjectContext,
        post_id: String,
    },
    SaveAndClose {
        storage: DraftStorage,
        context: ProjectContext,
        documents: Vec<PostDocument>,
    },
    SaveAll {
        storage: DraftStorage,
        context: ProjectContext,
        documents: Vec<PostDocument>,
    },
    DiscardAndClose {
        storage: DraftStorage,
        context: ProjectContext,
        documents: Vec<PostDocument>,
    },
}

pub enum DraftCompletion {
    Written {
        context_root: PathBuf,
        post_id: String,
        result: Result<(), AppError>,
    },
    Read {
        context: Box<ProjectContext>,
        document: PostDocument,
        epoch: u64,
        result: Result<Option<DraftDocument>, AppError>,
    },
    Deleted {
        context_root: PathBuf,
        post_id: String,
        result: Result<(), AppError>,
    },
    BatchSaved {
        report: BatchSaveReport,
        close_window: bool,
    },
    Discarded(DiscardReport),
}

impl DraftOperation {
    pub fn closes_window(&self) -> bool {
        matches!(
            self,
            Self::SaveAndClose { .. } | Self::DiscardAndClose { .. }
        )
    }

    pub fn execute(self) -> DraftCompletion {
        match self {
            Self::Write {
                storage,
                context,
                post_id,
                raw_frontmatter,
                body,
                base_revision,
            } => {
                let result =
                    storage.write(&context, &post_id, raw_frontmatter, body, base_revision);
                DraftCompletion::Written {
                    context_root: context.root,
                    post_id,
                    result,
                }
            }
            Self::Read {
                storage,
                context,
                document,
                epoch,
            } => {
                let result = storage.read(&context, &document.id);
                DraftCompletion::Read {
                    context: Box::new(context),
                    document,
                    epoch,
                    result,
                }
            }
            Self::Delete {
                storage,
                context,
                post_id,
            } => {
                let result = storage.delete(&context, &post_id);
                DraftCompletion::Deleted {
                    context_root: context.root,
                    post_id,
                    result,
                }
            }
            Self::SaveAndClose {
                storage,
                context,
                documents,
            } => DraftCompletion::BatchSaved {
                report: save_documents(&storage, &context, documents),
                close_window: true,
            },
            Self::SaveAll {
                storage,
                context,
                documents,
            } => DraftCompletion::BatchSaved {
                report: save_documents(&storage, &context, documents),
                close_window: false,
            },
            Self::DiscardAndClose {
                storage,
                context,
                documents,
            } => DraftCompletion::Discarded(discard_documents(&storage, &context, documents)),
        }
    }
}

/// FIFO + single-flight：任意时刻最多一个 operation 在飞，其余按到达顺序
/// 排队。草稿 operation 之间有严格的先后依赖（比如"写入"排在"批量保存"
/// 之前、"批量保存"完成后才该"删除"对应的恢复草稿），不能像
/// `PreviewCoordinator`/`LastDocumentWriter` 那样只保留最新值合并掉中间的。
#[derive(Default)]
pub struct DraftCoordinator {
    next_ticket: u64,
    active: Option<DraftTicket>,
    pending: VecDeque<DraftOperation>,
}

impl DraftCoordinator {
    pub fn enqueue(&mut self, operation: DraftOperation) -> DraftAction {
        self.pending.push_back(operation);
        self.start_next()
    }

    /// `stop_queue` 为 true 时（窗口正在关闭）清空剩余排队项，不再继续执行。
    /// 调用方必须先处理完 completion 的副作用（比如可能因此产生的新
    /// enqueue）、再调用这个方法：这样新 enqueue 的操作会先进 `pending`
    /// 排队，而不是在 `active` 还没被正式释放前就抢跑。
    pub fn complete(&mut self, ticket: DraftTicket, stop_queue: bool) -> DraftAction {
        if self.active != Some(ticket) {
            return DraftAction::None;
        }

        self.active = None;

        if stop_queue {
            self.pending.clear();
            DraftAction::None
        } else {
            self.start_next()
        }
    }

    fn start_next(&mut self) -> DraftAction {
        if self.active.is_some() {
            return DraftAction::None;
        }

        let Some(operation) = self.pending.pop_front() else {
            return DraftAction::None;
        };

        let ticket = DraftTicket(self.next_ticket);
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.active = Some(ticket);

        DraftAction::Execute(Box::new(DraftTask { ticket, operation }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_op(post_id: &str) -> DraftOperation {
        DraftOperation::Delete {
            storage: DraftStorage::new(PathBuf::from("/tmp/primary"), None),
            context: ProjectContext {
                root: PathBuf::from("/tmp/project"),
                content_root: PathBuf::from("/tmp/project"),
                config_path: PathBuf::from("/tmp/project/.cloudstack.json"),
                config: Default::default(),
            },
            post_id: post_id.to_owned(),
        }
    }

    fn ticket_of(action: &DraftAction) -> DraftTicket {
        match action {
            DraftAction::Execute(task) => task.ticket,
            DraftAction::None => panic!("expected DraftAction::Execute"),
        }
    }

    #[test]
    fn first_operation_starts_immediately() {
        let mut coordinator = DraftCoordinator::default();
        let action = coordinator.enqueue(write_op("a.md"));
        assert!(matches!(action, DraftAction::Execute(_)));
    }

    #[test]
    fn queued_operations_preserve_fifo_order() {
        let mut coordinator = DraftCoordinator::default();
        let first = coordinator.enqueue(write_op("a.md"));
        let first_ticket = ticket_of(&first);

        assert!(matches!(
            coordinator.enqueue(write_op("b.md")),
            DraftAction::None
        ));
        assert!(matches!(
            coordinator.enqueue(write_op("c.md")),
            DraftAction::None
        ));

        let next = coordinator.complete(first_ticket, false);
        let DraftAction::Execute(task) = next else {
            panic!("expected the queued b.md operation to start");
        };
        match task.operation {
            DraftOperation::Delete { post_id, .. } => assert_eq!(post_id, "b.md"),
            _ => panic!("unexpected operation"),
        }
    }

    #[test]
    fn mismatched_completion_does_not_release_active_operation() {
        let mut coordinator = DraftCoordinator::default();
        let first = coordinator.enqueue(write_op("a.md"));
        let first_ticket = ticket_of(&first);
        coordinator.enqueue(write_op("b.md"));

        // 一个不对应当前 active 的 ticket（比如迟到的重复回调）不能误清
        // active 状态，也不能提前把还没轮到的 b.md 启动。
        let stray = DraftTicket(first_ticket.0.wrapping_add(999));
        assert!(matches!(
            coordinator.complete(stray, false),
            DraftAction::None
        ));

        let next = coordinator.complete(first_ticket, false);
        assert!(matches!(next, DraftAction::Execute(_)));
    }

    #[test]
    fn stopping_after_completion_discards_remaining_queue() {
        let mut coordinator = DraftCoordinator::default();
        let first = coordinator.enqueue(write_op("a.md"));
        let first_ticket = ticket_of(&first);
        coordinator.enqueue(write_op("b.md"));
        coordinator.enqueue(write_op("c.md"));

        let next = coordinator.complete(first_ticket, true);
        assert!(matches!(next, DraftAction::None));

        // 队列已经被清空，即使再 enqueue 之前排队的操作也不会残留执行。
        let after_stop = coordinator.enqueue(write_op("d.md"));
        let DraftAction::Execute(task) = after_stop else {
            panic!("expected d.md to start immediately since the queue is idle");
        };
        match task.operation {
            DraftOperation::Delete { post_id, .. } => assert_eq!(post_id, "d.md"),
            _ => panic!("unexpected operation"),
        }
    }
}
