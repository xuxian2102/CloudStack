use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_application::publish::{PublishPlan, PublishStatusBlocker, PublishSubmission};
use cloudstack_core::model::{ChangeKind, GitStatus, PostSummary, ProjectContext, PublishResult};
use cloudstack_core::services::{git, project};

use super::{
    git_panel, git_panel::operation_log, has_unsaved_documents, set_busy, show_error,
    show_user_facing_error, toast, EditorState, Widgets,
};
use crate::i18n::{self, UiMessage};
use crate::tasks;

const MAX_STATUS_CHANGES: usize = 100;

struct PublishDialog {
    dialog: adw::Dialog,
    branch_label: gtk::Label,
    upstream_label: gtk::Label,
    changes_label: gtk::Label,
    plan: RefCell<PublishPlan>,
    choice_buttons: Vec<gtk::CheckButton>,
    remember_choices: gtk::CheckButton,
    message_entry: gtk::Entry,
    push_check: gtk::CheckButton,
    publish_button: gtk::Button,
    spinner: gtk::Spinner,
    result_label: gtk::Label,
    trace_buffer: gtk::TextBuffer,
    working: Cell<bool>,
}

impl PublishDialog {
    fn new(context: &ProjectContext, posts: &[PostSummary], status: &GitStatus) -> Rc<Self> {
        let branch_label = detail_label();
        let upstream_label = detail_label();
        let changes_label = detail_label();
        changes_label.set_selectable(true);
        changes_label.set_wrap(true);

        let status_group = adw::PreferencesGroup::builder()
            .title(i18n::text(UiMessage::GitPublishRepositoryStatus))
            .description(i18n::text(UiMessage::GitPublishRepositoryDescription))
            .build();
        let branch_row = adw::ActionRow::builder()
            .title(i18n::text(UiMessage::GitBranchLabel))
            .build();
        branch_row.add_suffix(&branch_label);
        let upstream_row = adw::ActionRow::builder()
            .title(i18n::text(UiMessage::GitUpstreamLabel))
            .build();
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

        let plan = PublishPlan::new(context, posts, status);
        let choice_buttons = plan
            .choices()
            .iter()
            .map(|choice| {
                let label = choice
                    .article_id
                    .clone()
                    .unwrap_or_else(|| choice.paths.first().cloned().unwrap_or_default());
                let checkbox = gtk::CheckButton::with_label(&label);
                checkbox.set_active(choice.selected);
                checkbox.set_tooltip_text(Some(&i18n::text(UiMessage::GitManagedPathCount {
                    count: choice.paths.len(),
                })));
                checkbox
            })
            .collect::<Vec<_>>();
        let choices_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for checkbox in &choice_buttons {
            choices_box.append(checkbox);
        }
        if choice_buttons.is_empty() {
            choices_box.append(
                &gtk::Label::builder()
                    .label(i18n::text(UiMessage::GitNoSelectableChanges))
                    .xalign(0.0)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        let choices_group = adw::PreferencesGroup::builder()
            .title(i18n::text(UiMessage::GitThisCommit))
            .description(i18n::text(UiMessage::GitThisCommitDescription))
            .build();
        let choices_row = adw::PreferencesRow::new();
        choices_row.set_child(Some(&choices_box));
        choices_group.add(&choices_row);
        let remember_choices =
            gtk::CheckButton::with_label(&i18n::text(UiMessage::GitRememberArticleSelection));
        remember_choices.set_active(true);

        let message_entry = gtk::Entry::builder()
            .placeholder_text(i18n::text(UiMessage::GitCommitMessagePlaceholder))
            .activates_default(true)
            .build();
        let push_check = gtk::CheckButton::with_label(&i18n::text(UiMessage::GitPushAfterCommit));
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
            .label(i18n::text(UiMessage::GitExecutionLog))
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
            .label(i18n::text(UiMessage::GitPublish))
            .css_classes(["suggested-action"])
            .sensitive(false)
            .build();
        let header = adw::HeaderBar::new();
        let title = i18n::text(UiMessage::GitCommitAndPushTitle);
        header.set_title_widget(Some(&gtk::Label::new(Some(&title))));
        header.pack_end(&spinner);
        header.pack_end(&publish_button);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(620)
            .content_height(540)
            .child(&toolbar)
            .build();

        let this = Rc::new(Self {
            dialog,
            branch_label,
            upstream_label,
            changes_label,
            plan: RefCell::new(plan),
            choice_buttons,
            remember_choices,
            message_entry,
            push_check,
            publish_button,
            spinner,
            result_label,
            trace_buffer,
            working: Cell::new(false),
        });
        this.apply_status(status);
        let weak = Rc::downgrade(&this);
        this.message_entry.connect_changed(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.sync_publish_button();
            }
        });
        for (index, checkbox) in this.choice_buttons.iter().enumerate() {
            let weak = Rc::downgrade(&this);
            checkbox.connect_toggled(move |checkbox| {
                if let Some(dialog) = weak.upgrade() {
                    dialog
                        .plan
                        .borrow_mut()
                        .set_selected(index, checkbox.is_active());
                    dialog.sync_publish_button();
                }
            });
        }
        this
    }

    fn apply_status(&self, status: &GitStatus) {
        self.branch_label
            .set_label(status.branch.as_deref().unwrap_or(""));
        if status.branch.is_none() {
            self.branch_label
                .set_label(&i18n::text(UiMessage::GitBranchDetached));
        }
        let upstream = status
            .upstream
            .clone()
            .unwrap_or_else(|| i18n::text(UiMessage::GitUpstreamNotConfigured));
        self.upstream_label
            .set_label(&i18n::text(UiMessage::GitUpstreamStatus {
                upstream,
                ahead: status.ahead,
                behind: status.behind,
            }));

        if status.changes.is_empty() {
            self.changes_label
                .set_label(&i18n::text(UiMessage::GitWorkspaceNoChanges));
        } else {
            let mut lines = status
                .changes
                .iter()
                .filter(|change| change.managed)
                .chain(status.changes.iter().filter(|change| !change.managed))
                .take(MAX_STATUS_CHANGES)
                .map(localized_change_line)
                .collect::<Vec<_>>();
            let omitted = status.changes.len().saturating_sub(lines.len());
            if omitted > 0 {
                lines.push(i18n::text(UiMessage::GitChangesOmitted { count: omitted }));
            }
            self.changes_label.set_label(&lines.join("\n"));
        }

        let (has_upstream, status_blocker) = {
            let mut plan = self.plan.borrow_mut();
            plan.update_status(status);
            (plan.has_upstream(), plan.status_blocker())
        };
        self.push_check.set_sensitive(has_upstream);
        self.push_check.set_active(has_upstream);

        if self.result_label.label().is_empty() {
            match status_blocker {
                Some(PublishStatusBlocker::Conflicts) => self
                    .result_label
                    .set_label(&i18n::text(UiMessage::GitConflictStatus)),
                Some(PublishStatusBlocker::BehindRemote) => self
                    .result_label
                    .set_label(&i18n::text(UiMessage::GitBehindStatus)),
                Some(PublishStatusBlocker::NoManagedChanges) => self
                    .result_label
                    .set_label(&i18n::text(UiMessage::GitNoManagedChanges)),
                None => {}
            }
        }
        self.sync_publish_button();
    }

    fn set_working(&self, working: bool) {
        self.working.set(working);
        self.dialog.set_can_close(!working);
        self.message_entry.set_sensitive(!working);
        self.push_check
            .set_sensitive(!working && self.plan.borrow().has_upstream());
        self.remember_choices.set_sensitive(!working);
        for checkbox in &self.choice_buttons {
            checkbox.set_sensitive(!working);
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
        let blocked = self
            .plan
            .borrow()
            .blocker(&self.message_entry.text(), self.working.get())
            .is_some();
        self.publish_button.set_sensitive(!blocked);
    }
}

pub(super) fn show_dialog(widgets: &Widgets, state: &Rc<std::cell::RefCell<EditorState>>) {
    let has_unsaved = has_unsaved_documents(state);
    let (context, posts) = {
        let state = state.borrow();
        if state.session.busy || state.session.dirty || has_unsaved {
            return;
        }
        let Some(context) = &state.session.project else {
            return;
        };
        (context.clone(), state.session.posts.clone())
    };
    let status_message = i18n::text(UiMessage::GitReadingStatus);
    set_busy(widgets, state, true, &status_message);
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
                Err(error) => show_user_facing_error(&widgets, &error),
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
        let submission = {
            let plan = callback_dialog.plan.borrow();
            plan.prepare_submission(
                &callback_dialog.message_entry.text(),
                callback_dialog.push_check.is_active(),
                callback_dialog.remember_choices.is_active(),
                callback_dialog.working.get(),
            )
        };
        let Ok(submission) = submission else {
            return;
        };
        let PublishSubmission {
            message,
            push,
            selected_paths,
            updated_exclusions,
        } = submission;

        callback_dialog
            .result_label
            .set_label(&i18n::text(UiMessage::GitStagingStatus));
        callback_dialog.set_working(true);
        set_busy(
            &callback_widgets,
            &callback_state,
            true,
            &i18n::text(UiMessage::GitCommittingStatus),
        );

        let task_context = context.clone();
        let fallback_context = context.clone();
        let task_dialog = Rc::clone(&callback_dialog);
        let task_widgets = callback_widgets.clone();
        let task_state = Rc::clone(&callback_state);
        tasks::run(
            move || {
                let task_context = match updated_exclusions {
                    Some(exclusions) => {
                        let mut config = task_context.config.clone();
                        config.git.excluded_articles = exclusions;
                        project::write_project_config(&task_context, config)?
                    }
                    None => task_context,
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
                            .session
                            .project
                            .as_ref()
                            .is_some_and(|project| project.root == updated_context.root);
                        if should_replace {
                            task_state.borrow_mut().session.project = Some(updated_context);
                        }
                        task_dialog
                            .result_label
                            .set_label(&publish_summary(&result, push));
                        task_dialog
                            .trace_buffer
                            .set_text(&publish_operation_log(&result));
                        if result.error.is_none() {
                            toast(
                                &task_widgets,
                                &i18n::text(UiMessage::GitPublishSuccessToast),
                            );
                        } else {
                            show_error(
                                &task_widgets,
                                &i18n::text(UiMessage::GitPublishIncompleteToast),
                            );
                        }
                    }
                    Err(error) => {
                        task_dialog.result_label.set_label(&format!(
                            "{}: {error}",
                            i18n::text(UiMessage::GitOperationIncomplete)
                        ));
                        show_user_facing_error(&task_widgets, &error);
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
                Err(error) => show_user_facing_error(&widgets, &error),
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

fn localized_change_line(change: &cloudstack_core::model::FileChange) -> String {
    let path = change.old_path.as_deref().map_or_else(
        || change.path.clone(),
        |old_path| {
            i18n::text(UiMessage::GitRenamedPath {
                old_path: old_path.to_owned(),
                path: change.path.clone(),
            })
        },
    );
    i18n::text(UiMessage::GitChangeLine {
        marker: change_kind_symbol(change.kind).to_owned(),
        scope: if change.managed {
            String::new()
        } else {
            i18n::text(UiMessage::GitUnmanagedSuffix)
        },
        staged: if change.staged {
            i18n::text(UiMessage::GitStagedSuffix)
        } else {
            String::new()
        },
        path,
    })
}

fn publish_summary(result: &PublishResult, requested_push: bool) -> String {
    let mut lines = Vec::new();
    if result.staged {
        lines.push(i18n::text(UiMessage::GitStageSummary {
            count: result.staged_files.len(),
        }));
    }
    if result.committed {
        lines.push(i18n::text(UiMessage::GitCommitSummary {
            hash: result.commit_hash.as_deref().unwrap_or("done").to_owned(),
        }));
    }
    if result.pushed {
        lines.push(i18n::text(UiMessage::GitPushedSummary));
    } else if !requested_push && result.committed {
        lines.push(i18n::text(UiMessage::GitPushNotRequestedSummary));
    }
    if let Some(stage) = &result.error_stage {
        lines.push(i18n::text(UiMessage::GitStopStage {
            stage: stage.clone(),
        }));
    }
    if let Some(error) = &result.error {
        let mapped = i18n::git_payload_error(error);
        lines.push(i18n::text(mapped.message));
    }
    if lines.is_empty() {
        i18n::text(UiMessage::GitNoChangesExecuted)
    } else {
        lines.join("\n")
    }
}

fn publish_operation_log(result: &PublishResult) -> String {
    let mut log = operation_log(&result.report);
    if let Some(error) = &result.error {
        if !log.is_empty() {
            log.push_str("\n\n");
        }
        // The translated summary stays concise; the structured fallback remains
        // available in the expandable execution details for diagnosis.
        log.push_str(error.fallback());
    }
    log
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
        assert!(summary.contains("1"));
        assert!(summary.contains("abc123"));
        assert!(summary.contains("push"));
        assert!(summary.contains(&i18n::text(UiMessage::GitErrorPushRejected)));
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
}
