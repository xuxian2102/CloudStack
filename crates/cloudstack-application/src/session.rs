//! 一次编辑会话持有的核心业务状态：当前打开的项目、文章列表、当前文档、
//! 未保存快照、Git 快照、dirty/busy 和三个 generation 计数器。从
//! `cloudstack-gtk` 的 `EditorState` 机械迁移过来（第 9A 轮）。
//!
//! 字段目前都是 `pub`——这一轮只搬字段、不新增状态转换逻辑，GTK 仍然直接
//! 读写这些字段（现在经过 `.session.` 这一层）。真正的状态转换方法（打开/
//! 关闭 workspace、安装文档、标记 dirty、应用保存结果等）留给第 9B 轮，
//! 到时候这些字段会收进方法背后，不再对外公开。

use std::collections::HashMap;

use cloudstack_core::model::{PostDocument, PostSummary, ProjectContext, RepositorySnapshot};

/// Transitional field visibility for the 9A mechanical migration.
/// These fields remain public only until state transitions move behind
/// WorkspaceSession methods in 9B.
#[derive(Default)]
pub struct WorkspaceSession {
    pub project: Option<ProjectContext>,
    pub posts: Vec<PostSummary>,
    pub document: Option<PostDocument>,
    pub dirty: bool,
    pub busy: bool,
    pub document_epoch: u64,
    /// 每次 mark_document_dirty 自增一次，用来在保存完成时判断保存期间
    /// buffer 有没有被再次修改。
    pub edit_generation: u64,
    pub git_snapshot: Option<RepositorySnapshot>,
    /// 每次触发 Git 状态刷新时自增，防止后台线程池乱序完成时旧请求覆盖新状态。
    pub git_refresh_generation: u64,
    /// 当前会话中已修改但尚未写回磁盘的文章快照。允许切换文章时保留编辑内容。
    pub unsaved_documents: HashMap<String, PostDocument>,
}
