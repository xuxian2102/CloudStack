use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cloudstack_core::model::PostSummary;
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
            if !settings.auto_reopen_last_project {
                return;
            }
            // e2e 强制打开的项目、或者用户手速极快已经点开了别的项目，都不要抢。
            if state.borrow().project.is_some() || state.borrow().busy {
                return;
            }
            if let Some(most_recent) = projects.iter().max_by_key(|p| p.last_opened_ms) {
                open_project(&widgets, &state, &most_recent.root);
            }
        },
    );
}

thread_local! {
    static LAST_DOCUMENT_WRITE: RefCell<LastDocumentWrite> = RefCell::new(LastDocumentWrite::default());
}

#[derive(Default)]
struct LastDocumentWrite {
    in_flight: bool,
    pending: Option<(PathBuf, String)>,
}

/// 每次文章被展示出来时调用：记住这是这个项目最后打开的文章。任意时刻最多
/// 一个写入在飞；写入进行中又调用这个函数，只更新"待写入的最新值"，不重新
/// 派发——快速切换几篇文章最终只会落最后一篇，中间被跳过的值不会真的写盘。
pub(super) fn touch_last_document(project_root: &Path, document_id: &str) {
    let should_dispatch = LAST_DOCUMENT_WRITE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.pending = Some((project_root.to_path_buf(), document_id.to_owned()));
        if state.in_flight {
            false
        } else {
            state.in_flight = true;
            true
        }
    });
    if should_dispatch {
        dispatch_last_document_write();
    }
}

fn dispatch_last_document_write() {
    let Some((root, document_id)) =
        LAST_DOCUMENT_WRITE.with(|cell| cell.borrow_mut().pending.take())
    else {
        LAST_DOCUMENT_WRITE.with(|cell| cell.borrow_mut().in_flight = false);
        return;
    };
    tasks::run(
        move || recent::set_last_document(&app_data_dir(), &root, &document_id),
        move |result| {
            if let Err(error) = result {
                log::warn!("记录最后打开的文章失败：{error}");
            }
            let has_more = LAST_DOCUMENT_WRITE.with(|cell| cell.borrow().pending.is_some());
            if has_more {
                dispatch_last_document_write();
            } else {
                LAST_DOCUMENT_WRITE.with(|cell| cell.borrow_mut().in_flight = false);
            }
        },
    );
}

/// 五个条件都满足才真正恢复：开关开着、用户还没手动选任何文章、这仍然是
/// 派发时的那个项目会话（epoch 没变）、项目 root 没变、这篇文章还在最新扫出
/// 来的列表里。
#[allow(clippy::too_many_arguments)]
fn choose_document_to_restore(
    enabled: bool,
    project_still_current: bool,
    no_document_selected: bool,
    epoch_unchanged: bool,
    posts: &[PostSummary],
    last_document_id: Option<&str>,
) -> Option<String> {
    if !enabled || !project_still_current || !no_document_selected || !epoch_unchanged {
        return None;
    }
    let post_id = last_document_id?;
    posts
        .iter()
        .any(|post| post.id == post_id)
        .then(|| post_id.to_owned())
}

/// 项目成功打开后调用一次。
pub(super) fn maybe_reopen_last_document(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    project_root: PathBuf,
) {
    let expected_epoch = state.borrow().document_epoch;
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
            let state_snapshot = state.borrow();
            let project_still_current = state_snapshot
                .project
                .as_ref()
                .is_some_and(|context| context.root == project_root);
            let to_restore = choose_document_to_restore(
                settings.restore_last_document_on_open,
                project_still_current,
                state_snapshot.document.is_none(),
                state_snapshot.document_epoch == expected_epoch,
                &state_snapshot.posts,
                last_document_id.as_deref(),
            );
            drop(state_snapshot);
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

    #[test]
    fn choose_document_to_restore_returns_none_when_disabled() {
        let posts = [post("a.md")];
        assert_eq!(
            choose_document_to_restore(false, true, true, true, &posts, Some("a.md")),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_document_already_selected() {
        let posts = [post("a.md")];
        assert_eq!(
            choose_document_to_restore(true, true, false, true, &posts, Some("a.md")),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_epoch_changed() {
        let posts = [post("a.md")];
        assert_eq!(
            choose_document_to_restore(true, true, true, false, &posts, Some("a.md")),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_none_when_post_no_longer_exists() {
        let posts = [post("a.md")];
        assert_eq!(
            choose_document_to_restore(true, true, true, true, &posts, Some("missing.md")),
            None
        );
    }

    #[test]
    fn choose_document_to_restore_returns_the_remembered_id_when_all_conditions_met() {
        let posts = [post("a.md"), post("b.md")];
        assert_eq!(
            choose_document_to_restore(true, true, true, true, &posts, Some("b.md")),
            Some("b.md".to_string())
        );
    }
}
