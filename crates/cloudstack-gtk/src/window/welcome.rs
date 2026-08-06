use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::services::recent::RecentProject;
use gtk::glib;

use crate::i18n::{self, UiMessage};

#[derive(Clone)]
pub(super) struct WelcomePage {
    root: adw::StatusPage,
    open_button: gtk::Button,
    pinned_group: adw::PreferencesGroup,
    pinned_rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    recent_group: adw::PreferencesGroup,
    recent_rows: Rc<RefCell<Vec<adw::ActionRow>>>,
}

impl WelcomePage {
    pub(super) fn new() -> Self {
        let open_button = gtk::Button::builder()
            .label(i18n::text(UiMessage::WelcomeOpenProjectLabel))
            .tooltip_text(i18n::text(UiMessage::WelcomeOpenProjectTooltip))
            .action_name("win.open-project")
            .css_classes(["suggested-action", "pill"])
            .halign(gtk::Align::Center)
            .build();
        let shortcut_hint = gtk::Label::builder()
            .label(i18n::text(UiMessage::WelcomeShortcutHint))
            .css_classes(["dim-label", "caption"])
            .halign(gtk::Align::Center)
            .build();

        let pinned_group = adw::PreferencesGroup::builder()
            .title(i18n::text(UiMessage::PinnedProjectsTitle))
            .visible(false)
            .build();
        let recent_group = adw::PreferencesGroup::builder()
            .title(i18n::text(UiMessage::RecentProjectsTitle))
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_bottom(24);
        content.append(&open_button);
        content.append(&shortcut_hint);
        content.append(&pinned_group);
        content.append(&recent_group);
        let clamp = adw::Clamp::builder()
            .maximum_size(420)
            .child(&content)
            .build();

        let root = adw::StatusPage::builder()
            .icon_name("folder-symbolic")
            .title(i18n::text(UiMessage::WelcomeTitle))
            .description(i18n::text(UiMessage::WelcomeDescription))
            .child(&clamp)
            .vexpand(true)
            .build();

        let page = Self {
            root,
            open_button,
            pinned_group,
            pinned_rows: Rc::new(RefCell::new(Vec::new())),
            recent_group,
            recent_rows: Rc::new(RefCell::new(Vec::new())),
        };
        page.set_recent(&[], |_| {}, |_| {}, |_, _| {});
        page
    }

    pub(super) fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub(super) fn set_open_sensitive(&self, sensitive: bool) {
        self.open_button.set_sensitive(sensitive);
    }

    /// 重建整组行；`on_open`/`on_remove`/`on_toggle_pin` 只接收路径（和目标固定
    /// 状态），WelcomePage 自身从不接触 Widgets/EditorState。
    pub(super) fn set_recent(
        &self,
        projects: &[RecentProject],
        on_open: impl Fn(&Path) + 'static,
        on_remove: impl Fn(&Path) + 'static,
        on_toggle_pin: impl Fn(&Path, bool) + 'static,
    ) {
        let on_open = Rc::new(on_open);
        let on_remove = Rc::new(on_remove);
        let on_toggle_pin = Rc::new(on_toggle_pin);

        let (pinned, recent): (Vec<_>, Vec<_>) =
            projects.iter().partition(|project| project.pinned);

        rebuild_group(
            &self.pinned_group,
            &self.pinned_rows,
            &pinned,
            None,
            &on_open,
            &on_remove,
            &on_toggle_pin,
        );
        self.pinned_group.set_visible(!pinned.is_empty());

        rebuild_group(
            &self.recent_group,
            &self.recent_rows,
            &recent,
            Some(&i18n::text(UiMessage::NoRecentProjects)),
            &on_open,
            &on_remove,
            &on_toggle_pin,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_group(
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
    projects: &[&RecentProject],
    empty_placeholder: Option<&str>,
    on_open: &Rc<impl Fn(&Path) + 'static>,
    on_remove: &Rc<impl Fn(&Path) + 'static>,
    on_toggle_pin: &Rc<impl Fn(&Path, bool) + 'static>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }

    if projects.is_empty() {
        if let Some(placeholder_text) = empty_placeholder {
            let placeholder = adw::ActionRow::builder()
                .title(placeholder_text)
                .use_markup(false)
                .sensitive(false)
                .build();
            group.add(&placeholder);
            rows.borrow_mut().push(placeholder);
        }
        return;
    }

    for project in projects {
        let folder_name = project
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| project.root.display().to_string());

        let row = adw::ActionRow::builder()
            .title(folder_name)
            .subtitle(project.root.display().to_string())
            .use_markup(false)
            .activatable(true)
            .build();

        let pin_button = gtk::Button::builder()
            .icon_name(if project.pinned {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            })
            .tooltip_text(if project.pinned {
                i18n::text(UiMessage::UnpinProjectTooltip)
            } else {
                i18n::text(UiMessage::PinProjectTooltip)
            })
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let remove_button = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text(i18n::text(UiMessage::RemoveRecentProjectTooltip))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let time_label = gtk::Label::builder()
            .label(format_last_opened(project.last_opened_ms))
            .css_classes(["dim-label", "caption"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&time_label);
        row.add_suffix(&pin_button);
        row.add_suffix(&remove_button);

        let path = project.root.clone();
        let open_cb = Rc::clone(on_open);
        row.connect_activated(move |_| open_cb(&path));

        let path = project.root.clone();
        let remove_cb = Rc::clone(on_remove);
        remove_button.connect_clicked(move |_| remove_cb(&path));

        let path = project.root.clone();
        let next_pinned = !project.pinned;
        let toggle_cb = Rc::clone(on_toggle_pin);
        pin_button.connect_clicked(move |_| toggle_cb(&path, next_pinned));

        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

fn format_last_opened(last_opened_ms: u64) -> String {
    let seconds = i64::try_from(last_opened_ms / 1000).unwrap_or(i64::MAX);
    glib::DateTime::from_unix_local(seconds)
        .and_then(|datetime| datetime.format("%Y-%m-%d %H:%M"))
        .map(|value| value.to_string())
        .unwrap_or_else(|_| i18n::text(UiMessage::UnknownTime))
}
