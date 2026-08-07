use std::cell::RefCell;
#[cfg(feature = "e2e")]
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use cloudstack_application::drafts::{
    self, classify_recovery, BatchSaveReport, CurrentDraftTargetInput, DiscardReport, DraftAction,
    DraftCleanupWarning, DraftCompletion, DraftCoordinator, DraftOperation, DraftRecoveryDecision,
    DraftRecoveryEligibilityInput, DraftStorage,
};
use cloudstack_core::model::{DraftDocument, PostDocument, ProjectContext};

use super::{set_busy, show_error, sync_controls, toast, EditorState, Widgets};
use crate::i18n::{self, UiMessage};
use crate::tasks;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(700);

#[derive(Default)]
pub(super) struct DraftQueue {
    coordinator: DraftCoordinator,
    timer: Option<gtk::glib::SourceId>,
}

fn draft_storage() -> DraftStorage {
    #[cfg(feature = "e2e")]
    if let Some(path) = std::env::var_os("CLOUDSTACK_E2E_DATA_DIR") {
        return DraftStorage::new(
            PathBuf::from(path),
            std::env::var_os("CLOUDSTACK_E2E_LEGACY_DATA_DIR").map(PathBuf::from),
        );
    }

    let user_data = gtk::glib::user_data_dir();
    DraftStorage::new(
        user_data.join(crate::APPLICATION_ID),
        Some(user_data.join(crate::LEGACY_APPLICATION_ID)),
    )
}

pub(super) fn schedule(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    cancel_timer(state);
    let widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let source = gtk::glib::timeout_add_local_once(AUTOSAVE_DELAY, move || {
        callback_state.borrow_mut().draft_queue.timer = None;
        enqueue_current_snapshot(&widgets, &callback_state);
    });
    state.borrow_mut().draft_queue.timer = Some(source);
}

pub(super) fn flush_current(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    cancel_timer(state);
    enqueue_current_snapshot(widgets, state);
}

pub(super) fn inspect_loaded_document(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: ProjectContext,
    document: PostDocument,
    epoch: u64,
) {
    enqueue(
        widgets,
        state,
        DraftOperation::Read {
            storage: draft_storage(),
            context,
            document,
            epoch,
        },
    );
}

pub(super) fn delete_for_post(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: ProjectContext,
    post_id: String,
) {
    enqueue_delete(widgets, state, context, post_id);
}

pub(super) fn save_and_close(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    cancel_timer(state);
    let (context, mut documents) = {
        let state = state.borrow();
        let Some(context) = state.session.project() else {
            return widgets.window.close();
        };
        (
            context.clone(),
            state
                .session
                .unsaved_documents()
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    documents.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if documents.is_empty() {
        set_busy(widgets, state, false, "");
        widgets.window.close();
        return;
    }

    let busy_message = i18n::text(UiMessage::SavingUnsavedStatus);
    set_busy(widgets, state, true, &busy_message);
    enqueue(
        widgets,
        state,
        DraftOperation::SaveAndClose {
            storage: draft_storage(),
            context,
            documents,
        },
    );
}

/// 保存当前会话中的全部未保存文章，但保留窗口打开。Git 主按钮在有未保存文章
/// 时调用这个入口，保存完成后按钮会恢复为真正的仓库动作，用户再明确执行一次。
pub(super) fn save_all(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    cancel_timer(state);
    let (context, mut documents) = {
        let state = state.borrow();
        let Some(context) = state.session.project() else {
            return;
        };
        (
            context.clone(),
            state
                .session
                .unsaved_documents()
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    documents.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if documents.is_empty() {
        return;
    }

    let busy_message = i18n::text(UiMessage::SavingUnsavedStatus);
    set_busy(widgets, state, true, &busy_message);
    enqueue(
        widgets,
        state,
        DraftOperation::SaveAll {
            storage: draft_storage(),
            context,
            documents,
        },
    );
}

pub(super) fn discard_and_close(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    cancel_timer(state);
    let (context, mut documents) = {
        let state = state.borrow();
        let Some(context) = state.session.project() else {
            return widgets.window.close();
        };
        (
            context.clone(),
            state
                .session
                .unsaved_documents()
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    documents.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if documents.is_empty() {
        set_busy(widgets, state, false, "");
        widgets.window.close();
        return;
    }

    if let Err(error) = state.borrow_mut().pending_assets.discard_all() {
        log::warn!("退出时清理待提交图片失败：{error}");
    }
    let busy_message = i18n::text(UiMessage::DiscardingUnsavedStatus);
    set_busy(widgets, state, true, &busy_message);
    enqueue(
        widgets,
        state,
        DraftOperation::DiscardAndClose {
            storage: draft_storage(),
            context,
            documents,
        },
    );
}

fn cancel_timer(state: &Rc<RefCell<EditorState>>) {
    if let Some(source) = state.borrow_mut().draft_queue.timer.take() {
        source.remove();
    }
}

fn enqueue_current_snapshot(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let (context, document, raw_frontmatter) = {
        let state = state.borrow();
        if !state.session.dirty() {
            return;
        }
        let (Some(context), Some(document)) = (state.session.project(), state.session.document())
        else {
            return;
        };
        (
            context.clone(),
            document.clone(),
            document.raw_frontmatter.clone(),
        )
    };
    let text = widgets.buffer.text(
        &widgets.buffer.start_iter(),
        &widgets.buffer.end_iter(),
        true,
    );
    enqueue(
        widgets,
        state,
        DraftOperation::Write {
            storage: draft_storage(),
            context,
            post_id: document.id,
            raw_frontmatter,
            body: text.to_string(),
            base_revision: document.revision,
        },
    );
}

fn enqueue_delete(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: ProjectContext,
    post_id: String,
) {
    enqueue(
        widgets,
        state,
        DraftOperation::Delete {
            storage: draft_storage(),
            context,
            post_id,
        },
    );
}

fn enqueue(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, operation: DraftOperation) {
    let action = state
        .borrow_mut()
        .draft_queue
        .coordinator
        .enqueue(operation);
    dispatch_action(widgets, state, action);
}

fn dispatch_action(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, action: DraftAction) {
    let DraftAction::Execute(task) = action else {
        return;
    };

    let ticket = task.ticket;
    let closes_window = task.operation.closes_window();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || Ok(task.operation.execute()),
        move |result| {
            let stop_queue = match result {
                Ok(completion) => handle_completion(&widgets, &state, completion),
                Err(error) => {
                    if closes_window {
                        set_busy(&widgets, &state, false, "");
                    }
                    super::show_user_facing_error(&widgets, &error);
                    false
                }
            };

            let next = state
                .borrow_mut()
                .draft_queue
                .coordinator
                .complete(ticket, stop_queue);
            dispatch_action(&widgets, &state, next);
        },
    );
}

/// 返回 true 表示窗口已进入关闭流程，不再启动后续任务。
fn handle_completion(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    completion: DraftCompletion,
) -> bool {
    match completion {
        DraftCompletion::Written {
            context_root,
            post_id,
            result,
        } => {
            if let Err(error) = result {
                let is_current = is_current_post(state, &context_root, &post_id);
                if is_current {
                    super::show_user_facing(
                        widgets,
                        i18n::user_facing_message(
                            UiMessage::DraftWriteFailed {
                                path: post_id.clone(),
                            },
                            error.to_string(),
                        ),
                    );
                }
            }
        }
        DraftCompletion::Read {
            context,
            document,
            epoch,
            result,
        } => match result {
            Ok(Some(draft)) if can_offer_recovery(state, &context.root, &document.id, epoch) => {
                match classify_recovery(&document, &draft) {
                    DraftRecoveryDecision::DeleteRedundant => {
                        delete_for_post(widgets, state, *context, document.id);
                    }
                    DraftRecoveryDecision::Offer {
                        disk_changed_since_draft,
                    } => {
                        show_recovery_dialog(
                            widgets,
                            state,
                            *context,
                            document,
                            draft,
                            epoch,
                            disk_changed_since_draft,
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(error) if can_offer_recovery(state, &context.root, &document.id, epoch) => {
                super::show_user_facing(
                    widgets,
                    i18n::user_facing_message(
                        UiMessage::DraftRecoveryFailed {
                            path: document.relative_path.clone(),
                        },
                        error.to_string(),
                    ),
                );
            }
            Err(_) => {}
        },
        DraftCompletion::Deleted {
            context_root,
            post_id,
            result,
        } => {
            if let Err(error) = result {
                let is_current = is_current_post(state, &context_root, &post_id);
                if is_current {
                    super::show_user_facing(
                        widgets,
                        i18n::user_facing_message(
                            UiMessage::DraftDeleteFailed {
                                path: post_id.clone(),
                            },
                            error.to_string(),
                        ),
                    );
                }
            }
        }
        DraftCompletion::BatchSaved {
            report,
            close_window,
        } => return complete_batch_save(widgets, state, report, close_window),
        DraftCompletion::Discarded(report) => return complete_discard(widgets, state, report),
    }
    false
}

fn complete_batch_save(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    report: BatchSaveReport,
    close_window: bool,
) -> bool {
    let saved_ids = report
        .saved
        .iter()
        .map(|document| document.id.clone())
        .collect::<Vec<_>>();
    let project_root = state
        .borrow()
        .session
        .project()
        .map(|context| context.root.clone());
    {
        let mut editor_state = state.borrow_mut();
        for document in &report.saved {
            if let Some(project_root) = &project_root {
                if let Err(error) = editor_state.pending_assets.reconcile_saved_post(
                    project_root,
                    &document.id,
                    &document.body,
                ) {
                    log::warn!("保存后清理待提交图片失败：{error}");
                }
            }
        }
        editor_state.session.apply_batch_saved(&report.saved);
    }
    for post_id in &saved_ids {
        super::update_post_marker(widgets, state, post_id);
    }
    if !saved_ids.is_empty() {
        super::git_panel::refresh(widgets, state);
    }

    if report.failed.is_empty() {
        for warning in &report.cleanup_warnings {
            log::warn!("{}", cleanup_warning_text(warning));
        }
        set_busy(widgets, state, false, "");
        if close_window {
            widgets.window.close();
            return true;
        }
        toast(widgets, &i18n::text(UiMessage::BatchSaveSuccessContinueGit));
        return false;
    }

    set_busy(widgets, state, false, "");
    let details = report
        .failed
        .iter()
        .map(|failure| format!("{}：{}", failure.relative_path, failure.error))
        .collect::<Vec<_>>()
        .join("\n");
    if !report.cleanup_warnings.is_empty() {
        let warnings = report
            .cleanup_warnings
            .iter()
            .map(cleanup_warning_text)
            .collect::<Vec<_>>()
            .join("；");
        log::warn!("{warnings}");
    }
    show_error(widgets, &i18n::text(UiMessage::BatchSaveFailed { details }));
    false
}

fn cleanup_warning_text(warning: &DraftCleanupWarning) -> String {
    format!(
        "{}：文章已保存，但清理自动恢复草稿失败：{}",
        warning.relative_path, warning.error
    )
}

fn complete_discard(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    report: DiscardReport,
) -> bool {
    let discarded_ids = report.discarded.clone();
    state
        .borrow_mut()
        .session
        .discard_unsaved_documents(&discarded_ids);
    for post_id in &discarded_ids {
        super::update_post_marker(widgets, state, post_id);
    }

    if report.failed.is_empty() {
        set_busy(widgets, state, false, "");
        widgets.window.close();
        return true;
    }

    set_busy(widgets, state, false, "");
    let details = report
        .failed
        .iter()
        .map(|failure| format!("{}：{}", failure.relative_path, failure.error))
        .collect::<Vec<_>>()
        .join("\n");
    show_error(widgets, &i18n::text(UiMessage::DiscardFailed { details }));
    false
}

fn is_current_post(
    state: &Rc<RefCell<EditorState>>,
    context_root: &std::path::Path,
    post_id: &str,
) -> bool {
    let state = state.borrow();
    drafts::is_current_draft_target(CurrentDraftTargetInput {
        expected_project_root: context_root,
        current_project_root: state
            .session
            .project()
            .map(|context| context.root.as_path()),
        expected_post_id: post_id,
        current_post_id: state
            .session
            .document()
            .map(|document| document.id.as_str()),
    })
}

fn can_offer_recovery(
    state: &Rc<RefCell<EditorState>>,
    project_root: &std::path::Path,
    post_id: &str,
    epoch: u64,
) -> bool {
    let state = state.borrow();
    drafts::can_offer_recovery(DraftRecoveryEligibilityInput {
        busy: state.session.busy(),
        dirty: state.session.dirty(),
        expected_project_root: project_root,
        current_project_root: state
            .session
            .project()
            .map(|context| context.root.as_path()),
        expected_post_id: post_id,
        current_post_id: state
            .session
            .document()
            .map(|document| document.id.as_str()),
        expected_epoch: epoch,
        current_epoch: state.session.document_epoch(),
    })
}

fn show_recovery_dialog(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: ProjectContext,
    document: PostDocument,
    draft: DraftDocument,
    epoch: u64,
    disk_changed_since_draft: bool,
) {
    let body = if disk_changed_since_draft {
        i18n::text(UiMessage::DraftRecoveryDiskChanged {
            path: document.relative_path.clone(),
        })
    } else {
        i18n::text(UiMessage::DraftRecoveryAvailable {
            path: document.relative_path.clone(),
        })
    };
    let dialog = adw::AlertDialog::builder()
        .heading(i18n::text(UiMessage::DraftRecoveryHeading))
        .body(body)
        .default_response("restore")
        .close_response("later")
        .build();
    dialog.add_responses(&[
        ("disk", i18n::text(UiMessage::UseDiskVersion).as_str()),
        ("restore", i18n::text(UiMessage::RecoverDraft).as_str()),
    ]);
    dialog.set_response_appearance("disk", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);

    let restore_widgets = widgets.clone();
    let restore_state = Rc::clone(state);
    let restore_document = document.clone();
    let restore_root = context.root.clone();
    dialog.connect_response(Some("restore"), move |_, _| {
        if !can_offer_recovery(&restore_state, &restore_root, &restore_document.id, epoch) {
            return;
        }
        {
            let mut state = restore_state.borrow_mut();
            state.loading_buffer = true;
            state
                .session
                .set_current_frontmatter(draft.raw_frontmatter.clone());
        }
        restore_widgets.buffer.set_text(&draft.body);
        restore_state.borrow_mut().loading_buffer = false;
        // mark_document_dirty() 会设置 dirty、推进 edit_generation、把当前
        // buffer（已经是 draft.body）连同上面刚设置的 frontmatter 一起写进
        // unsaved_documents 快照，不需要在这里单独再设一次 dirty。
        super::mark_document_dirty(&restore_widgets, &restore_state);
        restore_widgets
            .status_label
            .set_label(&i18n::text(UiMessage::RecoveredDraftStatus {
                path: restore_document.relative_path.clone(),
            }));
        restore_widgets.preview.schedule(draft.body.clone(), true);
        sync_controls(&restore_widgets, &restore_state);
        super::frontmatter::refresh(&restore_widgets, &restore_state);
        restore_widgets.editor.grab_focus();
    });

    let disk_widgets = widgets.clone();
    let disk_state = Rc::clone(state);
    dialog.connect_response(Some("disk"), move |_, _| {
        if can_offer_recovery(&disk_state, &context.root, &document.id, epoch) {
            delete_for_post(
                &disk_widgets,
                &disk_state,
                context.clone(),
                document.id.clone(),
            );
        }
    });
    dialog.present(Some(&widgets.window));
}
