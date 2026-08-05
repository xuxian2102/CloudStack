use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::model::{ChangeKind, GitStatus, PublishResult};
use cloudstack_core::services::git;

use super::{set_busy, show_error, toast, EditorState, Widgets};
use crate::tasks;

struct PublishDialog {
    dialog: adw::Dialog,
    branch_label: gtk::Label,
    upstream_label: gtk::Label,
    changes_label: gtk::Label,
    message_entry: gtk::Entry,
    push_check: gtk::CheckButton,
    publish_button: gtk::Button,
    spinner: gtk::Spinner,
    result_label: gtk::Label,
    status_allows_publish: Cell<bool>,
    has_upstream: Cell<bool>,
    working: Cell<bool>,
}

impl PublishDialog {
    fn new(status: &GitStatus) -> Rc<Self> {
        let branch_label = detail_label();
        let upstream_label = detail_label();
        let changes_label = detail_label();
        changes_label.set_selectable(true);
        changes_label.set_wrap(true);

        let status_group = adw::PreferencesGroup::builder()
            .title("仓库状态")
            .description("只会暂存内容目录与当前项目配置文件；其他改动仅供检查。")
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
        form.append(&result_label);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);
        content.append(&status_group);
        content.append(&changes_scroll);
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
            message_entry,
            push_check,
            publish_button,
            spinner,
            result_label,
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
            let text = status
                .changes
                .iter()
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
                .collect::<Vec<_>>()
                .join("\n");
            self.changes_label.set_label(&text);
        }

        let has_managed = status.changes.iter().any(|change| change.managed);
        let has_conflict = status
            .changes
            .iter()
            .any(|change| change.kind == ChangeKind::Unmerged);
        self.status_allows_publish.set(has_managed && !has_conflict);
        if has_conflict && self.result_label.label().is_empty() {
            self.result_label
                .set_label("存在未解决的 Git 冲突，必须先在外部解决。")
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
                && !self.message_entry.text().trim().is_empty(),
        );
    }
}

pub(super) fn show_dialog(widgets: &Widgets, state: &Rc<std::cell::RefCell<EditorState>>) {
    let context = {
        let state = state.borrow();
        if state.busy || state.dirty {
            return;
        }
        let Some(context) = &state.project else {
            return;
        };
        context.clone()
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
                Ok(status) => present_dialog(&widgets, &state, context, status),
                Err(error) => show_error(&widgets, &error.to_string()),
            }
        },
    );
}

fn present_dialog(
    widgets: &Widgets,
    state: &Rc<std::cell::RefCell<EditorState>>,
    context: cloudstack_core::ProjectContext,
    status: GitStatus,
) {
    let dialog = PublishDialog::new(&status);
    let callback_dialog = Rc::clone(&dialog);
    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    dialog.publish_button.connect_clicked(move |_| {
        let message = callback_dialog.message_entry.text().trim().to_owned();
        if message.is_empty() || callback_dialog.working.get() {
            return;
        }
        let push = callback_dialog.push_check.is_active();
        callback_dialog.result_label.set_label("正在暂存受管改动…");
        callback_dialog.set_working(true);
        set_busy(
            &callback_widgets,
            &callback_state,
            true,
            "正在提交 Git 改动…",
        );

        let task_context = context.clone();
        let refresh_context = context.clone();
        let task_dialog = Rc::clone(&callback_dialog);
        let task_widgets = callback_widgets.clone();
        let task_state = Rc::clone(&callback_state);
        tasks::run(
            move || git::publish(&task_context, &message, push),
            move |result| {
                match result {
                    Ok(result) => {
                        task_dialog
                            .result_label
                            .set_label(&publish_summary(&result, push));
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
        };
        let summary = publish_summary(&result, true);
        assert!(summary.contains("暂存：1"));
        assert!(summary.contains("提交：abc123"));
        assert!(summary.contains("停止阶段：push"));
        assert!(summary.contains("推送失败"));
    }
}
