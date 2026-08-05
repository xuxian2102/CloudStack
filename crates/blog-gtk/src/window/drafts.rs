use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use blog_editor_core::model::{DraftDocument, PostDocument, ProjectContext};
use blog_editor_core::services::drafts;
use blog_editor_core::AppError;

use super::{set_busy, show_error, sync_controls, EditorState, Widgets};
use crate::tasks;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(700);

#[derive(Default)]
pub(super) struct DraftQueue {
    active: bool,
    pending: VecDeque<Operation>,
    timer: Option<gtk::glib::SourceId>,
}

enum Operation {
    Write {
        app_data_dir: PathBuf,
        context: ProjectContext,
        post_id: String,
        raw_frontmatter: Option<String>,
        body: String,
        base_revision: String,
    },
    Read {
        app_data_dir: PathBuf,
        context: ProjectContext,
        document: PostDocument,
        epoch: u64,
    },
    Delete {
        app_data_dir: PathBuf,
        context: ProjectContext,
        post_id: String,
        close_after: bool,
    },
}

enum Completion {
    Written {
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
        post_id: String,
        close_after: bool,
        result: Result<(), AppError>,
    },
}

impl Operation {
    fn closes_window(&self) -> bool {
        matches!(
            self,
            Self::Delete {
                close_after: true,
                ..
            }
        )
    }

    fn execute(self) -> Completion {
        match self {
            Self::Write {
                app_data_dir,
                context,
                post_id,
                raw_frontmatter,
                body,
                base_revision,
            } => {
                let result = drafts::write(
                    &app_data_dir,
                    &context,
                    &post_id,
                    raw_frontmatter,
                    body,
                    base_revision,
                );
                Completion::Written { post_id, result }
            }
            Self::Read {
                app_data_dir,
                context,
                document,
                epoch,
            } => {
                let result = drafts::read(&app_data_dir, &context, &document.id);
                Completion::Read {
                    context: Box::new(context),
                    document,
                    epoch,
                    result,
                }
            }
            Self::Delete {
                app_data_dir,
                context,
                post_id,
                close_after,
            } => {
                let result = drafts::delete(&app_data_dir, &context, &post_id);
                Completion::Deleted {
                    post_id,
                    close_after,
                    result,
                }
            }
        }
    }
}

fn app_data_dir() -> PathBuf {
    #[cfg(feature = "e2e")]
    if let Some(path) = std::env::var_os("BLOG_EDITOR_E2E_DATA_DIR") {
        return PathBuf::from(path);
    }

    gtk::glib::user_data_dir().join(crate::APPLICATION_ID)
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
        Operation::Read {
            app_data_dir: app_data_dir(),
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
    enqueue_delete(widgets, state, context, post_id, false);
}

pub(super) fn discard_and_close(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    cancel_timer(state);
    let current = {
        let state = state.borrow();
        let (Some(context), Some(document)) = (&state.project, &state.document) else {
            return widgets.window.close();
        };
        (context.clone(), document.id.clone())
    };

    set_busy(widgets, state, true, "正在清理自动恢复草稿…");
    enqueue_delete(widgets, state, current.0, current.1, true);
}

fn cancel_timer(state: &Rc<RefCell<EditorState>>) {
    if let Some(source) = state.borrow_mut().draft_queue.timer.take() {
        source.remove();
    }
}

fn enqueue_current_snapshot(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let (context, document, raw_frontmatter) = {
        let state = state.borrow();
        if !state.dirty {
            return;
        }
        let (Some(context), Some(document)) = (&state.project, &state.document) else {
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
        Operation::Write {
            app_data_dir: app_data_dir(),
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
    close_after: bool,
) {
    enqueue(
        widgets,
        state,
        Operation::Delete {
            app_data_dir: app_data_dir(),
            context,
            post_id,
            close_after,
        },
    );
}

fn enqueue(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, operation: Operation) {
    state.borrow_mut().draft_queue.pending.push_back(operation);
    pump(widgets, state);
}

fn pump(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let operation = {
        let mut state = state.borrow_mut();
        if state.draft_queue.active {
            return;
        }
        let Some(operation) = state.draft_queue.pending.pop_front() else {
            return;
        };
        state.draft_queue.active = true;
        operation
    };
    let closes_window = operation.closes_window();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || Ok(operation.execute()),
        move |result| {
            state.borrow_mut().draft_queue.active = false;
            match result {
                Ok(completion) => {
                    if handle_completion(&widgets, &state, completion) {
                        return;
                    }
                }
                Err(error) => {
                    if closes_window {
                        set_busy(&widgets, &state, false, "");
                    }
                    show_error(&widgets, &error.to_string());
                }
            }
            pump(&widgets, &state);
        },
    );
}

/// 返回 true 表示窗口已进入关闭流程，不再启动后续任务。
fn handle_completion(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    completion: Completion,
) -> bool {
    match completion {
        Completion::Written { post_id, result } => {
            if let Err(error) = result {
                let is_current = state
                    .borrow()
                    .document
                    .as_ref()
                    .is_some_and(|document| document.id == post_id);
                if is_current {
                    show_error(widgets, &format!("自动保存草稿失败：{error}"));
                }
            }
        }
        Completion::Read {
            context,
            document,
            epoch,
            result,
        } => match result {
            Ok(Some(draft)) if can_offer_recovery(state, &document.id, epoch) => {
                if draft.raw_frontmatter == document.raw_frontmatter && draft.body == document.body
                {
                    delete_for_post(widgets, state, *context, document.id);
                } else {
                    show_recovery_dialog(widgets, state, *context, document, draft, epoch);
                }
            }
            Ok(_) => {}
            Err(error) if can_offer_recovery(state, &document.id, epoch) => {
                show_error(widgets, &format!("读取自动恢复草稿失败：{error}"));
            }
            Err(_) => {}
        },
        Completion::Deleted {
            post_id,
            close_after,
            result,
        } => {
            if close_after {
                match result {
                    Ok(()) => {
                        state.borrow_mut().dirty = false;
                        set_busy(widgets, state, false, "");
                        widgets.window.close();
                        return true;
                    }
                    Err(error) => {
                        set_busy(widgets, state, false, "");
                        show_error(widgets, &format!("清理自动恢复草稿失败：{error}"));
                    }
                }
            } else if let Err(error) = result {
                let is_current = state
                    .borrow()
                    .document
                    .as_ref()
                    .is_some_and(|document| document.id == post_id);
                if is_current {
                    show_error(widgets, &format!("清理自动恢复草稿失败：{error}"));
                }
            }
        }
    }
    false
}

fn can_offer_recovery(state: &Rc<RefCell<EditorState>>, post_id: &str, epoch: u64) -> bool {
    let state = state.borrow();
    !state.dirty
        && state.document_epoch == epoch
        && state
            .document
            .as_ref()
            .is_some_and(|document| document.id == post_id)
}

fn show_recovery_dialog(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: ProjectContext,
    document: PostDocument,
    draft: DraftDocument,
    epoch: u64,
) {
    let body = if draft.base_revision == document.revision {
        format!("{} 存在上次未正常保存的编辑内容。", document.relative_path)
    } else {
        format!(
            "{} 存在自动恢复草稿，但磁盘文章之后可能又被修改过。恢复后请检查内容再保存。",
            document.relative_path
        )
    };
    let dialog = adw::AlertDialog::builder()
        .heading("恢复自动保存的草稿？")
        .body(body)
        .default_response("restore")
        .close_response("later")
        .build();
    dialog.add_responses(&[("disk", "使用磁盘版本"), ("restore", "恢复草稿")]);
    dialog.set_response_appearance("disk", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);

    let restore_widgets = widgets.clone();
    let restore_state = Rc::clone(state);
    let restore_document = document.clone();
    dialog.connect_response(Some("restore"), move |_, _| {
        if !can_offer_recovery(&restore_state, &restore_document.id, epoch) {
            return;
        }
        {
            let mut state = restore_state.borrow_mut();
            state.loading_buffer = true;
            if let Some(document) = state.document.as_mut() {
                document.raw_frontmatter.clone_from(&draft.raw_frontmatter);
            }
        }
        restore_widgets.buffer.set_text(&draft.body);
        {
            let mut state = restore_state.borrow_mut();
            state.loading_buffer = false;
            state.dirty = true;
        }
        restore_widgets.status_label.set_label(&format!(
            "{} · 已恢复草稿，未保存",
            restore_document.relative_path
        ));
        restore_widgets.preview.schedule(draft.body.clone(), true);
        sync_controls(&restore_widgets, &restore_state);
        super::frontmatter::refresh(&restore_widgets, &restore_state);
        restore_widgets.editor.grab_focus();
    });

    let disk_widgets = widgets.clone();
    let disk_state = Rc::clone(state);
    dialog.connect_response(Some("disk"), move |_, _| {
        if can_offer_recovery(&disk_state, &document.id, epoch) {
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
