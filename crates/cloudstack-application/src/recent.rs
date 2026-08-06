//! "最近项目/文章"相关的纯选择规则和写入协调，不碰文件、不发后台任务。
//! `cloudstack-gtk` 负责实际读写 `recent.json`/`settings.json`、派发
//! `tasks::run`、维护 `thread_local!` 运行时实例；这里只回答"该恢复哪一个"
//! 和"下一次该写哪一个"。

use std::path::{Path, PathBuf};

use cloudstack_core::model::PostSummary;
use cloudstack_core::services::recent::RecentProject;

/// 冷启动是否应该自动重开上一个项目。
#[derive(Debug, Clone, Copy)]
pub struct ProjectReopenInput<'a> {
    pub enabled: bool,
    pub has_open_project: bool,
    pub busy: bool,
    pub projects: &'a [RecentProject],
}

/// 磁盘上的最近项目列表把 pinned 项目排在前面，列表第一项不代表真正最近
/// 打开过的项目，必须按 `last_opened_ms` 比较。
pub fn choose_project_to_reopen(input: ProjectReopenInput<'_>) -> Option<PathBuf> {
    if !input.enabled || input.has_open_project || input.busy {
        return None;
    }

    input
        .projects
        .iter()
        .max_by_key(|project| project.last_opened_ms)
        .map(|project| project.root.clone())
}

/// 项目打开后是否应该自动恢复上次编辑的文章。
#[derive(Debug, Clone, Copy)]
pub struct DocumentRestoreInput<'a> {
    pub enabled: bool,

    pub expected_project_root: &'a Path,
    pub current_project_root: Option<&'a Path>,

    pub expected_document_epoch: u64,
    pub current_document_epoch: u64,

    pub document_already_selected: bool,
    pub posts: &'a [PostSummary],
    pub last_document_id: Option<&'a str>,
}

/// 五个条件都满足才真正恢复：开关开着、这仍然是派发时的那个项目会话（root
/// 和 epoch 都没变，跨异步边界后项目可能已经被切换或关闭）、用户还没手动
/// 选任何文章、记住的这篇文章还在最新扫出来的列表里。
pub fn choose_document_to_restore(input: DocumentRestoreInput<'_>) -> Option<String> {
    if !input.enabled
        || input.current_project_root != Some(input.expected_project_root)
        || input.document_already_selected
        || input.current_document_epoch != input.expected_document_epoch
    {
        return None;
    }

    let post_id = input.last_document_id?;

    input
        .posts
        .iter()
        .any(|post| post.id == post_id)
        .then(|| post_id.to_owned())
}

/// 一次"记住最后打开的文章"写入意图。`generation` 只用来在 completion 到达
/// 时确认它对应的确实是当前 `in_flight`，不是一次迟到的重复/错配回调。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastDocumentWrite {
    pub generation: u64,
    pub project_root: PathBuf,
    pub document_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastDocumentWriteAction {
    None,
    Persist(LastDocumentWrite),
}

/// 单写者 + 最新值合并：任意时刻最多一个写入在飞，飞行期间又来的新值只
/// 保留最新一份，跳过的中间值不会真的写盘。这个记录本来就是 best-effort
/// （失败只记日志，不重试），所以 completion 不需要携带 success——无论上
/// 一次写盘成功与否都直接看有没有更新的 pending 值可以继续写。
#[derive(Debug, Default)]
pub struct LastDocumentWriter {
    next_generation: u64,
    in_flight: Option<LastDocumentWrite>,
    pending: Option<LastDocumentWrite>,
}

impl LastDocumentWriter {
    pub fn record(&mut self, project_root: &Path, document_id: &str) -> LastDocumentWriteAction {
        let write = LastDocumentWrite {
            generation: self.next_generation,
            project_root: project_root.to_path_buf(),
            document_id: document_id.to_owned(),
        };
        self.next_generation = self.next_generation.wrapping_add(1);

        if self.in_flight.is_some() {
            self.pending = Some(write);
            LastDocumentWriteAction::None
        } else {
            self.in_flight = Some(write.clone());
            LastDocumentWriteAction::Persist(write)
        }
    }

    pub fn complete_write(&mut self, generation: u64) -> LastDocumentWriteAction {
        let Some(in_flight) = self.in_flight.as_ref() else {
            return LastDocumentWriteAction::None;
        };

        // 防止重复或错配的 completion 把真正的 active 状态清掉。
        if in_flight.generation != generation {
            return LastDocumentWriteAction::None;
        }

        self.in_flight = None;

        if let Some(next) = self.pending.take() {
            self.in_flight = Some(next.clone());
            LastDocumentWriteAction::Persist(next)
        } else {
            LastDocumentWriteAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str) -> PostSummary {
        PostSummary {
            id: id.to_string(),
            relative_path: id.to_string(),
            modified_ms: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_input<'a>(
        enabled: bool,
        expected_project_root: &'a Path,
        current_project_root: Option<&'a Path>,
        expected_document_epoch: u64,
        current_document_epoch: u64,
        document_already_selected: bool,
        posts: &'a [PostSummary],
        last_document_id: Option<&'a str>,
    ) -> DocumentRestoreInput<'a> {
        DocumentRestoreInput {
            enabled,
            expected_project_root,
            current_project_root,
            expected_document_epoch,
            current_document_epoch,
            document_already_selected,
            posts,
            last_document_id,
        }
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_disabled() {
        let posts = [post("a.md")];
        let root = PathBuf::from("/tmp/project");
        assert_eq!(
            choose_document_to_restore(restore_input(
                false,
                &root,
                Some(&root),
                1,
                1,
                false,
                &posts,
                Some("a.md"),
            )),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_document_already_selected() {
        let posts = [post("a.md")];
        let root = PathBuf::from("/tmp/project");
        assert_eq!(
            choose_document_to_restore(restore_input(
                true,
                &root,
                Some(&root),
                1,
                1,
                true,
                &posts,
                Some("a.md"),
            )),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_epoch_changed() {
        let posts = [post("a.md")];
        let root = PathBuf::from("/tmp/project");
        assert_eq!(
            choose_document_to_restore(restore_input(
                true,
                &root,
                Some(&root),
                1,
                2,
                false,
                &posts,
                Some("a.md"),
            )),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_post_no_longer_exists() {
        let posts = [post("a.md")];
        let root = PathBuf::from("/tmp/project");
        assert_eq!(
            choose_document_to_restore(restore_input(
                true,
                &root,
                Some(&root),
                1,
                1,
                false,
                &posts,
                Some("missing.md"),
            )),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_project_root_changed() {
        let posts = [post("a.md")];
        let expected_root = PathBuf::from("/tmp/project");
        let current_root = PathBuf::from("/tmp/other-project");
        assert_eq!(
            choose_document_to_restore(restore_input(
                true,
                &expected_root,
                Some(&current_root),
                1,
                1,
                false,
                &posts,
                Some("a.md"),
            )),
            None,
            "跨异步边界后项目已经切换，不该恢复到旧项目的文章"
        );
    }

    #[test]
    fn choose_document_to_restore_returns_the_remembered_id_when_all_conditions_met() {
        let posts = [post("a.md"), post("b.md")];
        let root = PathBuf::from("/tmp/project");
        assert_eq!(
            choose_document_to_restore(restore_input(
                true,
                &root,
                Some(&root),
                1,
                1,
                false,
                &posts,
                Some("b.md"),
            )),
            Some("b.md".to_string())
        );
    }

    fn recent(root: &str, last_opened_ms: u64, pinned: bool) -> RecentProject {
        RecentProject {
            root: PathBuf::from(root),
            last_opened_ms,
            pinned,
            last_document_id: None,
        }
    }

    #[test]
    fn project_reopen_is_blocked_when_disabled_or_workspace_is_not_idle() {
        let projects = [recent("/tmp/a", 100, false)];
        assert_eq!(
            choose_project_to_reopen(ProjectReopenInput {
                enabled: false,
                has_open_project: false,
                busy: false,
                projects: &projects,
            }),
            None
        );
        assert_eq!(
            choose_project_to_reopen(ProjectReopenInput {
                enabled: true,
                has_open_project: true,
                busy: false,
                projects: &projects,
            }),
            None,
            "已经有打开的项目（比如 e2e 强制打开）不该被抢"
        );
        assert_eq!(
            choose_project_to_reopen(ProjectReopenInput {
                enabled: true,
                has_open_project: false,
                busy: true,
                projects: &projects,
            }),
            None,
            "用户手速极快已经在做别的操作时不该抢"
        );
    }

    #[test]
    fn project_reopen_uses_highest_timestamp_not_pinned_display_order() {
        let projects = [
            recent("/tmp/pinned-old", 100, true),
            recent("/tmp/unpinned-new", 200, false),
        ];
        assert_eq!(
            choose_project_to_reopen(ProjectReopenInput {
                enabled: true,
                has_open_project: false,
                busy: false,
                projects: &projects,
            }),
            Some(PathBuf::from("/tmp/unpinned-new")),
            "即使 pinned 项目排在数组第一项，也必须选真正最近打开的那个"
        );
    }

    #[test]
    fn first_last_document_record_starts_write() {
        let mut writer = LastDocumentWriter::default();
        let root = PathBuf::from("/tmp/project");
        let action = writer.record(&root, "a.md");
        assert_eq!(
            action,
            LastDocumentWriteAction::Persist(LastDocumentWrite {
                generation: 0,
                project_root: root,
                document_id: "a.md".into(),
            })
        );
    }

    #[test]
    fn writes_while_busy_keep_only_the_latest_pending_value() {
        let mut writer = LastDocumentWriter::default();
        let root = PathBuf::from("/tmp/project");

        let a = writer.record(&root, "a.md");
        assert!(matches!(a, LastDocumentWriteAction::Persist(_)));
        assert_eq!(writer.record(&root, "b.md"), LastDocumentWriteAction::None);
        assert_eq!(writer.record(&root, "c.md"), LastDocumentWriteAction::None);

        let a_generation = match a {
            LastDocumentWriteAction::Persist(write) => write.generation,
            LastDocumentWriteAction::None => unreachable!(),
        };
        let completion = writer.complete_write(a_generation);
        assert_eq!(
            completion,
            LastDocumentWriteAction::Persist(LastDocumentWrite {
                generation: 2,
                project_root: root.clone(),
                document_id: "c.md".into(),
            }),
            "中间的 b.md 应该被跳过，只写最后的 c.md"
        );

        assert_eq!(writer.complete_write(2), LastDocumentWriteAction::None);

        // writer 已经恢复 idle，新记录应该立即再次开始写入。
        let d = writer.record(&root, "d.md");
        assert!(matches!(d, LastDocumentWriteAction::Persist(_)));
    }

    #[test]
    fn mismatched_completion_does_not_release_the_active_write() {
        let mut writer = LastDocumentWriter::default();
        let root = PathBuf::from("/tmp/project");
        writer.record(&root, "a.md");

        // 一个不对应当前 in_flight 的 generation（比如迟到的重复回调）不能
        // 误清 active 状态，也不能提前把还没到期的 pending 启动。
        assert_eq!(writer.complete_write(999), LastDocumentWriteAction::None);

        assert_eq!(writer.record(&root, "b.md"), LastDocumentWriteAction::None);
        let completion = writer.complete_write(0);
        assert_eq!(
            completion,
            LastDocumentWriteAction::Persist(LastDocumentWrite {
                generation: 1,
                project_root: root,
                document_id: "b.md".into(),
            })
        );
    }
}
