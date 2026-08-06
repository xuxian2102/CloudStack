//! 发布对话框的数据和决策：给定项目、文章列表和 Git 状态，算出"这次提交
//! 应该展示哪些可选项、当前能不能提交、提交后要写回哪些数据"。不依赖
//! GTK——GTK 只负责从 [`PublishChoice`] 创建 checkbox、展示 [`PublishBlocker`]
//! 对应的提示文案，以及执行 [`PublishSubmission`] 描述的副作用（写项目配置、
//! 调 `git::publish_selected`）。
//!
//! `cloudstack-core` 的 `publish_internal()` 仍然会重新读一遍 Git status、
//! 重新拒绝 behind/冲突/空 managed scope——这里算出来的 `PublishPlan` 是交互
//! 层的前置反馈，不是最后一道可信边界。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cloudstack_core::model::{ChangeKind, GitStatus, PostSummary, ProjectContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishChoice {
    pub article_id: Option<String>,
    pub paths: Vec<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStatusBlocker {
    Conflicts,
    BehindRemote,
    NoManagedChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishBlocker {
    Working,
    Status(PublishStatusBlocker),
    NoSelection,
    EmptyMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishSubmission {
    pub message: String,
    pub push: bool,
    pub selected_paths: Vec<String>,
    /// `None` 表示用户没有勾选"记住文章选择"，GTK 不写项目配置。
    pub updated_exclusions: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct PublishPlan {
    choices: Vec<PublishChoice>,
    base_exclusions: BTreeSet<String>,
    status_blocker: Option<PublishStatusBlocker>,
    has_upstream: bool,
}

impl PublishPlan {
    pub fn new(context: &ProjectContext, posts: &[PostSummary], status: &GitStatus) -> Self {
        Self {
            choices: build_choices(context, posts, status),
            base_exclusions: context
                .config
                .git
                .excluded_articles
                .iter()
                .cloned()
                .collect(),
            status_blocker: status_blocker(status),
            has_upstream: status.upstream.is_some(),
        }
    }

    pub fn choices(&self) -> &[PublishChoice] {
        &self.choices
    }

    pub fn set_selected(&mut self, index: usize, selected: bool) {
        if let Some(choice) = self.choices.get_mut(index) {
            choice.selected = selected;
        }
    }

    /// Git 面板刷新后调用：只更新仓库层面的门禁和 upstream 状态，不重建
    /// choices 列表——对话框最初建立的选项应该保留，不因为一次状态刷新就
    /// 打乱用户已经做出的勾选。
    pub fn update_status(&mut self, status: &GitStatus) {
        self.status_blocker = status_blocker(status);
        self.has_upstream = status.upstream.is_some();
    }

    pub fn status_blocker(&self) -> Option<PublishStatusBlocker> {
        self.status_blocker
    }

    pub fn has_upstream(&self) -> bool {
        self.has_upstream
    }

    /// 优先级：正在执行 > 仓库状态门禁 > 没有选中任何文章 > 提交信息为空。
    pub fn blocker(&self, message: &str, working: bool) -> Option<PublishBlocker> {
        if working {
            return Some(PublishBlocker::Working);
        }
        if let Some(blocker) = self.status_blocker {
            return Some(PublishBlocker::Status(blocker));
        }
        if !self.choices.iter().any(|choice| choice.selected) {
            return Some(PublishBlocker::NoSelection);
        }
        if message.trim().is_empty() {
            return Some(PublishBlocker::EmptyMessage);
        }
        None
    }

    pub fn prepare_submission(
        &self,
        message: &str,
        push_requested: bool,
        remember_choices: bool,
        working: bool,
    ) -> Result<PublishSubmission, PublishBlocker> {
        if let Some(blocker) = self.blocker(message, working) {
            return Err(blocker);
        }

        let selected_paths = self
            .choices
            .iter()
            .filter(|choice| choice.selected)
            .flat_map(|choice| choice.paths.iter().cloned())
            .collect();

        let updated_exclusions = remember_choices.then(|| {
            let mut excluded = self.base_exclusions.clone();
            for choice in &self.choices {
                let Some(article_id) = &choice.article_id else {
                    continue;
                };
                if choice.selected {
                    excluded.remove(article_id);
                } else {
                    excluded.insert(article_id.clone());
                }
            }
            excluded.into_iter().collect()
        });

        Ok(PublishSubmission {
            message: message.trim().to_owned(),
            // UI 状态即使意外不同步，也不能在没有 upstream 时请求 push。
            push: push_requested && self.has_upstream,
            selected_paths,
            updated_exclusions,
        })
    }
}

fn status_blocker(status: &GitStatus) -> Option<PublishStatusBlocker> {
    let has_managed = status.changes.iter().any(|change| change.managed);
    let has_conflict = status
        .changes
        .iter()
        .any(|change| change.kind == ChangeKind::Unmerged);

    if has_conflict {
        Some(PublishStatusBlocker::Conflicts)
    } else if status.behind > 0 {
        Some(PublishStatusBlocker::BehindRemote)
    } else if !has_managed {
        Some(PublishStatusBlocker::NoManagedChanges)
    } else {
        None
    }
}

fn build_choices(
    context: &ProjectContext,
    posts: &[PostSummary],
    status: &GitStatus,
) -> Vec<PublishChoice> {
    let content_prefix = format!("{}/", context.config.content_dir.trim_end_matches('/'));
    let mut article_ids = posts
        .iter()
        .map(|post| post.id.clone())
        .collect::<BTreeSet<_>>();
    // 已经从磁盘删除的文章不在 posts 里，但 Git 仍然认得它——同样要能归组、
    // 能作为一个可选提交项，而不是散成一堆无主路径。
    for change in status.changes.iter().filter(|change| change.managed) {
        if let Some(relative) = change.path.strip_prefix(&content_prefix) {
            let extension = Path::new(relative)
                .extension()
                .and_then(|extension| extension.to_str());
            if extension.is_some_and(|extension| {
                context
                    .config
                    .extensions
                    .iter()
                    .any(|allowed| allowed.strip_prefix('.') == Some(extension))
            }) {
                article_ids.insert(relative.to_owned());
            }
        }
    }

    let mut groups: BTreeMap<String, (Option<String>, Vec<String>)> = BTreeMap::new();
    for change in status.changes.iter().filter(|change| change.managed) {
        let article = article_for_git_path(context, &article_ids, &change.path);
        let key = article.as_ref().map_or_else(
            || format!("path:{}", change.path),
            |id| format!("article:{id}"),
        );
        let entry = groups.entry(key).or_insert_with(|| (article, Vec::new()));
        entry.1.push(change.path.clone());
        if let Some(old_path) = &change.old_path {
            if old_path == context.config.content_dir.as_str()
                || old_path.starts_with(&content_prefix)
            {
                entry.1.push(old_path.clone());
            }
        }
    }

    groups
        .into_values()
        .map(|(article_id, mut paths)| {
            paths.sort();
            paths.dedup();
            let selected = article_id.as_ref().is_none_or(|article| {
                !context
                    .config
                    .git
                    .excluded_articles
                    .iter()
                    .any(|excluded| excluded == article)
            });
            PublishChoice {
                article_id,
                paths,
                selected,
            }
        })
        .collect()
}

/// 同名 stem 目录下的资产要归到最长匹配的文章，不能被外层同前缀文章截胡：
/// `hello.md` 和 `hello/nested.md` 都存在时，`hello/nested/photo.png` 必须
/// 归给 `hello/nested.md`。
fn article_for_git_path(
    context: &ProjectContext,
    article_ids: &BTreeSet<String>,
    path: &str,
) -> Option<String> {
    let content_prefix = format!("{}/", context.config.content_dir.trim_end_matches('/'));
    let relative = path.strip_prefix(&content_prefix)?;
    if article_ids.contains(relative) {
        return Some(relative.to_owned());
    }
    article_ids
        .iter()
        .filter(|article| {
            let asset_prefix = article
                .rsplit_once('.')
                .map_or_else(|| format!("{article}/"), |(stem, _)| format!("{stem}/"));
            relative.starts_with(&asset_prefix)
        })
        .max_by_key(|article| article.len())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::model::FileChange;

    fn ctx() -> ProjectContext {
        let root = std::path::PathBuf::from("/tmp/cloudstack-publish-plan-test");
        ProjectContext {
            content_root: root.join("src/content/blog"),
            config_path: root.join(".cloudstack.json"),
            root,
            config: Default::default(),
        }
    }

    fn ctx_with_excluded(excluded: &[&str]) -> ProjectContext {
        let mut context = ctx();
        context.config.git.excluded_articles = excluded.iter().map(|s| s.to_string()).collect();
        context
    }

    fn managed(path: &str, kind: ChangeKind) -> FileChange {
        FileChange {
            path: path.to_owned(),
            old_path: None,
            kind,
            staged: false,
            managed: true,
        }
    }

    fn renamed(old_path: &str, path: &str) -> FileChange {
        FileChange {
            path: path.to_owned(),
            old_path: Some(old_path.to_owned()),
            kind: ChangeKind::Renamed,
            staged: true,
            managed: true,
        }
    }

    fn status(changes: Vec<FileChange>) -> GitStatus {
        GitStatus {
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 0,
            behind: 0,
            changes,
        }
    }

    fn post(id: &str) -> PostSummary {
        PostSummary {
            id: id.to_string(),
            relative_path: id.to_string(),
            modified_ms: None,
        }
    }

    #[test]
    fn nested_article_wins_over_an_outer_asset_directory() {
        let context = ctx();
        let articles = ["hello.md".to_string(), "hello/nested.md".to_string()]
            .into_iter()
            .collect();

        assert_eq!(
            article_for_git_path(&context, &articles, "src/content/blog/hello/nested.md")
                .as_deref(),
            Some("hello/nested.md")
        );
        assert_eq!(
            article_for_git_path(
                &context,
                &articles,
                "src/content/blog/hello/nested/photo.png"
            )
            .as_deref(),
            Some("hello/nested.md")
        );
    }

    #[test]
    fn publish_plan_groups_article_and_colocated_assets() {
        let context = ctx();
        let posts = [post("hello.md")];
        let status = status(vec![
            managed("src/content/blog/hello.md", ChangeKind::Modified),
            managed("src/content/blog/hello/photo.png", ChangeKind::Added),
        ]);

        let plan = PublishPlan::new(&context, &posts, &status);
        assert_eq!(
            plan.choices().len(),
            1,
            "文章和同名目录下的资产应该归成一组"
        );
        let choice = &plan.choices()[0];
        assert_eq!(choice.article_id.as_deref(), Some("hello.md"));
        assert_eq!(
            choice.paths,
            vec![
                "src/content/blog/hello.md".to_string(),
                "src/content/blog/hello/photo.png".to_string(),
            ]
        );
    }

    #[test]
    fn deleted_article_is_still_exposed_as_an_article_choice() {
        let context = ctx();
        // posts 里已经没有 old.md（磁盘上已经删除），但 Git 还认得它。
        let posts: [PostSummary; 0] = [];
        let status = status(vec![managed(
            "src/content/blog/old.md",
            ChangeKind::Deleted,
        )]);

        let plan = PublishPlan::new(&context, &posts, &status);
        assert_eq!(plan.choices().len(), 1);
        assert_eq!(plan.choices()[0].article_id.as_deref(), Some("old.md"));
    }

    #[test]
    fn excluded_articles_start_unselected_but_other_managed_paths_remain_selected() {
        let context = ctx_with_excluded(&["excluded.md"]);
        let posts = [post("excluded.md"), post("kept.md")];
        let status = status(vec![
            managed("src/content/blog/excluded.md", ChangeKind::Modified),
            managed("src/content/blog/kept.md", ChangeKind::Modified),
            managed(".cloudstack.json", ChangeKind::Modified),
        ]);

        let plan = PublishPlan::new(&context, &posts, &status);
        let excluded = plan
            .choices()
            .iter()
            .find(|choice| choice.article_id.as_deref() == Some("excluded.md"))
            .unwrap();
        assert!(!excluded.selected);

        let kept = plan
            .choices()
            .iter()
            .find(|choice| choice.article_id.as_deref() == Some("kept.md"))
            .unwrap();
        assert!(kept.selected);

        let unowned = plan
            .choices()
            .iter()
            .find(|choice| choice.article_id.is_none())
            .unwrap();
        assert!(unowned.selected, "不属于任何文章的其他受管路径默认应该选中");
    }

    #[test]
    fn renamed_article_submission_includes_old_and_new_paths() {
        let context = ctx();
        let posts = [post("new-name.md")];
        let status = status(vec![renamed(
            "src/content/blog/old-name.md",
            "src/content/blog/new-name.md",
        )]);

        let plan = PublishPlan::new(&context, &posts, &status);
        assert_eq!(plan.choices().len(), 1);
        let submission = plan
            .prepare_submission("rename", false, false, false)
            .unwrap();
        assert_eq!(
            submission.selected_paths,
            vec![
                "src/content/blog/new-name.md".to_string(),
                "src/content/blog/old-name.md".to_string(),
            ]
        );
    }

    #[test]
    fn submission_uses_current_selection_and_updates_exclusions() {
        let context = ctx_with_excluded(&["was-excluded.md"]);
        let posts = [post("was-excluded.md"), post("newly-excluded.md")];
        let status = status(vec![
            managed("src/content/blog/was-excluded.md", ChangeKind::Modified),
            managed("src/content/blog/newly-excluded.md", ChangeKind::Modified),
        ]);
        let mut plan = PublishPlan::new(&context, &posts, &status);

        let was_excluded_index = plan
            .choices()
            .iter()
            .position(|choice| choice.article_id.as_deref() == Some("was-excluded.md"))
            .unwrap();
        let newly_excluded_index = plan
            .choices()
            .iter()
            .position(|choice| choice.article_id.as_deref() == Some("newly-excluded.md"))
            .unwrap();
        plan.set_selected(was_excluded_index, true);
        plan.set_selected(newly_excluded_index, false);

        let submission = plan
            .prepare_submission("  发布一下  ", false, true, false)
            .unwrap();
        assert_eq!(submission.message, "发布一下", "提交信息应该被 trim");
        let exclusions = submission
            .updated_exclusions
            .expect("勾选了记住选择，应该返回新的 exclusions");
        assert!(!exclusions.contains(&"was-excluded.md".to_string()));
        assert!(exclusions.contains(&"newly-excluded.md".to_string()));

        let submission_without_remember = plan
            .prepare_submission("发布一下", false, false, false)
            .unwrap();
        assert!(submission_without_remember.updated_exclusions.is_none());
    }

    #[test]
    fn conflicts_behind_and_no_managed_changes_block_submission() {
        let context = ctx();
        let posts = [post("a.md")];

        let mut plan = PublishPlan::new(
            &context,
            &posts,
            &status(vec![managed("src/content/blog/a.md", ChangeKind::Unmerged)]),
        );
        assert_eq!(
            plan.blocker("msg", false),
            Some(PublishBlocker::Status(PublishStatusBlocker::Conflicts))
        );

        let mut behind_status =
            status(vec![managed("src/content/blog/a.md", ChangeKind::Modified)]);
        behind_status.behind = 1;
        plan.update_status(&behind_status);
        assert_eq!(
            plan.blocker("msg", false),
            Some(PublishBlocker::Status(PublishStatusBlocker::BehindRemote))
        );

        plan.update_status(&status(vec![]));
        assert_eq!(
            plan.blocker("msg", false),
            Some(PublishBlocker::Status(
                PublishStatusBlocker::NoManagedChanges
            ))
        );
    }

    #[test]
    fn working_empty_message_and_empty_selection_block_submission() {
        let context = ctx();
        let posts = [post("a.md")];
        let status = status(vec![managed("src/content/blog/a.md", ChangeKind::Modified)]);
        let mut plan = PublishPlan::new(&context, &posts, &status);

        assert_eq!(
            plan.blocker("msg", true),
            Some(PublishBlocker::Working),
            "working 优先级最高"
        );
        assert_eq!(
            plan.prepare_submission("msg", true, false, true),
            Err(PublishBlocker::Working)
        );

        plan.set_selected(0, false);
        assert_eq!(
            plan.blocker("msg", false),
            Some(PublishBlocker::NoSelection)
        );

        plan.set_selected(0, true);
        assert_eq!(
            plan.blocker("   ", false),
            Some(PublishBlocker::EmptyMessage)
        );

        // 没有 upstream 时即使传入 push_requested = true，也不会真的请求 push。
        let mut no_upstream_status = status.clone();
        no_upstream_status.upstream = None;
        plan.update_status(&no_upstream_status);
        let submission = plan.prepare_submission("msg", true, false, false).unwrap();
        assert!(!submission.push);
    }
}
