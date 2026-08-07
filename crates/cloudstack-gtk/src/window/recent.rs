use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cloudstack_application::recent::{
    choose_document_to_restore, choose_project_to_reopen, DocumentRestoreInput,
    LastDocumentWriteAction, LastDocumentWriter, ProjectReopenInput,
};
use cloudstack_core::services::recent::{self, RecentProject};
use cloudstack_core::services::settings;

use super::{app_data_dir, load_document, open_project, show_user_facing, EditorState, Widgets};
use crate::i18n::{self, UiMessage};
use crate::tasks;

/// 冷启动时调用一次，填充欢迎页的最近项目列表。
pub(super) fn load_async(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || recent::load(&app_data_dir()),
        move |result| {
            let projects = result.unwrap_or_else(|error| {
                log::warn!("加载最近项目列表失败：{error}");
                Vec::new()
            });
            bind(&widgets, &state, &projects);
        },
    );
}

/// 冷启动时调用一次。读设置判断要不要自动重开，读最近列表按 last_opened_ms
/// 找真正最近打开的一个（不能直接拿列表第一项——固定的项目会排在前面，不代表
/// 它最近打开）。
pub(super) fn maybe_reopen_last_project(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            let settings = settings::load(&app_data_dir());
            let projects = recent::load(&app_data_dir()).unwrap_or_else(|error| {
                log::warn!("加载最近项目列表失败：{error}");
                Vec::new()
            });
            Ok((settings, projects))
        },
        move |result: Result<_, cloudstack_core::AppError>| {
            let Ok((settings, projects)) = result else {
                return;
            };
            let project_to_reopen = {
                let state = state.borrow();
                choose_project_to_reopen(ProjectReopenInput {
                    enabled: settings.auto_reopen_last_project,
                    // e2e 强制打开的项目、或者用户手速极快已经点开了别的项目，都不要抢。
                    has_open_project: state.session.project().is_some(),
                    busy: state.session.busy(),
                    projects: &projects,
                })
            };
            if let Some(root) = project_to_reopen {
                open_project(&widgets, &state, &root);
            }
        },
    );
}

thread_local! {
    static LAST_DOCUMENT_WRITER: RefCell<LastDocumentWriter> =
        RefCell::new(LastDocumentWriter::default());
}

/// 每次文章被展示出来时调用：记住这是这个项目最后打开的文章。`LastDocumentWriter`
/// 负责单写者 + 最新值合并的判定，这里只执行它决定的 effect。
pub(super) fn touch_last_document(project_root: &Path, document_id: &str) {
    let action =
        LAST_DOCUMENT_WRITER.with(|writer| writer.borrow_mut().record(project_root, document_id));
    dispatch_last_document_write(action);
}

fn dispatch_last_document_write(action: LastDocumentWriteAction) {
    let LastDocumentWriteAction::Persist(write) = action else {
        return;
    };
    let generation = write.generation;
    tasks::run(
        move || recent::set_last_document(&app_data_dir(), &write.project_root, &write.document_id),
        move |result| {
            if let Err(error) = result {
                log::warn!("记录最后打开的文章失败：{error}");
            }
            let next =
                LAST_DOCUMENT_WRITER.with(|writer| writer.borrow_mut().complete_write(generation));
            dispatch_last_document_write(next);
        },
    );
}

/// 项目成功打开后调用一次。
pub(super) fn maybe_reopen_last_document(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    project_root: PathBuf,
) {
    let expected_epoch = state.borrow().session.document_epoch();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    let lookup_root = project_root.clone();
    tasks::run(
        move || {
            let settings = settings::load(&app_data_dir());
            let projects = recent::load(&app_data_dir()).unwrap_or_else(|error| {
                log::warn!("加载最近项目列表失败：{error}");
                Vec::new()
            });
            let last_document_id = projects
                .into_iter()
                .find(|project| project.root == lookup_root)
                .and_then(|project| project.last_document_id);
            Ok((settings, last_document_id))
        },
        move |result: Result<_, cloudstack_core::AppError>| {
            let Ok((settings, last_document_id)) = result else {
                return;
            };
            let to_restore = {
                let state = state.borrow();
                choose_document_to_restore(DocumentRestoreInput {
                    enabled: settings.restore_last_document_on_open,
                    expected_project_root: &project_root,
                    current_project_root: state
                        .session
                        .project()
                        .map(|context| context.root.as_path()),
                    expected_document_epoch: expected_epoch,
                    current_document_epoch: state.session.document_epoch(),
                    document_already_selected: state.session.document().is_some(),
                    posts: state.session.posts(),
                    last_document_id: last_document_id.as_deref(),
                })
            };
            if let Some(post_id) = to_restore {
                load_document(&widgets, &state, &post_id);
            }
        },
    );
}

/// 打开项目成功后调用：不刷新 UI（欢迎页此时已不可见），失败只记日志，不影响
/// 本次打开。
pub(super) fn touch(project_root: &Path) {
    let root = project_root.to_path_buf();
    tasks::run(
        move || recent::touch(&app_data_dir(), &root),
        |result| {
            if let Err(error) = result {
                log::warn!("更新最近项目列表失败：{error}");
            }
        },
    );
}

fn remove_async(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, project_root: &Path) {
    let root = project_root.to_path_buf();
    let display_path = root.display().to_string();
    let complete_widgets = widgets.clone();
    let complete_state = Rc::clone(state);
    tasks::run(
        move || recent::remove(&app_data_dir(), &root),
        move |result| match result {
            Ok(projects) => bind(&complete_widgets, &complete_state, &projects),
            Err(error) => show_user_facing(
                &complete_widgets,
                i18n::user_facing_message(
                    UiMessage::RecentProjectRemoveFailed { path: display_path },
                    error.to_string(),
                ),
            ),
        },
    );
}

fn set_pinned_async(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    project_root: &Path,
    pinned: bool,
) {
    let root = project_root.to_path_buf();
    let display_path = root.display().to_string();
    let complete_widgets = widgets.clone();
    let complete_state = Rc::clone(state);
    tasks::run(
        move || recent::set_pinned(&app_data_dir(), &root, pinned),
        move |result| match result {
            Ok(projects) => bind(&complete_widgets, &complete_state, &projects),
            Err(error) => show_user_facing(
                &complete_widgets,
                i18n::user_facing_message(
                    UiMessage::RecentProjectPinFailed { path: display_path },
                    error.to_string(),
                ),
            ),
        },
    );
}

/// (Re)绑定欢迎页的最近列表。WelcomePage 自身从不持有 Widgets/EditorState——
/// 这个自由函数才是两者之间唯一的耦合点，每次调用都重建回调闭包。
fn bind(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, projects: &[RecentProject]) {
    let open_widgets = widgets.clone();
    let open_state = Rc::clone(state);
    let remove_widgets = widgets.clone();
    let remove_state = Rc::clone(state);
    let pin_widgets = widgets.clone();
    let pin_state = Rc::clone(state);
    widgets.welcome_page.set_recent(
        projects,
        move |path: &Path| open_project(&open_widgets, &open_state, path),
        move |path: &Path| remove_async(&remove_widgets, &remove_state, path),
        move |path: &Path, pinned: bool| set_pinned_async(&pin_widgets, &pin_state, path, pinned),
    );
}
