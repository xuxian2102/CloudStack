use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use cloudstack_core::model::{DraftDocument, PostDocument, ProjectContext};
use cloudstack_core::services::{drafts, posts};
use cloudstack_core::AppError;

use super::{set_busy, show_error, sync_controls, EditorState, Widgets};
use crate::tasks;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(700);

#[derive(Debug, Clone)]
struct DraftStorage {
    primary: PathBuf,
    legacy: Option<PathBuf>,
}

impl DraftStorage {
    fn read(
        &self,
        context: &ProjectContext,
        post_id: &str,
    ) -> Result<Option<DraftDocument>, AppError> {
        if let Some(draft) = drafts::read(&self.primary, context, post_id)? {
            return Ok(Some(draft));
        }
        match &self.legacy {
            Some(legacy) => drafts::read(legacy, context, post_id),
            None => Ok(None),
        }
    }

    fn write(
        &self,
        context: &ProjectContext,
        post_id: &str,
        raw_frontmatter: Option<String>,
        body: String,
        base_revision: String,
    ) -> Result<(), AppError> {
        drafts::write(
            &self.primary,
            context,
            post_id,
            raw_frontmatter,
            body,
            base_revision,
        )?;
        if let Some(legacy) = &self.legacy {
            drafts::delete(legacy, context, post_id)?;
        }
        Ok(())
    }

    fn delete(&self, context: &ProjectContext, post_id: &str) -> Result<(), AppError> {
        // 两次删除都必须执行，避免一个目录的错误令另一个目录留下会再次出现的旧草稿。
        let primary_result = drafts::delete(&self.primary, context, post_id);
        let legacy_result = self
            .legacy
            .as_ref()
            .map_or(Ok(()), |legacy| drafts::delete(legacy, context, post_id));
        primary_result.and(legacy_result)
    }
}

pub(super) struct BatchFailure {
    pub relative_path: String,
    pub error: String,
}

pub(super) struct BatchSaveReport {
    pub saved: Vec<PostDocument>,
    pub failed: Vec<BatchFailure>,
    pub cleanup_warnings: Vec<String>,
}

pub(super) struct DiscardReport {
    pub discarded: Vec<String>,
    pub failed: Vec<BatchFailure>,
}

#[derive(Default)]
pub(super) struct DraftQueue {
    active: bool,
    pending: VecDeque<Operation>,
    timer: Option<gtk::glib::SourceId>,
}

enum Operation {
    Write {
        storage: DraftStorage,
        context: ProjectContext,
        post_id: String,
        raw_frontmatter: Option<String>,
        body: String,
        base_revision: String,
    },
    Read {
        storage: DraftStorage,
        context: ProjectContext,
        document: PostDocument,
        epoch: u64,
    },
    Delete {
        storage: DraftStorage,
        context: ProjectContext,
        post_id: String,
    },
    SaveAndClose {
        storage: DraftStorage,
        context: ProjectContext,
        documents: Vec<PostDocument>,
    },
    DiscardAndClose {
        storage: DraftStorage,
        context: ProjectContext,
        documents: Vec<PostDocument>,
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
        result: Result<(), AppError>,
    },
    BatchSaved(BatchSaveReport),
    Discarded(DiscardReport),
}

impl Operation {
    fn closes_window(&self) -> bool {
        matches!(
            self,
            Self::SaveAndClose { .. } | Self::DiscardAndClose { .. }
        )
    }

    fn execute(self) -> Completion {
        match self {
            Self::Write {
                storage,
                context,
                post_id,
                raw_frontmatter,
                body,
                base_revision,
            } => {
                let result =
                    storage.write(&context, &post_id, raw_frontmatter, body, base_revision);
                Completion::Written { post_id, result }
            }
            Self::Read {
                storage,
                context,
                document,
                epoch,
            } => {
                let result = storage.read(&context, &document.id);
                Completion::Read {
                    context: Box::new(context),
                    document,
                    epoch,
                    result,
                }
            }
            Self::Delete {
                storage,
                context,
                post_id,
            } => {
                let result = storage.delete(&context, &post_id);
                Completion::Deleted { post_id, result }
            }
            Self::SaveAndClose {
                storage,
                context,
                documents,
            } => Completion::BatchSaved(save_documents(&storage, &context, documents)),
            Self::DiscardAndClose {
                storage,
                context,
                documents,
            } => Completion::Discarded(discard_documents(&storage, &context, documents)),
        }
    }
}

fn save_documents(
    storage: &DraftStorage,
    context: &ProjectContext,
    documents: Vec<PostDocument>,
) -> BatchSaveReport {
    let mut report = BatchSaveReport {
        saved: Vec::new(),
        failed: Vec::new(),
        cleanup_warnings: Vec::new(),
    };
    for document in documents {
        match posts::write_post(
            context,
            &document.id,
            document.raw_frontmatter.as_deref(),
            &document.body,
            &document.revision,
        ) {
            Ok(revision) => {
                let mut saved = document;
                saved.revision = revision;
                if let Err(error) = storage.delete(context, &saved.id) {
                    report.cleanup_warnings.push(format!(
                        "{}：文章已保存，但清理自动恢复草稿失败：{error}",
                        saved.relative_path
                    ));
                }
                report.saved.push(saved);
            }
            Err(error) => report.failed.push(BatchFailure {
                relative_path: document.relative_path,
                error: error.to_string(),
            }),
        }
    }
    report
}

fn discard_documents(
    storage: &DraftStorage,
    context: &ProjectContext,
    documents: Vec<PostDocument>,
) -> DiscardReport {
    let mut report = DiscardReport {
        discarded: Vec::new(),
        failed: Vec::new(),
    };
    for document in documents {
        match storage.delete(context, &document.id) {
            Ok(()) => report.discarded.push(document.id),
            Err(error) => report.failed.push(BatchFailure {
                relative_path: document.relative_path,
                error: error.to_string(),
            }),
        }
    }
    report
}

fn draft_storage() -> DraftStorage {
    #[cfg(feature = "e2e")]
    if let Some(path) = std::env::var_os("CLOUDSTACK_E2E_DATA_DIR") {
        return DraftStorage {
            primary: PathBuf::from(path),
            legacy: std::env::var_os("CLOUDSTACK_E2E_LEGACY_DATA_DIR").map(PathBuf::from),
        };
    }

    let user_data = gtk::glib::user_data_dir();
    DraftStorage {
        primary: user_data.join(crate::APPLICATION_ID),
        legacy: Some(user_data.join(crate::LEGACY_APPLICATION_ID)),
    }
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
        let Some(context) = &state.project else {
            return widgets.window.close();
        };
        (
            context.clone(),
            state
                .unsaved_documents
                .values()
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

    set_busy(widgets, state, true, "正在保存未保存文章…");
    enqueue(
        widgets,
        state,
        Operation::SaveAndClose {
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
        let Some(context) = &state.project else {
            return widgets.window.close();
        };
        (
            context.clone(),
            state
                .unsaved_documents
                .values()
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
    set_busy(widgets, state, true, "正在放弃未保存文章…");
    enqueue(
        widgets,
        state,
        Operation::DiscardAndClose {
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
        Operation::Delete {
            storage: draft_storage(),
            context,
            post_id,
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
        Completion::Deleted { post_id, result } => {
            if let Err(error) = result {
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
        Completion::BatchSaved(report) => return complete_batch_save(widgets, state, report),
        Completion::Discarded(report) => return complete_discard(widgets, state, report),
    }
    false
}

fn complete_batch_save(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    report: BatchSaveReport,
) -> bool {
    let saved_ids = report
        .saved
        .iter()
        .map(|document| document.id.clone())
        .collect::<Vec<_>>();
    let project_root = state
        .borrow()
        .project
        .as_ref()
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
            editor_state.unsaved_documents.remove(&document.id);
            if editor_state
                .document
                .as_ref()
                .is_some_and(|current| current.id == document.id)
            {
                editor_state.document = Some(document.clone());
            }
        }
        let current_id = editor_state
            .document
            .as_ref()
            .map(|document| document.id.clone());
        editor_state.dirty = current_id
            .as_deref()
            .is_some_and(|post_id| editor_state.unsaved_documents.contains_key(post_id));
    }
    for post_id in &saved_ids {
        super::update_post_marker(widgets, state, post_id);
    }
    if !saved_ids.is_empty() {
        super::git_panel::refresh(widgets, state);
    }

    if report.failed.is_empty() {
        for warning in report.cleanup_warnings {
            log::warn!("{warning}");
        }
        set_busy(widgets, state, false, "");
        widgets.window.close();
        return true;
    }

    set_busy(widgets, state, false, "");
    let mut message = String::from("以下文章保存失败，窗口保持打开：");
    for failure in &report.failed {
        message.push_str(&format!("\n{}：{}", failure.relative_path, failure.error));
    }
    if !report.cleanup_warnings.is_empty() {
        log::warn!("{}", report.cleanup_warnings.join("；"));
    }
    show_error(widgets, &message);
    false
}

fn complete_discard(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    report: DiscardReport,
) -> bool {
    let discarded_ids = report.discarded.clone();
    {
        let mut editor_state = state.borrow_mut();
        for post_id in &discarded_ids {
            editor_state.unsaved_documents.remove(post_id);
        }
        let current_id = editor_state
            .document
            .as_ref()
            .map(|document| document.id.clone());
        editor_state.dirty = current_id
            .as_deref()
            .is_some_and(|post_id| editor_state.unsaved_documents.contains_key(post_id));
    }
    for post_id in &discarded_ids {
        super::update_post_marker(widgets, state, post_id);
    }

    if report.failed.is_empty() {
        set_busy(widgets, state, false, "");
        widgets.window.close();
        return true;
    }

    set_busy(widgets, state, false, "");
    let mut message = String::from("以下文章的自动恢复草稿未能清理，窗口保持打开：");
    for failure in &report.failed {
        message.push_str(&format!("\n{}：{}", failure.relative_path, failure.error));
    }
    show_error(widgets, &message);
    false
}

fn can_offer_recovery(state: &Rc<RefCell<EditorState>>, post_id: &str, epoch: u64) -> bool {
    let state = state.borrow();
    !state.busy
        && !state.dirty
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
        super::mark_document_dirty(&restore_widgets, &restore_state);
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

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::model::ProjectConfig;

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        ProjectContext,
        DraftStorage,
    ) {
        let project = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "disk\n").unwrap();
        let context = ProjectContext {
            root: root.clone(),
            content_root: root.clone(),
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig::default(),
        };
        let storage = DraftStorage {
            primary: app_data.path().join("dev.xuxian.cloudstack"),
            legacy: Some(app_data.path().join("dev.xuxian.blogeditor")),
        };
        (project, app_data, context, storage)
    }

    fn write_at(path: &std::path::Path, context: &ProjectContext, body: &str) {
        drafts::write(
            path,
            context,
            "a.md",
            None,
            body.to_owned(),
            "revision".into(),
        )
        .unwrap();
    }

    #[test]
    fn primary_draft_wins_and_legacy_is_the_fallback() {
        let (_project, _app_data, context, storage) = fixture();
        let legacy = storage.legacy.as_ref().unwrap();
        write_at(legacy, &context, "legacy");
        assert_eq!(
            storage.read(&context, "a.md").unwrap().unwrap().body,
            "legacy"
        );

        write_at(&storage.primary, &context, "primary");
        assert_eq!(
            storage.read(&context, "a.md").unwrap().unwrap().body,
            "primary"
        );
    }

    #[test]
    fn writing_primary_removes_the_matching_legacy_draft() {
        let (_project, _app_data, context, storage) = fixture();
        let legacy = storage.legacy.as_ref().unwrap();
        write_at(legacy, &context, "legacy");

        storage
            .write(&context, "a.md", None, "primary".into(), "revision".into())
            .unwrap();

        assert!(drafts::read(legacy, &context, "a.md").unwrap().is_none());
        assert_eq!(
            storage.read(&context, "a.md").unwrap().unwrap().body,
            "primary"
        );
    }

    #[test]
    fn deleting_a_draft_clears_both_storage_locations() {
        let (_project, _app_data, context, storage) = fixture();
        let legacy = storage.legacy.as_ref().unwrap();
        write_at(&storage.primary, &context, "primary");
        write_at(legacy, &context, "legacy");

        storage.delete(&context, "a.md").unwrap();

        assert!(drafts::read(&storage.primary, &context, "a.md")
            .unwrap()
            .is_none());
        assert!(drafts::read(legacy, &context, "a.md").unwrap().is_none());
    }

    #[test]
    fn batch_save_writes_each_snapshot_and_clears_its_draft() {
        let (project, _app_data, context, storage) = fixture();
        std::fs::write(context.root.join("b.md"), "old b\n").unwrap();
        let mut first = posts::read_post(&context, "a.md").unwrap();
        let mut second = posts::read_post(&context, "b.md").unwrap();
        first.body = "new a\n".into();
        second.body = "new b\n".into();
        write_at(&storage.primary, &context, "new a\n");

        let report = save_documents(&storage, &context, vec![first, second]);

        assert!(report.failed.is_empty());
        assert_eq!(report.saved.len(), 2);
        assert_eq!(
            std::fs::read_to_string(project.path().join("a.md")).unwrap(),
            "new a\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("b.md")).unwrap(),
            "new b\n"
        );
        assert!(storage.read(&context, "a.md").unwrap().is_none());
    }

    #[test]
    fn batch_save_keeps_external_conflict_as_a_failed_article() {
        let (_project, _app_data, context, storage) = fixture();
        let mut document = posts::read_post(&context, "a.md").unwrap();
        document.body = "edited\n".into();
        std::fs::write(context.root.join("a.md"), "changed elsewhere\n").unwrap();

        let report = save_documents(&storage, &context, vec![document]);

        assert!(report.saved.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].relative_path, "a.md");
        assert!(report.failed[0].error.contains("外部"));
    }

    #[test]
    fn batch_discard_removes_all_recovery_drafts() {
        let (_project, _app_data, context, storage) = fixture();
        write_at(&storage.primary, &context, "discard me");
        let document = posts::read_post(&context, "a.md").unwrap();

        let report = discard_documents(&storage, &context, vec![document]);

        assert_eq!(report.discarded, vec!["a.md"]);
        assert!(report.failed.is_empty());
        assert!(storage.read(&context, "a.md").unwrap().is_none());
    }
}
