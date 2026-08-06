//! 草稿存储、批量保存/丢弃 use case、FIFO 执行队列、恢复资格判定：从
//! `cloudstack-gtk` 的 `window/drafts.rs` 拆出来。
//!
//! GTK 仍然负责发现 primary/legacy 目录路径（依赖 `glib::user_data_dir()`
//! 和 `e2e` 环境变量）、派发 `tasks::run`、维护 700ms 定时器、渲染恢复对话框
//! 和响应处理。

mod batch;
mod coordinator;
mod recovery;
mod storage;

pub use batch::{
    discard_documents, save_documents, BatchFailure, BatchSaveReport, DiscardReport,
    DraftCleanupWarning,
};
pub use coordinator::{
    DraftAction, DraftCompletion, DraftCoordinator, DraftOperation, DraftTask, DraftTicket,
};
pub use recovery::{
    can_offer_recovery, classify_recovery, is_current_draft_target, CurrentDraftTargetInput,
    DraftRecoveryDecision, DraftRecoveryEligibilityInput,
};
pub use storage::DraftStorage;
