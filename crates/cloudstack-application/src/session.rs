//! 一次编辑会话持有的核心业务状态：当前打开的项目、文章列表、当前文档、
//! 未保存快照、Git 快照、dirty/busy 和三个 generation 计数器。从
//! `cloudstack-gtk` 的 `EditorState` 机械迁移过来（第 9A 轮）。
//!
//! 第 9A 轮只搬字段、不新增状态转换逻辑；第 9B 轮按"workspace 生命周期 →
//! document 生命周期 → save/dirty 生命周期"三个语义簇把状态转换方法加回
//! `WorkspaceSession`；第 9C 轮把剩下的 Git 刷新 request/completion
//! （[`WorkspaceSession::begin_git_refresh`]/[`WorkspaceSession::apply_git_snapshot`]）、
//! `busy`（[`WorkspaceSession::set_busy`]）和控件能力计算
//! （[`WorkspaceSession::capabilities`]）也收进方法，并把全部字段私有化，
//! 只留只读 getter 对外。GTK 现在不再直接读写任何字段。

use std::collections::HashMap;

use cloudstack_core::model::{PostDocument, PostSummary, ProjectContext, RepositorySnapshot};

use crate::controls::{capabilities_for, WorkspaceCapabilities, WorkspaceCapabilitiesInput};
use crate::git_refresh::should_apply_git_refresh;
use crate::save::{apply_successful_save, classify_save_completion, SaveCompletionOutcome};

#[derive(Default)]
pub struct WorkspaceSession {
    project: Option<ProjectContext>,
    posts: Vec<PostSummary>,
    document: Option<PostDocument>,
    dirty: bool,
    busy: bool,
    document_epoch: u64,
    /// 每次 mark_document_dirty 自增一次，用来在保存完成时判断保存期间
    /// buffer 有没有被再次修改。
    edit_generation: u64,
    git_snapshot: Option<RepositorySnapshot>,
    /// 每次触发 Git 状态刷新时自增，防止后台线程池乱序完成时旧请求覆盖新状态。
    git_refresh_generation: u64,
    /// 当前会话中已修改但尚未写回磁盘的文章快照。允许切换文章时保留编辑内容。
    unsaved_documents: HashMap<String, PostDocument>,
}

/// [`WorkspaceSession::install_workspace`] 的结果：GTK 只需要新的
/// `document_epoch` 就能让预览/异步请求跟这次打开的会话对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceInstalled {
    pub document_epoch: u64,
}

/// [`WorkspaceSession::close_workspace`] 的结果，字段含义同上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceClosed {
    pub document_epoch: u64,
}

/// [`WorkspaceSession::install_document`] 的结果，字段含义同上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentInstalled {
    pub document_epoch: u64,
}

/// [`WorkspaceSession::clear_document`] 的结果，字段含义同上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentCleared {
    pub document_epoch: u64,
}

/// [`WorkspaceSession::mark_document_dirty`] 的结果：GTK 拿 `post_id` 去更新
/// 侧栏未保存标记、拿 `edit_generation`（如果需要）判断这次编辑相对哪个
/// generation。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentDirty {
    pub post_id: String,
    pub edit_generation: u64,
}

/// [`WorkspaceSession::begin_git_refresh`] 的结果：GTK 拿 `context` 去后台
/// 执行 `git::snapshot`，完成时把这份 request 原样传回
/// [`WorkspaceSession::apply_git_snapshot`]/[`WorkspaceSession::is_git_refresh_current`]
/// 校验它是不是还对应当前项目会话。
#[derive(Debug, Clone)]
pub struct GitRefreshRequest {
    pub context: ProjectContext,
    pub generation: u64,
}

impl WorkspaceSession {
    /// 打开一个新 workspace：安装 `project`/`posts`，清空上一个 workspace
    /// 遗留的当前文档、dirty、Git 快照和未保存快照表，并推进
    /// `document_epoch`（让任何还在飞行的、绑定旧 epoch 的异步请求/预览在
    /// 完成时被拒绝应用）。不触碰 `edit_generation`/`git_refresh_generation`——
    /// 两者分别只在编辑和 Git 刷新时才有意义，不属于"打开 workspace"这个
    /// 转换。
    pub fn install_workspace(
        &mut self,
        context: ProjectContext,
        posts: Vec<PostSummary>,
    ) -> WorkspaceInstalled {
        self.project = Some(context);
        self.posts = posts;
        self.document = None;
        self.dirty = false;
        self.git_snapshot = None;
        self.unsaved_documents.clear();
        self.document_epoch = self.document_epoch.wrapping_add(1);
        WorkspaceInstalled {
            document_epoch: self.document_epoch,
        }
    }

    /// 关闭当前 workspace，回到没有项目打开的状态。调用方必须先确认没有
    /// 未保存文章（这个前置条件由 GTK 的 `ensure_no_unsaved_documents` 负责，
    /// 不属于这个方法的职责）；`unsaved_documents.clear()` 仍然执行一遍，
    /// 只是防御性的——正常情况下这个表此时已经是空的。
    pub fn close_workspace(&mut self) -> WorkspaceClosed {
        self.project = None;
        self.posts.clear();
        self.document = None;
        self.dirty = false;
        self.git_snapshot = None;
        self.unsaved_documents.clear();
        self.document_epoch = self.document_epoch.wrapping_add(1);
        WorkspaceClosed {
            document_epoch: self.document_epoch,
        }
    }

    /// 用新扫描到的文章列表整体替换 `posts`，不改变 `document`/`dirty`/
    /// `document_epoch` 或其他任何字段——创建/重命名文章后刷新列表就是这个
    /// 语义。删除当前正打开的文章需要额外清空 `document`/`dirty` 并推进
    /// `document_epoch`，那是 [`WorkspaceSession::clear_document`] 的职责，
    /// 不在这个方法里做。
    pub fn replace_posts(&mut self, posts: Vec<PostSummary>) {
        self.posts = posts;
    }

    /// 一次 Git 操作（比如发布）可能会返回更新过的 `ProjectContext`（例如
    /// exclude 配置被顺带写入）。只有这份 context 仍然对应当前打开的项目
    /// （root 匹配）才安装，返回是否真的替换了——异步操作飞行期间项目可能
    /// 已经被切换或关闭，那种情况下这份 context 已经过期，不该覆盖。不改变
    /// `posts`/`document`/`dirty` 等其他字段。
    pub fn replace_project_context(&mut self, context: ProjectContext) -> bool {
        let matches = self
            .project
            .as_ref()
            .is_some_and(|current| current.root == context.root);
        if matches {
            self.project = Some(context);
        }
        matches
    }

    /// 切换到（或首次安装）一篇文档：`dirty` 总是显式取调用方传入的值，
    /// 不会跟上一篇文档的 dirty 状态混合——调用方决定这是"从磁盘/新建/
    /// 重命名后干净加载"（`dirty = false`）还是"恢复一份未保存快照"
    /// （`dirty = true`）。不触碰 `unsaved_documents`：其他文章的未保存快照
    /// 本来就一直留在表里（由 `mark_document_dirty` 持续写入，不是切换文档
    /// 时才快照一次），切换文档天然不需要主动"保留"它们，只是不去碰。
    pub fn install_document(&mut self, document: PostDocument, dirty: bool) -> DocumentInstalled {
        self.document = Some(document);
        self.dirty = dirty;
        self.document_epoch = self.document_epoch.wrapping_add(1);
        DocumentInstalled {
            document_epoch: self.document_epoch,
        }
    }

    /// 清空当前打开的文档（比如删除的正是当前文章），回到"项目已打开、
    /// 没有文档被选中"的状态，并推进 `document_epoch`。不改变 `posts`/
    /// `project`/`unsaved_documents`——删除文章时这些字段分别由
    /// [`WorkspaceSession::replace_posts`] 和
    /// [`WorkspaceSession::remove_unsaved_document`] 处理。
    pub fn clear_document(&mut self) -> DocumentCleared {
        self.document = None;
        self.dirty = false;
        self.document_epoch = self.document_epoch.wrapping_add(1);
        DocumentCleared {
            document_epoch: self.document_epoch,
        }
    }

    /// 从未保存快照表里移除一个特定 id 的条目。删除/重命名文章时用来清掉
    /// 旧 id 可能残留的未保存快照；GTK 当前的删除/重命名入口在有任何未保存
    /// 文章时就整体拒绝操作（`ensure_no_unsaved_documents`/`has_unsaved`），
    /// 所以实际调用时这个 id 通常已经不在表里——这里仍然显式调用，把"删除/
    /// 重命名不残留旧 id"的不变量写成代码，而不是隐式依赖调用方的守卫逻辑。
    ///
    /// 命名故意是 `remove_unsaved_document` 而不是 `remove_document`——同一个
    /// 类型里已经有 `install_document`/`clear_document`，`remove_document`
    /// 容易被误读成"删除当前文档"（那是 [`WorkspaceSession::clear_document`]
    /// 的职责），实际上这里只操作 `unsaved_documents` 这张表。
    pub fn remove_unsaved_document(&mut self, post_id: &str) {
        self.unsaved_documents.remove(post_id);
    }

    /// 当前文档的正文被编辑：`dirty = true`，`edit_generation` 推进，把最新
    /// 正文快照写入 `unsaved_documents`（覆盖同 id 的旧快照）。没有当前文档
    /// 时返回 `None`，不做任何改动——对应原 GTK `mark_document_dirty()` 开头
    /// 的提前返回。
    pub fn mark_document_dirty(&mut self, body: String) -> Option<DocumentDirty> {
        let mut snapshot = self.document.clone()?;
        self.dirty = true;
        self.edit_generation = self.edit_generation.wrapping_add(1);
        snapshot.body = body;
        let post_id = snapshot.id.clone();
        self.unsaved_documents.insert(post_id.clone(), snapshot);
        Some(DocumentDirty {
            post_id,
            edit_generation: self.edit_generation,
        })
    }

    /// 修改当前文档的 frontmatter 原始文本。只改 `raw_frontmatter` 这一个
    /// 字段，不推进 `edit_generation`、不写 `unsaved_documents`——frontmatter
    /// 变更后随即要读取最新 buffer 正文一起调用
    /// [`WorkspaceSession::mark_document_dirty`]，两者一起才构成一次完整的
    /// "未保存快照同时包含新 frontmatter 和新正文"。没有当前文档时返回
    /// `false` 并且不做任何改动。
    pub fn set_current_frontmatter(&mut self, raw_frontmatter: Option<String>) -> bool {
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        document.raw_frontmatter = raw_frontmatter;
        true
    }

    /// 单篇保存完成：复用 [`crate::save::classify_save_completion`]/
    /// [`crate::save::apply_successful_save`]（不复制它们的判定逻辑），按
    /// 分类结果落地 `document`/`unsaved_documents`/`dirty`。`NotCurrent` 时
    /// 什么都不碰；`Clean` 时清空 dirty、移除 unsaved 条目；`RevisionOnly`
    /// （保存进行期间又发生了编辑）时只同步 revision，dirty 和 unsaved
    /// 快照原样保留——这就是"旧保存 completion 不能清掉新编辑产生的 dirty"
    /// 这条不变量的落地位置。不碰 `pending_assets`：那是会触碰磁盘的副作用，
    /// 留给 GTK 在 `Clean` 分支里处理。
    pub fn apply_saved_document(
        &mut self,
        saved: PostDocument,
        saved_document_epoch: u64,
        saved_generation: u64,
    ) -> SaveCompletionOutcome {
        let outcome = classify_save_completion(
            self.document.as_ref().map(|document| document.id.as_str()),
            &saved.id,
            self.document_epoch,
            saved_document_epoch,
            self.edit_generation,
            saved_generation,
        );
        apply_successful_save(
            outcome,
            &mut self.document,
            &mut self.unsaved_documents,
            &mut self.dirty,
            &saved.id,
            &saved.revision,
            saved.raw_frontmatter,
            saved.body,
        );
        outcome
    }

    /// 批量保存完成：每篇成功保存的文档都从 `unsaved_documents` 移除；如果
    /// 恰好是当前打开的文档，`document` 换成保存后的新副本（同步 revision/
    /// body/frontmatter）。最后按"当前文档 id 是否仍在 unsaved_documents
    /// 里"重新计算 `dirty`——部分失败的批量保存只清掉成功项，失败项的 id
    /// 原样留在表里，如果失败的正是当前文档，`dirty` 会保持 true。
    pub fn apply_batch_saved(&mut self, saved: &[PostDocument]) {
        for document in saved {
            self.unsaved_documents.remove(&document.id);
            if self
                .document
                .as_ref()
                .is_some_and(|current| current.id == document.id)
            {
                self.document = Some(document.clone());
            }
        }
        self.recompute_dirty_from_unsaved();
    }

    /// 批量丢弃完成：移除给定的 id，重新计算 `dirty`。跟
    /// [`WorkspaceSession::apply_batch_saved`] 共享同一条"dirty 取决于当前
    /// 文档 id 是否还在 unsaved_documents 里"的规则，但丢弃不替换
    /// `document` 本身（丢弃的是未保存快照，不是磁盘上的新内容）。
    pub fn discard_unsaved_documents(&mut self, discarded_ids: &[String]) {
        for post_id in discarded_ids {
            self.unsaved_documents.remove(post_id);
        }
        self.recompute_dirty_from_unsaved();
    }

    fn recompute_dirty_from_unsaved(&mut self) {
        let current_id = self.document.as_ref().map(|document| document.id.clone());
        self.dirty = current_id
            .as_deref()
            .is_some_and(|post_id| self.unsaved_documents.contains_key(post_id));
    }

    /// 写入 `busy`。状态本身只是一个布尔字段；根据 busy 决定展示哪条状态栏
    /// 文案（读取 `document`/`dirty`/`project`/`posts` 来选择本地化文案）
    /// 是 presentation 决策，留在 GTK 的 `set_busy()` 里，只是改成通过这里
    /// 的只读 getter 读取状态，不再直接碰字段。
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// 从当前状态一次性算出所有控件的可用性，取代 GTK 手工拼
    /// `WorkspaceCapabilitiesInput` 再调用 `capabilities_for()`。
    pub fn capabilities(&self) -> WorkspaceCapabilities {
        capabilities_for(WorkspaceCapabilitiesInput {
            has_project: self.project.is_some(),
            has_document: self.document.is_some(),
            unsaved_document_count: self.unsaved_documents.len(),
            busy: self.busy,
            dirty: self.dirty,
            git_snapshot: self.git_snapshot.as_ref(),
        })
    }

    /// 发起一次 Git 状态刷新：没有打开的项目时清空 `git_snapshot`（不需要
    /// 刷新，也不该留着上一个项目的快照）并返回 `None`；否则推进
    /// `git_refresh_generation`，返回携带当前项目 `context` 和新 generation
    /// 的 request，调用方拿 `context` 去后台执行 `git::snapshot`。
    pub fn begin_git_refresh(&mut self) -> Option<GitRefreshRequest> {
        let Some(context) = self.project.clone() else {
            self.git_snapshot = None;
            return None;
        };
        self.git_refresh_generation = self.git_refresh_generation.wrapping_add(1);
        Some(GitRefreshRequest {
            context,
            generation: self.git_refresh_generation,
        })
    }

    /// 一次 Git 刷新 completion 是否仍然对应当前项目会话——后台线程池不
    /// 保证完成顺序，同一项目连续触发两次刷新时，先发出的请求可能后完成；
    /// 项目也可能在请求飞行期间被切换或关闭。校验 root 和 generation 双重
    /// 匹配，复用 [`crate::git_refresh::should_apply_git_refresh`]。
    pub fn is_git_refresh_current(&self, request: &GitRefreshRequest) -> bool {
        should_apply_git_refresh(
            self.project.as_ref().map(|context| context.root.as_path()),
            &request.context.root,
            self.git_refresh_generation,
            request.generation,
        )
    }

    /// Git 刷新成功完成时调用：只有请求仍然是当前会话的才安装
    /// `git_snapshot`，返回是否真的安装了（GTK 据此决定要不要把结果渲染
    /// 进面板）；已经过期的请求什么都不做。
    pub fn apply_git_snapshot(
        &mut self,
        request: &GitRefreshRequest,
        snapshot: RepositorySnapshot,
    ) -> bool {
        if !self.is_git_refresh_current(request) {
            return false;
        }
        self.git_snapshot = Some(snapshot);
        true
    }

    pub fn project(&self) -> Option<&ProjectContext> {
        self.project.as_ref()
    }

    pub fn posts(&self) -> &[PostSummary] {
        &self.posts
    }

    pub fn document(&self) -> Option<&PostDocument> {
        self.document.as_ref()
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn document_epoch(&self) -> u64 {
        self.document_epoch
    }

    pub fn edit_generation(&self) -> u64 {
        self.edit_generation
    }

    pub fn git_snapshot(&self) -> Option<&RepositorySnapshot> {
        self.git_snapshot.as_ref()
    }

    pub fn unsaved_document_count(&self) -> usize {
        self.unsaved_documents.len()
    }

    pub fn has_unsaved_documents(&self) -> bool {
        !self.unsaved_documents.is_empty()
    }

    pub fn unsaved_document(&self, post_id: &str) -> Option<&PostDocument> {
        self.unsaved_documents.get(post_id)
    }

    pub fn unsaved_documents(&self) -> impl Iterator<Item = &PostDocument> {
        self.unsaved_documents.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(root: &str) -> ProjectContext {
        ProjectContext {
            root: root.into(),
            content_root: format!("{root}/content").into(),
            config_path: format!("{root}/.cloudstack.json").into(),
            config: Default::default(),
        }
    }

    fn post(id: &str) -> PostSummary {
        PostSummary {
            id: id.to_string(),
            relative_path: id.to_string(),
            modified_ms: None,
        }
    }

    fn document(id: &str) -> PostDocument {
        PostDocument {
            id: id.to_string(),
            relative_path: id.to_string(),
            raw_frontmatter: None,
            body: String::new(),
            revision: "rev".into(),
        }
    }

    fn dirty_session_with_leftovers() -> WorkspaceSession {
        let mut session = WorkspaceSession {
            project: Some(context("/tmp/old")),
            posts: vec![post("old.md")],
            document: Some(document("old.md")),
            dirty: true,
            busy: false,
            document_epoch: 5,
            edit_generation: 9,
            git_snapshot: None,
            git_refresh_generation: 3,
            unsaved_documents: HashMap::new(),
        };
        session
            .unsaved_documents
            .insert("other.md".into(), document("other.md"));
        session
    }

    #[test]
    fn install_workspace_clears_previous_document_git_snapshot_and_unsaved_map() {
        let mut session = dirty_session_with_leftovers();

        let new_context = context("/tmp/new");
        let new_posts = vec![post("a.md"), post("b.md")];
        session.install_workspace(new_context.clone(), new_posts.clone());

        assert_eq!(
            session.project.as_ref().map(|c| &c.root),
            Some(&new_context.root)
        );
        assert_eq!(post_ids(&session.posts), post_ids(&new_posts));
        assert!(session.document.is_none());
        assert!(!session.dirty);
        assert!(session.git_snapshot.is_none());
        assert!(session.unsaved_documents.is_empty());
        // 打开新 workspace 不是一次编辑或 Git 刷新，这两个 generation 保持原样。
        assert_eq!(session.edit_generation, 9);
        assert_eq!(session.git_refresh_generation, 3);
    }

    #[test]
    fn install_workspace_advances_document_epoch() {
        let mut session = dirty_session_with_leftovers();
        let starting_epoch = session.document_epoch;

        let installed = session.install_workspace(context("/tmp/new"), Vec::new());

        assert_eq!(installed.document_epoch, starting_epoch.wrapping_add(1));
        assert_eq!(session.document_epoch, installed.document_epoch);

        let installed_again = session.install_workspace(context("/tmp/another"), Vec::new());
        assert_eq!(
            installed_again.document_epoch,
            installed.document_epoch.wrapping_add(1)
        );
    }

    #[test]
    fn close_workspace_clears_project_state_and_advances_epoch() {
        let mut session = dirty_session_with_leftovers();
        let starting_epoch = session.document_epoch;

        let closed = session.close_workspace();

        assert!(session.project.is_none());
        assert!(session.posts.is_empty());
        assert!(session.document.is_none());
        assert!(!session.dirty);
        assert!(session.git_snapshot.is_none());
        assert!(session.unsaved_documents.is_empty());
        assert_eq!(closed.document_epoch, starting_epoch.wrapping_add(1));
        assert_eq!(session.document_epoch, closed.document_epoch);
    }

    #[test]
    fn close_workspace_when_empty_stays_empty_and_advances_epoch() {
        let mut session = WorkspaceSession::default();
        let closed = session.close_workspace();
        assert_eq!(closed.document_epoch, 1);
        assert!(session.project.is_none());
    }

    #[test]
    fn replace_posts_only_replaces_the_list() {
        let mut session = dirty_session_with_leftovers();
        let starting_epoch = session.document_epoch;
        let starting_document = session.document.clone();

        session.replace_posts(vec![post("new.md")]);

        assert_eq!(post_ids(&session.posts), vec!["new.md"]);
        assert_eq!(
            session.document.as_ref().map(|d| &d.id),
            starting_document.as_ref().map(|d| &d.id)
        );
        assert!(session.dirty);
        assert_eq!(session.document_epoch, starting_epoch);
    }

    #[test]
    fn replace_project_context_replaces_when_root_matches() {
        let mut session = clean_session_with_document();
        let starting_document_id = session.document.as_ref().map(|d| d.id.clone());
        let starting_dirty = session.dirty;
        let starting_epoch = session.document_epoch;
        let starting_unsaved_count = session.unsaved_documents.len();

        let mut updated = context("/tmp/project");
        updated.config_path = "/tmp/project/.blog-editor.json".into();
        let replaced = session.replace_project_context(updated.clone());

        assert!(replaced);
        assert_eq!(
            session.project.as_ref().map(|c| c.config_path.clone()),
            Some(updated.config_path)
        );
        // 同一个 workspace 的 context 被重新写入配置后更新，不是打开新
        // workspace，不该碰其他任何字段。
        assert_eq!(
            session.document.as_ref().map(|d| d.id.clone()),
            starting_document_id
        );
        assert_eq!(session.dirty, starting_dirty);
        assert_eq!(session.document_epoch, starting_epoch);
        assert_eq!(session.unsaved_documents.len(), starting_unsaved_count);
    }

    #[test]
    fn replace_project_context_rejects_a_different_root() {
        let mut session = clean_session_with_document();
        let other = context("/tmp/other");

        let replaced = session.replace_project_context(other);

        assert!(!replaced);
        assert_eq!(
            session.project.as_ref().map(|c| c.root.clone()),
            Some("/tmp/project".into())
        );
    }

    #[test]
    fn replace_project_context_rejects_without_a_project() {
        let mut session = WorkspaceSession::default();

        let replaced = session.replace_project_context(context("/tmp/project"));

        assert!(!replaced);
        assert!(session.project.is_none());
    }

    #[test]
    fn install_document_replaces_current_document_and_advances_epoch() {
        let mut session = dirty_session_with_leftovers();
        let starting_epoch = session.document_epoch;

        let installed = session.install_document(document("new.md"), false);

        assert_eq!(
            session.document.as_ref().map(|d| d.id.as_str()),
            Some("new.md")
        );
        assert_eq!(installed.document_epoch, starting_epoch.wrapping_add(1));
        assert_eq!(session.document_epoch, installed.document_epoch);
    }

    #[test]
    fn install_document_does_not_inherit_previous_dirty_state() {
        // 上一篇文档是 dirty 的；切到一篇干净加载的新文档，dirty 必须显式变
        // false，不能因为"没显式清空"就继承上一篇的 true。
        let mut session = dirty_session_with_leftovers();
        assert!(session.dirty);
        session.install_document(document("clean.md"), false);
        assert!(!session.dirty);

        // 反过来：上一篇是干净的，恢复一份未保存快照（dirty = true），也必须
        // 显式变 true，不能因为"上一篇是 false"就保持 false。
        let mut clean_session = WorkspaceSession {
            dirty: false,
            ..dirty_session_with_leftovers()
        };
        clean_session.install_document(document("restored.md"), true);
        assert!(clean_session.dirty);
    }

    #[test]
    fn install_document_does_not_touch_posts_or_other_unsaved_snapshots() {
        // 切换文档不应该清空 unsaved_documents——其他文章的未保存快照是靠
        // mark_document_dirty 持续写入的，不是靠这个方法主动"保留"，所以这里
        // 只需要证明它完全不碰这张表和 posts。
        let mut session = dirty_session_with_leftovers();
        let starting_posts_owned = session.posts.clone();
        let starting_posts = post_ids(&starting_posts_owned);

        session.install_document(document("new.md"), false);

        assert_eq!(post_ids(&session.posts), starting_posts);
        assert!(session.unsaved_documents.contains_key("other.md"));
    }

    #[test]
    fn clear_document_resets_current_document_and_advances_epoch() {
        let mut session = dirty_session_with_leftovers();
        let starting_epoch = session.document_epoch;

        let cleared = session.clear_document();

        assert!(session.document.is_none());
        assert!(!session.dirty);
        assert_eq!(cleared.document_epoch, starting_epoch.wrapping_add(1));
        assert_eq!(session.document_epoch, cleared.document_epoch);
    }

    #[test]
    fn clear_document_does_not_touch_posts_project_or_unsaved_documents() {
        let mut session = dirty_session_with_leftovers();
        let starting_posts_owned = session.posts.clone();
        let starting_posts = post_ids(&starting_posts_owned);
        let starting_root = session.project.as_ref().map(|c| c.root.clone());

        session.clear_document();

        assert_eq!(post_ids(&session.posts), starting_posts);
        assert_eq!(
            session.project.as_ref().map(|c| &c.root),
            starting_root.as_ref()
        );
        assert!(session.unsaved_documents.contains_key("other.md"));
    }

    #[test]
    fn remove_unsaved_document_removes_only_the_given_entry() {
        let mut session = dirty_session_with_leftovers();
        session
            .unsaved_documents
            .insert("keep.md".into(), document("keep.md"));

        session.remove_unsaved_document("other.md");

        assert!(!session.unsaved_documents.contains_key("other.md"));
        assert!(session.unsaved_documents.contains_key("keep.md"));
    }

    #[test]
    fn remove_unsaved_document_is_a_noop_when_absent() {
        let mut session = WorkspaceSession::default();
        session.remove_unsaved_document("missing.md");
        assert!(session.unsaved_documents.is_empty());
    }

    fn clean_session_with_document() -> WorkspaceSession {
        WorkspaceSession {
            project: Some(context("/tmp/project")),
            posts: vec![post("a.md")],
            document: Some(document("a.md")),
            dirty: false,
            busy: false,
            document_epoch: 5,
            edit_generation: 9,
            git_snapshot: None,
            git_refresh_generation: 3,
            unsaved_documents: HashMap::new(),
        }
    }

    #[test]
    fn mark_document_dirty_advances_generation_and_snapshots_body() {
        let mut session = clean_session_with_document();
        let starting_generation = session.edit_generation;

        let dirty = session
            .mark_document_dirty("new body".into())
            .expect("current document exists");

        assert_eq!(dirty.post_id, "a.md");
        assert_eq!(dirty.edit_generation, starting_generation.wrapping_add(1));
        assert!(session.dirty);
        assert_eq!(session.edit_generation, dirty.edit_generation);
        assert_eq!(
            session
                .unsaved_documents
                .get("a.md")
                .map(|d| d.body.as_str()),
            Some("new body")
        );
    }

    #[test]
    fn mark_document_dirty_returns_none_without_a_current_document() {
        let mut session = WorkspaceSession::default();
        assert!(session.mark_document_dirty("body".into()).is_none());
        assert!(!session.dirty);
        assert_eq!(session.edit_generation, 0);
        assert!(session.unsaved_documents.is_empty());
    }

    #[test]
    fn mark_document_dirty_overwrites_snapshot_for_the_same_id() {
        let mut session = clean_session_with_document();

        session.mark_document_dirty("first".into());
        let after_first = session.edit_generation;
        session.mark_document_dirty("second".into());

        assert_eq!(session.edit_generation, after_first.wrapping_add(1));
        assert_eq!(session.unsaved_documents.len(), 1);
        assert_eq!(
            session
                .unsaved_documents
                .get("a.md")
                .map(|d| d.body.as_str()),
            Some("second")
        );
    }

    #[test]
    fn set_current_frontmatter_replaces_raw_frontmatter_without_touching_generation_or_unsaved() {
        let mut session = clean_session_with_document();
        let starting_generation = session.edit_generation;

        let changed = session.set_current_frontmatter(Some("title: new".into()));

        assert!(changed);
        assert_eq!(
            session
                .document
                .as_ref()
                .and_then(|d| d.raw_frontmatter.as_deref()),
            Some("title: new")
        );
        assert_eq!(session.edit_generation, starting_generation);
        assert!(!session.dirty);
        assert!(session.unsaved_documents.is_empty());
    }

    #[test]
    fn set_current_frontmatter_then_mark_document_dirty_snapshots_both() {
        let mut session = clean_session_with_document();

        session.set_current_frontmatter(Some("title: new".into()));
        session.mark_document_dirty("new body".into());

        let snapshot = session
            .unsaved_documents
            .get("a.md")
            .expect("snapshot recorded");
        assert_eq!(snapshot.raw_frontmatter.as_deref(), Some("title: new"));
        assert_eq!(snapshot.body, "new body");
    }

    #[test]
    fn apply_saved_document_clean_clears_dirty_and_removes_unsaved_entry() {
        let mut session = clean_session_with_document();
        session.dirty = true;
        session
            .unsaved_documents
            .insert("a.md".into(), document("a.md"));
        let epoch = session.document_epoch;
        let generation = session.edit_generation;

        let mut saved = document("a.md");
        saved.body = "saved body".into();
        saved.revision = "rev-2".into();
        let outcome = session.apply_saved_document(saved, epoch, generation);

        assert_eq!(outcome, SaveCompletionOutcome::Clean);
        assert!(!session.dirty);
        assert!(!session.unsaved_documents.contains_key("a.md"));
        assert_eq!(
            session.document.as_ref().map(|d| d.revision.as_str()),
            Some("rev-2")
        );
    }

    #[test]
    fn apply_saved_document_revision_only_keeps_dirty_when_generation_advanced() {
        let mut session = clean_session_with_document();
        session.dirty = true;
        session
            .unsaved_documents
            .insert("a.md".into(), document("a.md"));
        let epoch = session.document_epoch;
        let saved_generation = session.edit_generation;
        // 保存派发之后、完成之前又发生了一次编辑。
        session.edit_generation = session.edit_generation.wrapping_add(1);

        let mut saved = document("a.md");
        saved.revision = "rev-2".into();
        let outcome = session.apply_saved_document(saved, epoch, saved_generation);

        assert_eq!(outcome, SaveCompletionOutcome::RevisionOnly);
        assert!(session.dirty, "保存期间又编辑，不能清掉 dirty");
        assert!(session.unsaved_documents.contains_key("a.md"));
        assert_eq!(
            session.document.as_ref().map(|d| d.revision.as_str()),
            Some("rev-2")
        );
    }

    #[test]
    fn apply_saved_document_not_current_when_epoch_advanced_does_not_overwrite_document() {
        let mut session = clean_session_with_document();
        let saved_epoch = session.document_epoch;
        let saved_generation = session.edit_generation;
        // 保存派发之后、完成之前用户切换到了另一篇文档（epoch 推进）。
        session.install_document(document("b.md"), false);

        let mut saved = document("a.md");
        saved.revision = "rev-2".into();
        let outcome = session.apply_saved_document(saved, saved_epoch, saved_generation);

        assert_eq!(outcome, SaveCompletionOutcome::NotCurrent);
        assert_eq!(
            session.document.as_ref().map(|d| d.id.as_str()),
            Some("b.md")
        );
    }

    #[test]
    fn apply_batch_saved_updates_current_document_revision_and_body() {
        let mut session = clean_session_with_document();
        session.dirty = true;
        session
            .unsaved_documents
            .insert("a.md".into(), document("a.md"));

        let mut saved = document("a.md");
        saved.body = "saved body".into();
        saved.revision = "rev-2".into();
        session.apply_batch_saved(&[saved]);

        assert_eq!(
            session.document.as_ref().map(|d| d.revision.as_str()),
            Some("rev-2")
        );
        assert_eq!(
            session.document.as_ref().map(|d| d.body.as_str()),
            Some("saved body")
        );
        assert!(!session.unsaved_documents.contains_key("a.md"));
        assert!(!session.dirty);
    }

    #[test]
    fn apply_batch_saved_partial_failure_only_clears_successful_entries() {
        let mut session = clean_session_with_document(); // 当前文档是 a.md
        session.dirty = true;
        session
            .unsaved_documents
            .insert("a.md".into(), document("a.md"));
        session
            .unsaved_documents
            .insert("b.md".into(), document("b.md"));

        // 只有 b.md 保存成功；a.md（当前文档）保存失败，不出现在 saved 里。
        session.apply_batch_saved(&[document("b.md")]);

        assert!(
            session.unsaved_documents.contains_key("a.md"),
            "失败项不应该被清掉"
        );
        assert!(!session.unsaved_documents.contains_key("b.md"));
        assert!(session.dirty, "当前文档保存失败，dirty 必须保留");
    }

    #[test]
    fn discard_unsaved_documents_recomputes_current_dirty() {
        let mut session = clean_session_with_document(); // 当前文档是 a.md
        session.dirty = true;
        session
            .unsaved_documents
            .insert("a.md".into(), document("a.md"));
        session
            .unsaved_documents
            .insert("b.md".into(), document("b.md"));

        session.discard_unsaved_documents(&["b.md".to_string()]);
        assert!(session.dirty, "丢弃的不是当前文档，dirty 不应该被影响");
        assert!(session.unsaved_documents.contains_key("a.md"));

        session.discard_unsaved_documents(&["a.md".to_string()]);
        assert!(
            !session.dirty,
            "当前文档的未保存快照被丢弃后 dirty 必须清空"
        );
    }

    fn post_ids(posts: &[PostSummary]) -> Vec<&str> {
        posts.iter().map(|post| post.id.as_str()).collect()
    }

    fn snapshot() -> cloudstack_core::model::RepositorySnapshot {
        use cloudstack_core::model::{
            GitEnvironment, GitStatus, RepositoryTopology, SyncRelation, WorktreeState,
        };
        cloudstack_core::model::RepositorySnapshot {
            environment: GitEnvironment::default(),
            identity: None,
            topology: RepositoryTopology::NotInitialized,
            sync: SyncRelation::Unknown,
            worktree: WorktreeState::default(),
            remotes: Vec::new(),
            config_tracked: false,
            status: GitStatus {
                branch: None,
                upstream: None,
                ahead: 0,
                behind: 0,
                changes: Vec::new(),
            },
        }
    }

    #[test]
    fn set_busy_updates_the_busy_flag() {
        let mut session = WorkspaceSession::default();
        assert!(!session.busy());
        session.set_busy(true);
        assert!(session.busy());
        session.set_busy(false);
        assert!(!session.busy());
    }

    #[test]
    fn capabilities_reflects_current_state() {
        let mut session = clean_session_with_document();
        // 干净、不忙、有当前文档：保存按钮应该禁用（没有改动可保存）。
        assert!(!session.capabilities().save_enabled);

        session.mark_document_dirty("edited".into());
        assert!(
            session.capabilities().save_enabled,
            "dirty 之后 capabilities() 必须反映最新状态"
        );

        session.set_busy(true);
        assert!(
            !session.capabilities().save_enabled,
            "忙碌时即使 dirty 也不能保存"
        );
    }

    #[test]
    fn begin_git_refresh_returns_none_and_clears_snapshot_without_a_project() {
        let mut session = WorkspaceSession {
            git_snapshot: Some(snapshot()),
            ..Default::default()
        };

        assert!(session.begin_git_refresh().is_none());
        assert!(session.git_snapshot().is_none());
    }

    #[test]
    fn begin_git_refresh_advances_generation_and_captures_current_context() {
        let mut session = clean_session_with_document();
        let starting_generation = session.git_refresh_generation;

        let request = session
            .begin_git_refresh()
            .expect("project is open, refresh should start");

        assert_eq!(request.generation, starting_generation.wrapping_add(1));
        assert_eq!(session.git_refresh_generation, request.generation);
        assert_eq!(
            request.context.root,
            session.project().unwrap().root.clone()
        );
    }

    #[test]
    fn apply_git_snapshot_installs_when_request_is_current() {
        let mut session = clean_session_with_document();
        let request = session.begin_git_refresh().unwrap();

        let applied = session.apply_git_snapshot(&request, snapshot());

        assert!(applied);
        assert!(session.git_snapshot().is_some());
    }

    #[test]
    fn apply_git_snapshot_rejects_a_stale_generation() {
        let mut session = clean_session_with_document();
        let stale_request = session.begin_git_refresh().unwrap();
        // 同一个项目又触发了一次刷新，generation 已经推进。
        session.begin_git_refresh().unwrap();

        let applied = session.apply_git_snapshot(&stale_request, snapshot());

        assert!(!applied, "过期 generation 的刷新结果不能被安装");
        assert!(session.git_snapshot().is_none());
    }

    #[test]
    fn apply_git_snapshot_rejects_after_project_switched() {
        let mut session = clean_session_with_document();
        let request = session.begin_git_refresh().unwrap();
        // 刷新还没完成，用户已经切换到了另一个项目。
        session.install_workspace(context("/tmp/another"), Vec::new());

        let applied = session.apply_git_snapshot(&request, snapshot());

        assert!(!applied, "项目已经切换，旧项目的刷新结果不能被安装");
        assert!(session.git_snapshot().is_none());
    }
}
