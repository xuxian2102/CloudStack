use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cloudstack_core::services::recent::{self, RecentProject};

use super::{open_project, show_error, EditorState, Widgets};
use crate::tasks;

fn app_data_dir() -> PathBuf {
    #[cfg(feature = "e2e")]
    if let Some(path) = std::env::var_os("CLOUDSTACK_E2E_DATA_DIR") {
        return PathBuf::from(path);
    }
    gtk::glib::user_data_dir().join(crate::APPLICATION_ID)
}

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
    let complete_widgets = widgets.clone();
    let complete_state = Rc::clone(state);
    tasks::run(
        move || recent::remove(&app_data_dir(), &root),
        move |result| match result {
            Ok(projects) => bind(&complete_widgets, &complete_state, &projects),
            Err(error) => show_error(&complete_widgets, &format!("移除最近项目失败：{error}")),
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
    let complete_widgets = widgets.clone();
    let complete_state = Rc::clone(state);
    tasks::run(
        move || recent::set_pinned(&app_data_dir(), &root, pinned),
        move |result| match result {
            Ok(projects) => bind(&complete_widgets, &complete_state, &projects),
            Err(error) => show_error(&complete_widgets, &format!("更新固定状态失败：{error}")),
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
