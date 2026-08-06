use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::services::settings::{self, AppSettings, ColorScheme};

use super::{app_data_dir, toast, Widgets};
use crate::tasks;

fn to_adw_color_scheme(scheme: ColorScheme) -> adw::ColorScheme {
    match scheme {
        ColorScheme::System => adw::ColorScheme::Default,
        ColorScheme::Light => adw::ColorScheme::ForceLight,
        ColorScheme::Dark => adw::ColorScheme::ForceDark,
    }
}

fn apply_color_scheme(scheme: ColorScheme) {
    adw::StyleManager::default().set_color_scheme(to_adw_color_scheme(scheme));
}

fn save_async(widgets: &Widgets, settings: AppSettings) {
    let widgets = widgets.clone();
    tasks::run(
        move || settings::save(&app_data_dir(), &settings),
        move |result| {
            if let Err(error) = result {
                log::warn!("保存设置失败：{error}");
                toast(&widgets, &format!("保存设置失败：{error}"));
            }
        },
    );
}

/// 冷启动调用一次：读设置、应用到 AdwStyleManager。不需要 Widgets/EditorState——
/// 只碰全局的 StyleManager，跟窗口状态无关。
pub(super) fn load_and_apply_async() {
    tasks::run(
        || Ok(settings::load(&app_data_dir())),
        |result: Result<AppSettings, cloudstack_core::AppError>| {
            if let Ok(loaded) = result {
                apply_color_scheme(loaded.color_scheme);
            }
        },
    );
}

/// 打开设置对话框；先异步读一次当前完整设置再建 UI，这样两个控件的变化处理器
/// 才能"读旧值改一个字段再存盘"，不会互相覆盖对方刚存的字段。
pub(super) fn show_dialog(widgets: &Widgets) {
    let widgets = widgets.clone();
    tasks::run(
        || Ok(settings::load(&app_data_dir())),
        move |result: Result<AppSettings, cloudstack_core::AppError>| {
            build_and_present_dialog(&widgets, result.unwrap_or_default());
        },
    );
}

fn build_and_present_dialog(widgets: &Widgets, current: AppSettings) {
    let current = Rc::new(RefCell::new(current));

    let model = gtk::StringList::new(&["跟随系统", "浅色", "深色"]);
    let selected_index = match current.borrow().color_scheme {
        ColorScheme::System => 0,
        ColorScheme::Light => 1,
        ColorScheme::Dark => 2,
    };
    let color_scheme_row = adw::ComboRow::builder()
        .title("配色方案")
        .model(&model)
        .selected(selected_index)
        .build();

    let color_scheme_widgets = widgets.clone();
    let color_scheme_current = Rc::clone(&current);
    color_scheme_row.connect_selected_notify(move |row| {
        let scheme = match row.selected() {
            1 => ColorScheme::Light,
            2 => ColorScheme::Dark,
            _ => ColorScheme::System,
        };
        apply_color_scheme(scheme);
        let updated = AppSettings {
            color_scheme: scheme,
            ..color_scheme_current.borrow().clone()
        };
        *color_scheme_current.borrow_mut() = updated.clone();
        save_async(&color_scheme_widgets, updated);
    });

    let auto_reopen_row = adw::SwitchRow::builder()
        .title("启动时自动打开最近项目")
        .subtitle("跳过欢迎页，直接进入最后一次打开的项目")
        .active(current.borrow().auto_reopen_last_project)
        .build();

    let auto_reopen_widgets = widgets.clone();
    let auto_reopen_current = Rc::clone(&current);
    auto_reopen_row.connect_active_notify(move |row| {
        let updated = AppSettings {
            auto_reopen_last_project: row.is_active(),
            ..auto_reopen_current.borrow().clone()
        };
        *auto_reopen_current.borrow_mut() = updated.clone();
        save_async(&auto_reopen_widgets, updated);
    });

    let restore_document_row = adw::SwitchRow::builder()
        .title("打开项目时跳回上次打开的文章")
        .subtitle("记住每个项目最后打开的文章")
        .active(current.borrow().restore_last_document_on_open)
        .build();

    let restore_document_widgets = widgets.clone();
    let restore_document_current = Rc::clone(&current);
    restore_document_row.connect_active_notify(move |row| {
        let updated = AppSettings {
            restore_last_document_on_open: row.is_active(),
            ..restore_document_current.borrow().clone()
        };
        *restore_document_current.borrow_mut() = updated.clone();
        save_async(&restore_document_widgets, updated);
    });

    let appearance_group = adw::PreferencesGroup::builder().title("外观").build();
    appearance_group.add(&color_scheme_row);

    let startup_group = adw::PreferencesGroup::builder().title("打开项目").build();
    startup_group.add(&auto_reopen_row);
    startup_group.add(&restore_document_row);

    let page = adw::PreferencesPage::builder().title("通用").build();
    page.add(&appearance_group);
    page.add(&startup_group);

    let dialog = adw::PreferencesDialog::builder().title("设置").build();
    dialog.add(&page);
    dialog.present(Some(&widgets.window));
}
