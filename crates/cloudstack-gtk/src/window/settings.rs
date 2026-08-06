use std::cell::RefCell;

use adw::prelude::*;
use cloudstack_core::services::settings::{self, AppSettings, ColorScheme};
use cloudstack_core::AppError;
use gtk::glib;

use crate::i18n::{self, UiMessage};
use crate::tasks;
use cloudstack_application::{SettingsWriter, SettingsWriterAction, VersionedSettings};

use super::{app_data_dir, toast, Widgets};

/// 共享设置写者的生命周期。冷启动读盘完成前，`show_dialog` 会保持沉默，
/// 避免用户在默认值上修改、把磁盘上的真实设置冲掉。
enum SettingsRuntime {
    Uninitialized,
    Loading,
    Ready(SettingsWriter),
}

thread_local! {
    static SETTINGS_RUNTIME: RefCell<SettingsRuntime> =
        const { RefCell::new(SettingsRuntime::Uninitialized) };
    /// 设置对话框单例：已经打开就重新 present 它，不再建第二个——两个对话
    /// 框各自的控件没有互相同步机制，允许并存只会让其中一个显示过期的值。
    static SETTINGS_DIALOG: RefCell<Option<glib::WeakRef<adw::PreferencesDialog>>> =
        const { RefCell::new(None) };
}

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

fn dialog_may_open(runtime: &SettingsRuntime) -> bool {
    matches!(runtime, SettingsRuntime::Ready(_))
}

/// 冷启动调用一次：异步读一次设置，用它初始化共享的 `SettingsWriter` 并把
/// 配色应用到 `AdwStyleManager`。
pub(super) fn load_and_initialize() {
    SETTINGS_RUNTIME.with(|runtime| *runtime.borrow_mut() = SettingsRuntime::Loading);
    tasks::run(
        || Ok(settings::load(&app_data_dir())),
        |result: Result<AppSettings, AppError>| {
            let Ok(loaded) = result else {
                log::warn!("加载设置失败，设置入口本次会话保持不可用");
                return;
            };
            apply_color_scheme(loaded.color_scheme);
            SETTINGS_RUNTIME.with(|runtime| {
                *runtime.borrow_mut() = SettingsRuntime::Ready(SettingsWriter::new(loaded));
            });
        },
    );
}

/// 所有设置修改的唯一入口：直接改共享 writer 的内存快照，再按它的判定决定
/// 要不要派发一次写盘。
fn update_settings(widgets: &Widgets, edit: impl FnOnce(&mut AppSettings)) {
    let action = SETTINGS_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let SettingsRuntime::Ready(writer) = &mut *runtime else {
            return SettingsWriterAction::None;
        };
        writer.update(edit)
    });
    dispatch_settings_action(widgets, action);
}

fn dispatch_settings_action(widgets: &Widgets, action: SettingsWriterAction) {
    if let SettingsWriterAction::Persist(versioned) = action {
        dispatch_settings_write(widgets, versioned);
    }
}

fn dispatch_settings_write(widgets: &Widgets, versioned: VersionedSettings) {
    let generation = versioned.generation;
    let snapshot = versioned.snapshot;
    let widgets = widgets.clone();
    tasks::run(
        move || settings::save(&app_data_dir(), &snapshot),
        move |result| complete_settings_write(&widgets, generation, result),
    );
}

fn complete_settings_write(widgets: &Widgets, generation: u64, result: Result<(), AppError>) {
    if let Err(error) = &result {
        log::warn!("保存设置失败：{error}");
    }
    let success = result.is_ok();
    let transition = SETTINGS_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let SettingsRuntime::Ready(writer) = &mut *runtime else {
            return Default::default();
        };
        writer.complete_write(generation, success)
    });
    if transition.report_failure && result.is_err() {
        toast(widgets, &i18n::text(UiMessage::SettingsWriteFailed));
    }
    if let Some(next) = transition.next_write {
        dispatch_settings_write(widgets, next);
    }
}

/// 打开设置对话框。已经有一个开着就重新 present 它；否则确认共享设置已经
/// 加载完成（见 `dialog_may_open`），顺带把上一次失败、还没被后续修改顶掉
/// 的写入重新排一次队，再用 writer 当前的权威快照建对话框。
pub(super) fn show_dialog(widgets: &Widgets) {
    let existing =
        SETTINGS_DIALOG.with(|dialog| dialog.borrow().as_ref().and_then(glib::WeakRef::upgrade));
    if let Some(dialog) = existing {
        dialog.present(Some(&widgets.window));
        return;
    }

    let ready = SETTINGS_RUNTIME.with(|runtime| dialog_may_open(&runtime.borrow()));
    if !ready {
        return;
    }

    let retry_action = SETTINGS_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let SettingsRuntime::Ready(writer) = &mut *runtime else {
            unreachable!("刚确认过 dialog_may_open，GTK 单线程下状态不会被别的代码改掉");
        };
        writer.retry_failed_write()
    });
    dispatch_settings_action(widgets, retry_action);

    let current = SETTINGS_RUNTIME.with(|runtime| match &*runtime.borrow() {
        SettingsRuntime::Ready(writer) => writer.current().clone(),
        _ => unreachable!("刚确认过 dialog_may_open，GTK 单线程下状态不会被别的代码改掉"),
    });
    build_and_present_dialog(widgets, current);
}

fn build_and_present_dialog(widgets: &Widgets, current: AppSettings) {
    let color_scheme_options = [
        i18n::text(UiMessage::SettingsColorSchemeSystem),
        i18n::text(UiMessage::SettingsColorSchemeLight),
        i18n::text(UiMessage::SettingsColorSchemeDark),
    ];
    let color_scheme_options = color_scheme_options
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let model = gtk::StringList::new(&color_scheme_options);
    let selected_index = match current.color_scheme {
        ColorScheme::System => 0,
        ColorScheme::Light => 1,
        ColorScheme::Dark => 2,
    };
    let color_scheme_row = adw::ComboRow::builder()
        .title(i18n::text(UiMessage::SettingsColorSchemeTitle))
        .model(&model)
        .selected(selected_index)
        .build();

    let color_scheme_widgets = widgets.clone();
    color_scheme_row.connect_selected_notify(move |row| {
        let scheme = match row.selected() {
            1 => ColorScheme::Light,
            2 => ColorScheme::Dark,
            _ => ColorScheme::System,
        };
        apply_color_scheme(scheme);
        update_settings(&color_scheme_widgets, |settings| {
            settings.color_scheme = scheme
        });
    });

    let auto_reopen_row = adw::SwitchRow::builder()
        .title(i18n::text(UiMessage::SettingsAutoReopenTitle))
        .subtitle(i18n::text(UiMessage::SettingsAutoReopenSubtitle))
        .active(current.auto_reopen_last_project)
        .build();

    let auto_reopen_widgets = widgets.clone();
    auto_reopen_row.connect_active_notify(move |row| {
        let active = row.is_active();
        update_settings(&auto_reopen_widgets, move |settings| {
            settings.auto_reopen_last_project = active;
        });
    });

    let restore_document_row = adw::SwitchRow::builder()
        .title(i18n::text(UiMessage::SettingsRestoreDocumentTitle))
        .subtitle(i18n::text(UiMessage::SettingsRestoreDocumentSubtitle))
        .active(current.restore_last_document_on_open)
        .build();

    let restore_document_widgets = widgets.clone();
    restore_document_row.connect_active_notify(move |row| {
        let active = row.is_active();
        update_settings(&restore_document_widgets, move |settings| {
            settings.restore_last_document_on_open = active;
        });
    });

    let appearance_group = adw::PreferencesGroup::builder()
        .title(i18n::text(UiMessage::SettingsAppearanceGroup))
        .build();
    appearance_group.add(&color_scheme_row);

    let startup_group = adw::PreferencesGroup::builder()
        .title(i18n::text(UiMessage::SettingsOpenProjectGroup))
        .build();
    startup_group.add(&auto_reopen_row);
    startup_group.add(&restore_document_row);

    let page = adw::PreferencesPage::builder()
        .title(i18n::text(UiMessage::SettingsGeneralPage))
        .build();
    page.add(&appearance_group);
    page.add(&startup_group);

    let dialog = adw::PreferencesDialog::builder()
        .title(i18n::text(UiMessage::SettingsDialogTitle))
        .build();
    dialog.add(&page);
    SETTINGS_DIALOG.with(|cell| *cell.borrow_mut() = Some(dialog.downgrade()));
    dialog.present(Some(&widgets.window));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_may_open_only_when_settings_are_ready() {
        assert!(!dialog_may_open(&SettingsRuntime::Uninitialized));
        assert!(!dialog_may_open(&SettingsRuntime::Loading));
        assert!(dialog_may_open(&SettingsRuntime::Ready(
            SettingsWriter::new(AppSettings::default())
        )));
    }
}
