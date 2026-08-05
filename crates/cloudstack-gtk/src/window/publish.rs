use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::model::{ChangeKind, GitStatus, PostSummary, ProjectContext, PublishResult};
use cloudstack_core::services::{git, project};

use super::{
    git_panel, git_panel::operation_log, has_unsaved_documents, set_busy, show_error, toast,
    EditorState, Widgets,
};
use crate::tasks;

const MAX_STATUS_CHANGES: usize = 100;

struct PublishDialog {
    dialog: adw::Dialog,
    branch_label: gtk::Label,
    upstream_label: gtk::Label,
    changes_label: gtk::Label,
    article_choices: Vec<ArticleChoice>,
    remember_choices: gtk::CheckButton,
    message_entry: gtk::Entry,
    push_check: gtk::CheckButton,
    publish_button: gtk::Button,
    spinner: gtk::Spinner,
    result_label: gtk::Label,
    trace_buffer: gtk::TextBuffer,
    status_allows_publish: Cell<bool>,
    has_upstream: Cell<bool>,
    working: Cell<bool>,
}

struct ArticleChoice {
    article_id: Option<String>,
    paths: Vec<String>,
    checkbox: gtk::CheckButton,
}

impl PublishDialog {
    fn new(context: &ProjectContext, posts: &[PostSummary], status: &GitStatus) -> Rc<Self> {
        let branch_label = detail_label();
        let upstream_label = detail_label();
        let changes_label = detail_label();
        changes_label.set_selectable(true);
        changes_label.set_wrap(true);

        let status_group = adw::PreferencesGroup::builder()
            .title("仓库状态")
            .description("只提交下方勾选的文章及其图片；CloudStack 配置和其他文件不会提交。")
            .build();
        let branch_row = adw::ActionRow::builder().title("分支").build();
        branch_row.add_suffix(&branch_label);
        let upstream_row = adw::ActionRow::builder().title("上游").build();
        upstream_row.add_suffix(&upstream_label);
        status_group.add(&branch_row);
        status_group.add(&upstream_row);

        let changes_frame = gtk::Frame::builder().child(&changes_label).build();
        let changes_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(150)
            .max_content_height(260)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&changes_frame)
            .build();

        let article_choices = build_article_choices(context, posts, status);
        let choices_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for choice in &article_choices {
            choices_box.append(&choice.checkbox);
        }
        if article_choices.is_empty() {
            choices_box.append(
                &gtk::Label::builder()
                    .label("没有可选择的文章改动")
                    .xalign(0.0)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        let choices_group = adw::PreferencesGroup::builder()
            .title("本次提交")
            .description("取消勾选可跳过文章；记住选择后，下次仍保持排除。")
            .build();
        let choices_row = adw::PreferencesRow::new();
        choices_row.set_child(Some(&choices_box));
        choices_group.add(&choices_row);
        let remember_choices = gtk::CheckButton::with_label("记住文章选择");
        remember_choices.set_active(true);

        let message_entry = gtk::Entry::builder()
            .placeholder_text("提交信息（必填）")
            .activates_default(true)
            .build();
        let push_check = gtk::CheckButton::with_label("提交后推送到 upstream");
        let result_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .css_classes(["dim-label"])
            .build();
        let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
        form.append(&message_entry);
        form.append(&push_check);
        form.append(&remember_choices);
        form.append(&result_label);

        let trace_buffer = gtk::TextBuffer::new(None);
        let trace_view = gtk::TextView::builder()
            .buffer(&trace_buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .build();
        let trace_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(120)
            .max_content_height(220)
            .child(&trace_view)
            .build();
        let trace_expander = gtk::Expander::builder()
            .label("执行记录")
            .child(&trace_scroll)
            .build();
        form.append(&trace_expander);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);
        content.append(&status_group);
        content.append(&changes_scroll);
        content.append(&choices_group);
        content.append(&form);

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        let publish_button = gtk::Button::builder()
            .label("发布")
            .css_classes(["suggested-action"])
            .sensitive(false)
            .build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some("提交与推送"))));
        header.pack_end(&spinner);
        header.pack_end(&publish_button);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title("提交与推送")
            .content_width(620)
            .content_height(540)
            .child(&toolbar)
            .build();

        let this = Rc::new(Self {
            dialog,
            branch_label,
            upstream_label,
            changes_label,
            article_choices,
            remember_choices,
            message_entry,
            push_check,
            publish_button,
            spinner,
            result_label,
            trace_buffer,
            status_allows_publish: Cell::new(false),
            has_upstream: Cell::new(false),
            working: Cell::new(false),
        });
        this.apply_status(status);
        let weak = Rc::downgrade(&this);
        this.message_entry.connect_changed(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.sync_publish_button();
            }
        });
        for choice in &this.article_choices {
            let weak = Rc::downgrade(&this);
            choice.checkbox.connect_toggled(move |_| {
                if let Some(dialog) = weak.upgrade() {
                    dialog.sync_publish_button();
                }
            });
        }
        this
    }

    fn apply_status(&self, status: &GitStatus) {
        self.branch_label
            .set_label(status.branch.as_deref().unwrap_or("分离 HEAD"));
        let upstream = status.upstream.as_deref().unwrap_or("未配置");
        self.upstream_label.set_label(&format!(
            "{upstream} · ahead {} / behind {}",
            status.ahead, status.behind
        ));
        self.push_check.set_sensitive(status.upstream.is_some());
        self.push_check.set_active(status.upstream.is_some());
        self.has_upstream.set(status.upstream.is_some());

        if status.changes.is_empty() {
            self.changes_label.set_label("工作区没有改动");
        } else {
            let mut lines = status
                .changes
                .iter()
                .filter(|change| change.managed)
                .chain(status.changes.iter().filter(|change| !change.managed))
                .take(MAX_STATUS_CHANGES)
                .map(|change| {
                    let scope = if change.managed {
                        "受管"
                    } else {
                        "非受管"
                    };
                    let staged = if change.staged { "，已暂存" } else { "" };
                    let renamed = change
                        .old_path
                        .as_deref()
                        .map(|old| format!("（{old} → {}）", change.path))
                        .unwrap_or_else(|| change.path.clone());
                    format!(
                        "{}  [{scope}{staged}] {renamed}",
                        change_kind_symbol(change.kind)
                    )
                })
                .collect::<Vec<_>>();
            let omitted = status.changes.len().saturating_sub(lines.len());
            if omitted > 0 {
                lines.push(format!(
                    "… 还有 {omitted} 项未展开；请完善 .gitignore 后刷新"
                ));
            }
            self.changes_label.set_label(&lines.join("\n"));
        }

        let has_managed = status.changes.iter().any(|change| change.managed);
        let has_conflict = status
            .changes
            .iter()
            .any(|change| change.kind == ChangeKind::Unmerged);
        self.status_allows_publish
            .set(has_managed && !has_conflict && status.behind == 0);
        if has_conflict && self.result_label.label().is_empty() {
            self.result_label
                .set_label("存在未解决的 Git 冲突，必须先在外部解决。")
        } else if status.behind > 0 && self.result_label.label().is_empty() {
            self.result_label
                .set_label("远端含有本地没有的提交；为避免制造分叉，必须先处理同步状态。")
        } else if !has_managed && self.result_label.label().is_empty() {
            self.result_label.set_label("没有受管改动可提交。")
        }
        self.sync_publish_button();
    }

    fn set_working(&self, working: bool) {
        self.working.set(working);
        self.dialog.set_can_close(!working);
        self.message_entry.set_sensitive(!working);
        self.push_check
            .set_sensitive(!working && self.has_upstream.get());
        self.remember_choices.set_sensitive(!working);
        for choice in &self.article_choices {
            choice.checkbox.set_sensitive(!working);
        }
        self.spinner.set_visible(working);
        if working {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }
        self.sync_publish_button();
    }

    fn sync_publish_button(&self) {
        self.publish_button.set_sensitive(
            !self.working.get()
                && self.status_allows_publish.get()
                && self
                    .article_choices
                    .iter()
                    .any(|choice| choice.checkbox.is_active())
                && !self.message_entry.text().trim().is_empty(),
        );
    }

    fn selected_paths(&self) -> Vec<String> {
        self.article_choices
            .iter()
            .filter(|choice| choice.checkbox.is_active())
            .flat_map(|choice| choice.paths.iter().cloned())
            .collect()
    }

    fn updated_exclusions(&self, context: &ProjectContext) -> Vec<String> {
        let mut excluded = context
            .config
            .git
            .excluded_articles
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for choice in &self.article_choices {
            let Some(article) = &choice.article_id else {
                continue;
            };
            if choice.checkbox.is_active() {
                excluded.remove(article);
            } else {
                excluded.insert(article.clone());
            }
        }
        excluded.into_iter().collect()
    }
}

fn build_article_choices(
    context: &ProjectContext,
    posts: &[PostSummary],
    status: &GitStatus,
) -> Vec<ArticleChoice> {
    let content_prefix = format!("{}/", context.config.content_dir.trim_end_matches('/'));
    let mut article_ids = posts
        .iter()
        .map(|post| post.id.clone())
        .collect::<BTreeSet<_>>();
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
            let label = article_id
                .clone()
                .unwrap_or_else(|| paths.first().cloned().unwrap_or_default());
            let active = article_id.as_ref().is_none_or(|article| {
                !context
                    .config
                    .git
                    .excluded_articles
                    .iter()
                    .any(|excluded| excluded == article)
            });
            let checkbox = gtk::CheckButton::with_label(&label);
            checkbox.set_active(active);
            checkbox.set_tooltip_text(Some(&format!("{} 个受管路径", paths.len())));
            ArticleChoice {
                article_id,
                paths,
                checkbox,
            }
        })
        .collect()
}

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

pub(super) fn show_dialog(widgets: &Widgets, state: &Rc<std::cell::RefCell<EditorState>>) {
    let has_unsaved = has_unsaved_documents(state);
    let (context, posts) = {
        let state = state.borrow();
        if state.busy || state.dirty || has_unsaved {
            return;
        }
        let Some(context) = &state.project else {
            return;
        };
        (context.clone(), state.posts.clone())
    };
    set_busy(widgets, state, true, "正在读取 Git 状态…");
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        {
            let context = context.clone();
            move || git::status(&context)
        },
        move |result| {
            set_busy(&widgets, &state, false, "");
            match result {
                Ok(status) => present_dialog(&widgets, &state, context, posts, status),
                Err(error) => show_error(&widgets, &error.to_string()),
            }
        },
    );
}

fn present_dialog(
    widgets: &Widgets,
    state: &Rc<std::cell::RefCell<EditorState>>,
    context: cloudstack_core::ProjectContext,
    posts: Vec<PostSummary>,
    status: GitStatus,
) {
    let dialog = PublishDialog::new(&context, &posts, &status);
    let callback_dialog = Rc::clone(&dialog);
    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    dialog.publish_button.connect_clicked(move |_| {
        let message = callback_dialog.message_entry.text().trim().to_owned();
        if message.is_empty() || callback_dialog.working.get() {
            return;
        }
        let push = callback_dialog.push_check.is_active();
        let selected_paths = callback_dialog.selected_paths();
        let remember_choices = callback_dialog.remember_choices.is_active();
        let updated_exclusions = callback_dialog.updated_exclusions(&context);
        callback_dialog.result_label.set_label("正在暂存受管改动…");
        callback_dialog.set_working(true);
        set_busy(
            &callback_widgets,
            &callback_state,
            true,
            "正在提交 Git 改动…",
        );

        let task_context = context.clone();
        let fallback_context = context.clone();
        let task_dialog = Rc::clone(&callback_dialog);
        let task_widgets = callback_widgets.clone();
        let task_state = Rc::clone(&callback_state);
        tasks::run(
            move || {
                let task_context = if remember_choices {
                    let mut config = task_context.config.clone();
                    config.git.excluded_articles = updated_exclusions;
                    project::write_project_config(&task_context, config)?
                } else {
                    task_context
                };
                let result = git::publish_selected(&task_context, &message, push, &selected_paths)?;
                Ok((result, task_context))
            },
            move |result| {
                let refresh_context = match &result {
                    Ok((_, updated_context)) => updated_context.clone(),
                    Err(_) => fallback_context.clone(),
                };
                match result {
                    Ok((result, updated_context)) => {
                        let should_replace = task_state
                            .borrow()
                            .project
                            .as_ref()
                            .is_some_and(|project| project.root == updated_context.root);
                        if should_replace {
                            task_state.borrow_mut().project = Some(updated_context);
                        }
                        task_dialog
                            .result_label
                            .set_label(&publish_summary(&result, push));
                        task_dialog
                            .trace_buffer
                            .set_text(&operation_log(&result.report));
                        if result.error.is_none() {
                            toast(&task_widgets, "Git 提交发布完成");
                        } else {
                            show_error(&task_widgets, "Git 发布未完整完成，请查看分阶段结果");
                        }
                    }
                    Err(error) => {
                        task_dialog
                            .result_label
                            .set_label(&format!("Git 操作失败：{error}"));
                        show_error(&task_widgets, &error.to_string());
                    }
                }
                refresh_status(
                    &task_widgets,
                    &task_state,
                    refresh_context,
                    Rc::clone(&task_dialog),
                );
            },
        );
    });
    dialog.dialog.present(Some(&widgets.window));
    dialog.message_entry.grab_focus();
}

fn refresh_status(
    widgets: &Widgets,
    state: &Rc<std::cell::RefCell<EditorState>>,
    context: cloudstack_core::ProjectContext,
    dialog: Rc<PublishDialog>,
) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || git::status(&context),
        move |result| {
            set_busy(&widgets, &state, false, "");
            dialog.set_working(false);
            match result {
                Ok(status) => dialog.apply_status(&status),
                Err(error) => show_error(&widgets, &format!("刷新 Git 状态失败：{error}")),
            }
            git_panel::refresh(&widgets, &state);
        },
    );
}

fn detail_label() -> gtk::Label {
    gtk::Label::builder()
        .xalign(1.0)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(42)
        .build()
}

fn change_kind_symbol(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Modified => "M",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Untracked => "?",
        ChangeKind::Unmerged => "!",
    }
}

fn publish_summary(result: &PublishResult, requested_push: bool) -> String {
    let mut lines = Vec::new();
    if result.staged {
        lines.push(format!("暂存：{} 个受管路径", result.staged_files.len()));
    }
    if result.committed {
        lines.push(format!(
            "提交：{}",
            result.commit_hash.as_deref().unwrap_or("已完成")
        ));
    }
    if result.pushed {
        lines.push("推送：已完成".to_string());
    } else if !requested_push && result.committed {
        lines.push("推送：本次未请求".to_string());
    }
    if let Some(stage) = &result.error_stage {
        lines.push(format!("停止阶段：{stage}"));
    }
    if let Some(error) = &result.error {
        lines.push(error.fallback().to_string());
    }
    if lines.is_empty() {
        "没有执行 Git 改动。".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_summary_preserves_partial_success() {
        let result = PublishResult {
            staged: true,
            staged_files: vec!["content/a.md".into()],
            committed: true,
            commit_hash: Some("abc123".into()),
            pushed: false,
            error_stage: Some("push".into()),
            error: Some(cloudstack_core::error::ErrorPayload::git_push_failed(
                "推送失败",
            )),
            report: Default::default(),
        };
        let summary = publish_summary(&result, true);
        assert!(summary.contains("暂存：1"));
        assert!(summary.contains("提交：abc123"));
        assert!(summary.contains("停止阶段：push"));
        assert!(summary.contains("推送失败"));
    }

    #[test]
    fn operation_log_keeps_commands_and_outputs() {
        let report = cloudstack_core::model::OperationReport {
            traces: vec![cloudstack_core::model::CommandTrace {
                command: "git push".into(),
                stdout: "ok\n".into(),
                stderr: String::new(),
                exit_code: Some(0),
                success: true,
                duration_ms: 12,
            }],
        };
        let log = operation_log(&report);
        assert!(log.contains("$ git push"));
        assert!(log.contains("退出: 0"));
        assert!(log.contains("ok"));
    }

    #[test]
    fn nested_article_wins_over_an_outer_asset_directory() {
        let root = std::path::PathBuf::from("/tmp/cloudstack-publish-group-test");
        let context = ProjectContext {
            content_root: root.join("src/content/blog"),
            config_path: root.join(".cloudstack.json"),
            root,
            config: Default::default(),
        };
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
}
