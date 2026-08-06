mod articles;
mod drafts;
mod frontmatter;
mod git_panel;
mod publish;
mod recent;
mod settings;
mod welcome;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::model::{PostDocument, PostSummary, ProjectContext, RepositorySnapshot};
use cloudstack_core::services::assets::PendingAssetManager;
use cloudstack_core::services::{assets, git, posts, project};
use gtk::{gdk, gio, glib};
use sourceview::prelude::*;

use crate::app::{apply_successful_save, classify_save_completion, SaveCompletionOutcome};
use crate::i18n::{self, UiMessage};
use crate::search::SearchPanel;
use crate::tasks;

const DEFAULT_EDITOR_PANE_WIDTH: i32 = 570;
const MIN_PREVIEW_PANE_WIDTH: i32 = 360;

#[derive(Default)]
struct EditorState {
    project: Option<ProjectContext>,
    posts: Vec<PostSummary>,
    document: Option<PostDocument>,
    loading_buffer: bool,
    dirty: bool,
    busy: bool,
    document_epoch: u64,
    /// 每次 mark_document_dirty 自增一次，用来在保存完成时判断保存期间
    /// buffer 有没有被再次修改。
    edit_generation: u64,
    draft_queue: drafts::DraftQueue,
    pending_assets: PendingAssetManager,
    git_snapshot: Option<RepositorySnapshot>,
    /// 当前会话中已修改但尚未写回磁盘的文章快照。允许切换文章时保留编辑内容。
    unsaved_documents: HashMap<String, PostDocument>,
}

enum OpenProjectOutcome {
    Opened(ProjectContext, Vec<PostSummary>),
    NeedsInitialization {
        root: PathBuf,
        suggested_content_dir: String,
    },
    NeedsContentRepair {
        root: PathBuf,
        content_dir: String,
    },
}

#[derive(Clone)]
struct Widgets {
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    content_stack: gtk::Stack,
    welcome_page: welcome::WelcomePage,
    post_list: gtk::ListBox,
    git_panel: git_panel::GitPanel,
    open_button: gtk::Button,
    home_button: gtk::Button,
    new_button: gtk::Button,
    rename_button: gtk::Button,
    delete_button: gtk::Button,
    editor: sourceview::View,
    buffer: sourceview::Buffer,
    search_panel: SearchPanel,
    preview: crate::preview::Preview,
    frontmatter_panel: gtk::Box,
    frontmatter_split: adw::OverlaySplitView,
    properties_button: gtk::Button,
    save_button: gtk::Button,
    project_label: gtk::Label,
    status_label: gtk::Label,
}

pub fn present(application: &adw::Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }

    let app_name = i18n::text(UiMessage::AppName);
    glib::set_application_name(&app_name);
    sourceview::init();

    let state = Rc::new(RefCell::new(EditorState::default()));
    let widgets = build_window(application);
    connect_editor(&widgets, &state);
    connect_image_paste(&widgets, &state);
    connect_actions(application, &widgets, &state);
    connect_post_list(&widgets, &state);
    connect_close_guard(&widgets, &state);
    recent::load_async(&widgets, &state);
    settings::load_and_initialize();
    recent::maybe_reopen_last_project(&widgets, &state);

    #[cfg(feature = "e2e")]
    if let Some(root) = std::env::var_os("CLOUDSTACK_E2E_PROJECT") {
        open_project(&widgets, &state, Path::new(&root));
    }
    #[cfg(feature = "e2e")]
    if std::env::var_os("CLOUDSTACK_E2E_GIT_EXPANDED").is_some() {
        widgets.git_panel.set_expanded(true);
    }
    widgets.window.present();
}

fn build_window(application: &adw::Application) -> Widgets {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title(i18n::text(UiMessage::AppName))
        .default_width(1180)
        .default_height(760)
        .build();

    let open_button = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text(i18n::text(UiMessage::OpenProjectTooltip))
        .action_name("win.open-project")
        .build();
    let home_button = gtk::Button::builder()
        .icon_name("go-home-symbolic")
        .tooltip_text(i18n::text(UiMessage::CloseProjectTooltip))
        .action_name("win.close-project")
        .sensitive(false)
        .build();
    let save_content = adw::ButtonContent::builder()
        .icon_name("document-save-symbolic")
        .label(i18n::text(UiMessage::SaveLabel))
        .build();
    let save_button = gtk::Button::builder()
        .child(&save_content)
        .tooltip_text(i18n::text(UiMessage::SaveTooltip))
        .action_name("win.save")
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    let properties_button = gtk::Button::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text(i18n::text(UiMessage::ArticlePropertiesTooltip))
        .action_name("win.toggle-properties")
        .sensitive(false)
        .build();
    let project_label = gtk::Label::builder()
        .label(i18n::text(UiMessage::NoProject))
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(48)
        .build();
    let settings_button = gtk::Button::builder()
        .icon_name("preferences-system-symbolic")
        .tooltip_text(i18n::text(UiMessage::SettingsTooltip))
        .action_name("win.open-settings")
        .build();

    let header = adw::HeaderBar::new();
    header.pack_start(&open_button);
    header.pack_start(&home_button);
    header.pack_start(&project_label);
    header.pack_end(&settings_button);
    header.pack_end(&properties_button);
    header.pack_end(&save_button);

    let post_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(250)
        .vexpand(true)
        .child(&post_list)
        .build();

    let sidebar_title = gtk::Label::builder()
        .label(i18n::text(UiMessage::ArticlesHeading))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    let new_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(i18n::text(UiMessage::NewArticleTooltip))
        .action_name("win.new-post")
        .sensitive(false)
        .build();
    let rename_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(i18n::text(UiMessage::RenameArticleTooltip))
        .action_name("win.rename-post")
        .sensitive(false)
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(i18n::text(UiMessage::DeleteArticleTooltip))
        .action_name("win.delete-post")
        .sensitive(false)
        .build();
    let sidebar_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    sidebar_header.set_margin_top(8);
    sidebar_header.set_margin_bottom(8);
    sidebar_header.set_margin_start(8);
    sidebar_header.set_margin_end(8);
    sidebar_header.append(&sidebar_title);
    sidebar_header.append(&new_button);
    sidebar_header.append(&rename_button);
    sidebar_header.append(&delete_button);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.append(&sidebar_header);
    let git_panel = git_panel::GitPanel::new();
    let sidebar_split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .start_child(&sidebar_scroll)
        .end_child(git_panel.widget())
        .resize_start_child(true)
        .resize_end_child(false)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .vexpand(true)
        .build();
    git_panel.bind_split(&sidebar_split);
    sidebar.append(&sidebar_split);

    let buffer = sourceview::Buffer::builder()
        .enable_undo(true)
        .highlight_matching_brackets(true)
        .highlight_syntax(true)
        .implicit_trailing_newline(false)
        .build();
    if let Some(language) = sourceview::LanguageManager::default().language("markdown") {
        buffer.set_language(Some(&language));
    } else {
        log::warn!("GtkSourceView 没有提供 Markdown language definition");
    }
    connect_source_style(&buffer);

    let editor = sourceview::View::builder()
        .buffer(&buffer)
        .auto_indent(true)
        .highlight_current_line(true)
        .indent_on_tab(true)
        .insert_spaces_instead_of_tabs(true)
        .show_line_numbers(true)
        .smart_backspace(true)
        .tab_width(4)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(24)
        .bottom_margin(48)
        .left_margin(32)
        .right_margin(32)
        .build();
    let initial_editor_text = i18n::text(UiMessage::InitialEditorText);
    buffer.set_text(&format!("{initial_editor_text}\n"));

    let editor_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&editor)
        .build();
    let toast_overlay = adw::ToastOverlay::new();
    let search_panel = SearchPanel::new(&buffer, &editor);
    let preview = crate::preview::Preview::new(&buffer, &editor, &editor_scroll, &toast_overlay);
    let status_label = gtk::Label::builder()
        .label(i18n::text(UiMessage::ReadyStatus))
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    status_box.append(&status_label);
    status_box.append(preview.diagnostic_button());
    let editor_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    editor_box.append(search_panel.widget());
    editor_box.append(&editor_scroll);
    editor_box.append(&status_box);

    let content_split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&editor_box)
        .end_child(preview.widget())
        .shrink_start_child(false)
        .shrink_end_child(false)
        .build();
    let content_split_weak = content_split.downgrade();
    content_split.connect_realize(move |_| {
        let content_split_weak = content_split_weak.clone();
        glib::idle_add_local_once(move || {
            let Some(content_split) = content_split_weak.upgrade() else {
                return;
            };
            content_split.set_position(initial_content_split_position(
                content_split.min_position(),
                content_split.max_position(),
            ));
        });
    });

    let frontmatter_panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
    frontmatter_panel.set_margin_top(16);
    frontmatter_panel.set_margin_bottom(16);
    frontmatter_panel.set_margin_start(12);
    frontmatter_panel.set_margin_end(12);
    let frontmatter_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(340)
        .min_content_width(310)
        .max_content_width(380)
        .child(&frontmatter_panel)
        .build();
    let frontmatter_split = adw::OverlaySplitView::new();
    frontmatter_split.set_content(Some(&content_split));
    frontmatter_split.set_sidebar(Some(&frontmatter_scroll));
    frontmatter_split.set_sidebar_position(gtk::PackType::End);
    frontmatter_split.set_collapsed(true);
    frontmatter_split.set_show_sidebar(false);

    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .position(290)
        .start_child(&sidebar)
        .end_child(&frontmatter_split)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .build();

    let welcome_page = welcome::WelcomePage::new();
    let content_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vexpand(true)
        .hexpand(true)
        .build();
    content_stack.add_named(welcome_page.widget(), Some("welcome"));
    content_stack.add_named(&split, Some("workspace"));
    content_stack.set_visible_child_name("welcome");

    toast_overlay.set_child(Some(&content_stack));
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toast_overlay));
    window.set_content(Some(&toolbar_view));

    Widgets {
        window,
        toast_overlay,
        content_stack,
        welcome_page,
        post_list,
        git_panel,
        open_button,
        home_button,
        new_button,
        rename_button,
        delete_button,
        editor,
        buffer,
        search_panel,
        preview,
        frontmatter_panel,
        frontmatter_split,
        properties_button,
        save_button,
        project_label,
        status_label,
    }
}

fn initial_content_split_position(minimum: i32, maximum: i32) -> i32 {
    if maximum <= minimum {
        return minimum;
    }
    DEFAULT_EDITOR_PANE_WIDTH
        .min(maximum.saturating_sub(MIN_PREVIEW_PANE_WIDTH))
        .clamp(minimum, maximum)
}

fn connect_source_style(buffer: &sourceview::Buffer) {
    let style_manager = adw::StyleManager::default();
    set_source_style(buffer, style_manager.is_dark());
    let buffer = buffer.clone();
    style_manager.connect_dark_notify(move |manager| {
        set_source_style(&buffer, manager.is_dark());
    });
}

fn set_source_style(buffer: &sourceview::Buffer, dark: bool) {
    let schemes = sourceview::StyleSchemeManager::default();
    let candidates = if dark {
        ["Adwaita-dark", "classic-dark", "oblivion"]
    } else {
        ["Adwaita", "classic", "tango"]
    };
    if let Some(scheme) = candidates.iter().find_map(|id| schemes.scheme(id)) {
        buffer.set_style_scheme(Some(&scheme));
    }
}

fn connect_editor(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    widgets.buffer.clone().connect_changed(move |_| {
        if state.borrow().loading_buffer || state.borrow().document.is_none() {
            return;
        }
        mark_document_dirty(&widgets, &state);
        let source = widgets
            .buffer
            .text(
                &widgets.buffer.start_iter(),
                &widgets.buffer.end_iter(),
                true,
            )
            .to_string();
        widgets.preview.schedule(source, false);
    });
}

fn connect_image_paste(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let controller = gtk::EventControllerKey::new();
    let callback_widgets = widgets.clone();
    let state = Rc::clone(state);
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        let is_paste = modifiers.contains(gdk::ModifierType::CONTROL_MASK)
            && key.to_unicode().is_some_and(|character| character == 'v');
        let paste_blocked = {
            let state = state.borrow();
            state.document.is_none() || state.busy
        };
        if !is_paste || paste_blocked {
            return glib::Propagation::Proceed;
        }

        let clipboard = callback_widgets.editor.display().clipboard();
        if !clipboard
            .formats()
            .contains_type(gdk::Texture::static_type())
        {
            return glib::Propagation::Proceed;
        }

        paste_clipboard_image(&callback_widgets, &state, &clipboard);
        glib::Propagation::Stop
    });
    widgets.editor.add_controller(controller);
}

fn paste_clipboard_image(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    clipboard: &gdk::Clipboard,
) {
    let (context, post_id) = {
        let state = state.borrow();
        let (Some(context), Some(document)) = (&state.project, &state.document) else {
            return;
        };
        (context.clone(), document.id.clone())
    };
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    clipboard.read_texture_async(None::<&gio::Cancellable>, move |result| {
        let texture = match result {
            Ok(Some(texture)) => texture,
            Ok(None) => {
                show_error(&widgets, "剪贴板没有可读取的图片");
                return;
            }
            Err(error) => {
                show_error(&widgets, &format!("读取剪贴板图片失败：{error}"));
                return;
            }
        };

        let is_same_document = state
            .borrow()
            .document
            .as_ref()
            .is_some_and(|document| document.id == post_id);
        if !is_same_document {
            return;
        }

        let png = texture.save_to_png_bytes();
        let outcome = match assets::save_image(&context, &post_id, None, png.as_ref()) {
            Ok(outcome) => outcome,
            Err(error) => {
                show_error(&widgets, &error.to_string());
                return;
            }
        };
        if let Some(pending) = outcome.pending {
            state.borrow_mut().pending_assets.track(pending);
        }
        widgets
            .buffer
            .insert_at_cursor(&format!("![]({})", outcome.image.markdown_path));
        toast(&widgets, "图片已保存并插入文章");
    });
}

fn connect_actions(
    application: &adw::Application,
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
) {
    let open_action = gio::SimpleAction::new("open-project", None);
    let open_widgets = widgets.clone();
    let open_state = Rc::clone(state);
    open_action.connect_activate(move |_, _| select_project(&open_widgets, &open_state));
    widgets.window.add_action(&open_action);

    let settings_action = gio::SimpleAction::new("open-settings", None);
    let settings_widgets = widgets.clone();
    settings_action.connect_activate(move |_, _| settings::show_dialog(&settings_widgets));
    widgets.window.add_action(&settings_action);

    let close_project_action = gio::SimpleAction::new("close-project", None);
    let close_widgets = widgets.clone();
    let close_state = Rc::clone(state);
    close_project_action.connect_activate(move |_, _| close_project(&close_widgets, &close_state));
    widgets.window.add_action(&close_project_action);

    let save_action = gio::SimpleAction::new("save", None);
    let save_widgets = widgets.clone();
    let save_state = Rc::clone(state);
    save_action.connect_activate(move |_, _| save_document(&save_widgets, &save_state));
    widgets.window.add_action(&save_action);

    let properties_action = gio::SimpleAction::new("toggle-properties", None);
    let properties_split = widgets.frontmatter_split.clone();
    properties_action.connect_activate(move |_, _| {
        properties_split.set_show_sidebar(!properties_split.shows_sidebar());
    });
    widgets.window.add_action(&properties_action);

    let publish_action = gio::SimpleAction::new("publish", None);
    let publish_widgets = widgets.clone();
    let publish_state = Rc::clone(state);
    publish_action.connect_activate(move |_, _| {
        if publish_state.borrow().busy {
            return;
        }
        let unsaved_count = unsaved_document_count(&publish_state);
        if unsaved_count > 1 {
            toast(&publish_widgets, "请先保存其他未保存文章，再执行 Git 发布");
            return;
        }
        if unsaved_count == 1 && !publish_state.borrow().dirty {
            toast(&publish_widgets, "请先保存未保存文章，再执行 Git 发布");
            return;
        }
        if publish_state.borrow().dirty {
            let callback_widgets = publish_widgets.clone();
            let callback_state = Rc::clone(&publish_state);
            save_document_then(
                &publish_widgets,
                &publish_state,
                Some(Box::new(move || {
                    publish::show_dialog(&callback_widgets, &callback_state);
                })),
            );
        } else {
            publish::show_dialog(&publish_widgets, &publish_state);
        }
    });
    widgets.window.add_action(&publish_action);

    let refresh_git_action = gio::SimpleAction::new("refresh-git", None);
    let refresh_widgets = widgets.clone();
    let refresh_state = Rc::clone(state);
    refresh_git_action.connect_activate(move |_, _| {
        git_panel::refresh(&refresh_widgets, &refresh_state);
    });
    widgets.window.add_action(&refresh_git_action);

    let git_primary_action = gio::SimpleAction::new("git-primary", None);
    let primary_widgets = widgets.clone();
    let primary_state = Rc::clone(state);
    git_primary_action.connect_activate(move |_, _| {
        git_panel::activate_primary(&primary_widgets, &primary_state);
    });
    widgets.window.add_action(&git_primary_action);

    let fetch_git_action = gio::SimpleAction::new("fetch-git", None);
    let fetch_widgets = widgets.clone();
    let fetch_state = Rc::clone(state);
    fetch_git_action.connect_activate(move |_, _| {
        git_panel::fetch_remote(&fetch_widgets, &fetch_state);
    });
    widgets.window.add_action(&fetch_git_action);

    let untrack_config_action = gio::SimpleAction::new("untrack-config", None);
    let untrack_widgets = widgets.clone();
    let untrack_state = Rc::clone(state);
    untrack_config_action.connect_activate(move |_, _| {
        git_panel::untrack_config(&untrack_widgets, &untrack_state);
    });
    widgets.window.add_action(&untrack_config_action);

    let find_action = gio::SimpleAction::new("find", None);
    let search_panel = widgets.search_panel.clone();
    find_action.connect_activate(move |_, _| search_panel.open(false));
    widgets.window.add_action(&find_action);

    let replace_action = gio::SimpleAction::new("replace", None);
    let search_panel = widgets.search_panel.clone();
    replace_action.connect_activate(move |_, _| search_panel.open(true));
    widgets.window.add_action(&replace_action);

    let next_match_action = gio::SimpleAction::new("next-match", None);
    let search_panel = widgets.search_panel.clone();
    next_match_action.connect_activate(move |_, _| search_panel.next());
    widgets.window.add_action(&next_match_action);

    let previous_match_action = gio::SimpleAction::new("previous-match", None);
    let search_panel = widgets.search_panel.clone();
    previous_match_action.connect_activate(move |_, _| search_panel.previous());
    widgets.window.add_action(&previous_match_action);

    let new_post_action = gio::SimpleAction::new("new-post", None);
    let action_widgets = widgets.clone();
    let action_state = Rc::clone(state);
    new_post_action
        .connect_activate(move |_, _| articles::show_create_dialog(&action_widgets, &action_state));
    widgets.window.add_action(&new_post_action);

    let rename_post_action = gio::SimpleAction::new("rename-post", None);
    let action_widgets = widgets.clone();
    let action_state = Rc::clone(state);
    rename_post_action.connect_activate(move |_, _| {
        articles::show_rename_dialog(&action_widgets, &action_state);
    });
    widgets.window.add_action(&rename_post_action);

    let delete_post_action = gio::SimpleAction::new("delete-post", None);
    let action_widgets = widgets.clone();
    let action_state = Rc::clone(state);
    delete_post_action.connect_activate(move |_, _| {
        articles::show_delete_dialog(&action_widgets, &action_state);
    });
    widgets.window.add_action(&delete_post_action);

    application.set_accels_for_action("win.open-project", &["<Control>o"]);
    application.set_accels_for_action("win.save", &["<Control>s"]);
    application.set_accels_for_action("win.toggle-properties", &["<Control><Shift>p"]);
    application.set_accels_for_action("win.find", &["<Control>f"]);
    application.set_accels_for_action("win.replace", &["<Control>h"]);
    application.set_accels_for_action("win.next-match", &["F3"]);
    application.set_accels_for_action("win.previous-match", &["<Shift>F3"]);
    application.set_accels_for_action("win.new-post", &["<Control>n"]);
    application.set_accels_for_action("win.rename-post", &["F2"]);
    application.set_accels_for_action("win.delete-post", &["<Control>Delete"]);
}

fn connect_post_list(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    widgets
        .post_list
        .clone()
        .connect_row_activated(move |_, row| {
            let index = row.index();
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            let post_id = state.borrow().posts.get(index).map(|post| post.id.clone());
            if let Some(post_id) = post_id {
                load_document(&widgets, &state, &post_id);
            }
        });
}

/// `select_project`（打开新项目）和 `close_project`（返回主页）共用的守卫：
/// 忙碌或有未保存文章时都不能切走。
fn ensure_no_unsaved_documents(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) -> bool {
    let state_snapshot = state.borrow();
    if state_snapshot.busy {
        return false;
    }
    if !state_snapshot.unsaved_documents.is_empty() {
        drop(state_snapshot);
        toast(widgets, "当前项目有未保存文章，请先保存后再切换项目");
        return false;
    }
    true
}

/// `reconcile_saved_post` 可能因为一次性的 IO 错误把某个 entry 保留下来等重试；
/// 切换/关闭项目时（此时已经确认没有未保存文章）顺手重试一次。正常情况下这是
/// 空操作。
fn retry_pending_asset_cleanup(state: &Rc<RefCell<EditorState>>) {
    let Some(root) = state
        .borrow()
        .project
        .as_ref()
        .map(|context| context.root.clone())
    else {
        return;
    };
    if let Err(error) = state.borrow_mut().pending_assets.discard_project(&root) {
        log::warn!("重试清理待提交图片失败：{error}");
    }
}

fn select_project(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    if !ensure_no_unsaved_documents(widgets, state) {
        return;
    }
    retry_pending_asset_cleanup(state);

    let dialog = gtk::FileDialog::builder()
        .title(i18n::text(UiMessage::SelectProjectDialogTitle))
        .modal(true)
        .build();
    let widgets = widgets.clone();
    let parent = widgets.window.clone();
    let state = Rc::clone(state);
    dialog.select_folder(
        Some(&parent),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(folder) => {
                let Some(path) = folder.path() else {
                    show_error(&widgets, &i18n::text(UiMessage::OnlyLocalProject));
                    return;
                };
                open_project(&widgets, &state, &path);
            }
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(error) => show_error(&widgets, &format!("打开目录失败：{error}")),
        },
    );
}

/// `open_project` 的 `Opened` 分支反着走一遍：完全关闭当前项目，回到欢迎页。
fn close_project(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    if !ensure_no_unsaved_documents(widgets, state) {
        return;
    }
    if state.borrow().project.is_none() {
        return;
    }
    retry_pending_asset_cleanup(state);

    let mut editor_state = state.borrow_mut();
    editor_state.project = None;
    editor_state.posts.clear();
    editor_state.document = None;
    editor_state.dirty = false;
    editor_state.git_snapshot = None;
    editor_state.document_epoch = editor_state.document_epoch.wrapping_add(1);
    let epoch = editor_state.document_epoch;
    drop(editor_state);

    populate_post_list(widgets, state, &[]);
    widgets.buffer.set_text("");
    widgets.preview.clear(epoch);
    widgets.frontmatter_split.set_show_sidebar(false);
    widgets
        .project_label
        .set_label(&i18n::text(UiMessage::NoProject));
    widgets
        .window
        .set_title(Some(&i18n::text(UiMessage::AppName)));
    widgets.content_stack.set_visible_child_name("welcome");
    git_panel::refresh(widgets, state);
    set_busy(widgets, state, false, "");
    // touch() 在打开项目时是 fire-and-forget，不会刷新欢迎页已绑定的列表；
    // 不重新加载的话，这里看到的还是打开这个项目之前的旧快照。
    recent::load_async(widgets, state);
}

fn open_project(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, path: &Path) {
    if state.borrow().busy {
        return;
    }
    set_busy(widgets, state, true, "正在扫描项目…");
    let path = path.to_owned();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || match project::open_project(&path) {
            Ok(context) => {
                // CloudStack 配置是编辑器本地状态；对独立 Git 项目自动写入
                // repository-local exclude，避免第一次打开就把配置列为待提交文件。
                let _ = git::ensure_local_config_excluded(&context);
                let post_summaries = posts::list_posts(&context)?;
                Ok(OpenProjectOutcome::Opened(context, post_summaries))
            }
            Err(cloudstack_core::error::AppError::MissingProjectConfig) => {
                let suggested_content_dir = project::suggest_content_dir(&path)?;
                Ok(OpenProjectOutcome::NeedsInitialization {
                    root: path,
                    suggested_content_dir,
                })
            }
            Err(cloudstack_core::error::AppError::MissingContentDirectory(content_dir)) => {
                Ok(OpenProjectOutcome::NeedsContentRepair {
                    root: path,
                    content_dir,
                })
            }
            Err(error) => Err(error),
        },
        move |result| {
            let mut initialization = None;
            let mut content_repair = None;
            match result {
                Ok(OpenProjectOutcome::Opened(context, post_summaries)) => {
                    let root_for_recent = context.root.clone();
                    widgets
                        .project_label
                        .set_label(&context.root.display().to_string());
                    widgets
                        .status_label
                        .set_label(&i18n::text(UiMessage::ProjectOpenedStatus {
                            count: post_summaries.len(),
                        }));
                    let mut editor_state = state.borrow_mut();
                    editor_state.loading_buffer = true;
                    editor_state.project = Some(context);
                    editor_state.git_snapshot = None;
                    editor_state.posts = post_summaries;
                    editor_state.document = None;
                    editor_state.dirty = false;
                    editor_state.unsaved_documents.clear();
                    editor_state.document_epoch = editor_state.document_epoch.wrapping_add(1);
                    let epoch = editor_state.document_epoch;
                    drop(editor_state);
                    let post_summaries = state.borrow().posts.clone();
                    populate_post_list(&widgets, &state, &post_summaries);
                    let empty_post_list = i18n::text(UiMessage::InitialPostListText);
                    widgets.buffer.set_text(&format!("{empty_post_list}\n"));
                    state.borrow_mut().loading_buffer = false;
                    widgets.preview.clear(epoch);
                    #[cfg(feature = "e2e")]
                    widgets.frontmatter_split.set_show_sidebar(
                        std::env::var_os("CLOUDSTACK_E2E_PROPERTIES_OPEN").is_some(),
                    );
                    #[cfg(not(feature = "e2e"))]
                    widgets.frontmatter_split.set_show_sidebar(false);
                    let folder_name = project_folder_name(&root_for_recent);
                    widgets
                        .window
                        .set_title(Some(&i18n::text(UiMessage::WindowTitle {
                            folder: folder_name,
                        })));
                    widgets.content_stack.set_visible_child_name("workspace");
                    recent::touch(&root_for_recent);
                    recent::maybe_reopen_last_document(&widgets, &state, root_for_recent.clone());
                    frontmatter::refresh(&widgets, &state);
                    git_panel::refresh(&widgets, &state);
                }
                Ok(OpenProjectOutcome::NeedsInitialization {
                    root,
                    suggested_content_dir,
                }) => initialization = Some((root, suggested_content_dir)),
                Ok(OpenProjectOutcome::NeedsContentRepair { root, content_dir }) => {
                    content_repair = Some((root, content_dir));
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
            if let Some((root, suggested_content_dir)) = initialization {
                show_project_initialization_dialog(&widgets, &state, root, &suggested_content_dir);
                return;
            }
            if let Some((root, content_dir)) = content_repair {
                show_content_repair_dialog(&widgets, &state, root, &content_dir);
                #[cfg(feature = "e2e")]
                return;
            }
            #[cfg(feature = "e2e")]
            if std::env::var_os("CLOUDSTACK_E2E_OPEN_FIRST").is_some() {
                let requested = std::env::var("CLOUDSTACK_E2E_POST_ID").ok();
                let post_id =
                    requested.or_else(|| state.borrow().posts.first().map(|post| post.id.clone()));
                if let Some(post_id) = post_id {
                    load_document(&widgets, &state, &post_id);
                }
            }
        },
    );
}

fn show_content_repair_dialog(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    root: PathBuf,
    content_dir: &str,
) {
    let directory_entry = gtk::Entry::builder()
        .text(content_dir)
        .activates_default(true)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(
        &gtk::Label::builder()
            .label(i18n::text(UiMessage::ContentRepairDescription))
            .xalign(0.0)
            .wrap(true)
            .build(),
    );
    content.append(&directory_entry);
    let dialog = adw::AlertDialog::builder()
        .heading(i18n::text(UiMessage::MissingContentDirectoryHeading))
        .body(i18n::text(UiMessage::MissingContentDirectoryBody {
            content_dir: content_dir.to_owned(),
        }))
        .extra_child(&content)
        .default_response("repair")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", i18n::text(UiMessage::Cancel).as_str()),
        ("repair", i18n::text(UiMessage::RepairAndOpen).as_str()),
    ]);
    dialog.set_response_appearance("repair", adw::ResponseAppearance::Suggested);

    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let callback_entry = directory_entry.clone();
    dialog.connect_response(Some("repair"), move |_, _| {
        let content_dir = callback_entry.text().trim().to_owned();
        if content_dir.is_empty() {
            show_error(
                &callback_widgets,
                &i18n::text(UiMessage::ContentDirectoryEmpty),
            );
            return;
        }
        repair_and_open_project(
            &callback_widgets,
            &callback_state,
            root.clone(),
            content_dir,
        );
    });
    dialog.present(Some(&widgets.window));
    directory_entry.grab_focus();
    directory_entry.select_region(0, -1);
}

fn repair_and_open_project(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    root: PathBuf,
    content_dir: String,
) {
    let busy_message = i18n::text(UiMessage::RepairingContentDirectoryStatus);
    set_busy(widgets, state, true, &busy_message);
    let task_root = root.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || project::repair_content_directory(&task_root, &content_dir),
        move |result| {
            set_busy(&widgets, &state, false, "");
            match result {
                Ok(_) => {
                    toast(&widgets, &i18n::text(UiMessage::ContentDirectoryRepaired));
                    open_project(&widgets, &state, &root);
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
        },
    );
}

fn show_project_initialization_dialog(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    root: PathBuf,
    suggested_content_dir: &str,
) {
    let directory_entry = gtk::Entry::builder()
        .text(suggested_content_dir)
        .placeholder_text(i18n::text(UiMessage::ProjectDirectoryPlaceholder))
        .activates_default(true)
        .build();
    let blog_fields = gtk::CheckButton::with_label(&i18n::text(UiMessage::BlogFrontmatterOption));
    let path_label = gtk::Label::builder()
        .label(i18n::text(UiMessage::ProjectDirectoryLabel {
            path: root.display().to_string(),
        }))
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .tooltip_text(root.display().to_string())
        .css_classes(["dim-label"])
        .build();
    let directory_label = gtk::Label::builder()
        .label(i18n::text(UiMessage::ContentDirectoryLabel))
        .xalign(0.0)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(&path_label);
    content.append(&directory_label);
    content.append(&directory_entry);
    content.append(&blog_fields);

    let dialog = adw::AlertDialog::builder()
        .heading(i18n::text(UiMessage::CreateProjectHeading))
        .body(i18n::text(UiMessage::CreateProjectBody))
        .extra_child(&content)
        .default_response("create")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", i18n::text(UiMessage::Cancel).as_str()),
        ("create", i18n::text(UiMessage::CreateAndOpen).as_str()),
    ]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);

    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let callback_entry = directory_entry.clone();
    dialog.connect_response(Some("create"), move |_, _| {
        let content_dir = callback_entry.text().trim().to_owned();
        if content_dir.is_empty() {
            show_error(
                &callback_widgets,
                &i18n::text(UiMessage::ContentDirectoryEmpty),
            );
            return;
        }
        initialize_and_open_project(
            &callback_widgets,
            &callback_state,
            root.clone(),
            content_dir,
            blog_fields.is_active(),
        );
    });
    dialog.present(Some(&widgets.window));
    directory_entry.grab_focus();
    directory_entry.select_region(0, -1);
}

fn initialize_and_open_project(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    root: PathBuf,
    content_dir: String,
    with_blog_frontmatter: bool,
) {
    if state.borrow().busy {
        return;
    }
    let busy_message = i18n::text(UiMessage::CreatingProjectConfigStatus);
    set_busy(widgets, state, true, &busy_message);
    let task_root = root.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            let context =
                project::initialize_project(&task_root, &content_dir, with_blog_frontmatter)?;
            git::ensure_local_config_excluded(&context)?;
            Ok(context)
        },
        move |result| {
            set_busy(&widgets, &state, false, "");
            match result {
                Ok(_) => {
                    toast(&widgets, &i18n::text(UiMessage::ProjectCreated));
                    open_project(&widgets, &state, &root);
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
        },
    );
}

fn populate_post_list(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    post_summaries: &[PostSummary],
) {
    while let Some(child) = widgets.post_list.first_child() {
        widgets.post_list.remove(&child);
    }
    for post in post_summaries {
        let unsaved = state.borrow().unsaved_documents.contains_key(&post.id);
        let label = gtk::Label::builder()
            .label(post_list_label(&post.relative_path, unsaved).as_str())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .tooltip_text(post_list_tooltip(&post.relative_path, unsaved).as_str())
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(10)
            .margin_end(10)
            .build();
        widgets.post_list.append(&label);
    }
}

fn post_list_label(relative_path: &str, unsaved: bool) -> String {
    if unsaved {
        format!("* {relative_path}")
    } else {
        relative_path.to_owned()
    }
}

fn post_list_tooltip(relative_path: &str, unsaved: bool) -> String {
    if unsaved {
        format!("{relative_path}（有未保存修改）")
    } else {
        relative_path.to_owned()
    }
}

fn update_post_marker(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, post_id: &str) {
    let (index, relative_path, unsaved) = {
        let state = state.borrow();
        let Some((index, post)) = state
            .posts
            .iter()
            .enumerate()
            .find(|(_, post)| post.id == post_id)
        else {
            return;
        };
        (
            index,
            post.relative_path.clone(),
            state.unsaved_documents.contains_key(post_id),
        )
    };
    let Ok(index) = i32::try_from(index) else {
        return;
    };
    let Some(row) = widgets.post_list.row_at_index(index) else {
        return;
    };
    let Some(label) = row
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    else {
        return;
    };
    label.set_label(&post_list_label(&relative_path, unsaved));
    label.set_tooltip_text(Some(&post_list_tooltip(&relative_path, unsaved)));
}

fn load_document(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, post_id: &str) {
    let (context, unsaved_document, already_open) = {
        let state = state.borrow();
        let context = state.project.clone();
        let unsaved_document = state.unsaved_documents.get(post_id).cloned();
        let already_open = state
            .document
            .as_ref()
            .is_some_and(|document| document.id == post_id);
        (context, unsaved_document, already_open)
    };
    if already_open || state.borrow().busy {
        return;
    }
    let Some(context) = context else {
        return;
    };
    // 文章切换后自动保存队列仍可能继续写入旧文章，因此必须在切换前把当前
    // 快照排入队列，避免延迟计时器读取到新文章。
    drafts::flush_current(widgets, state);
    if let Some(document) = unsaved_document {
        display_document(widgets, state, document, true);
        return;
    }
    set_busy(widgets, state, true, "正在打开文章…");
    let post_id = post_id.to_owned();
    let recovery_context = context.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || posts::read_post(&context, &post_id),
        move |result| {
            match result {
                Ok(document) => {
                    let epoch = display_document(&widgets, &state, document.clone(), false);
                    drafts::inspect_loaded_document(
                        &widgets,
                        &state,
                        recovery_context,
                        document,
                        epoch,
                    );
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
        },
    );
}

fn display_document(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    document: PostDocument,
    dirty: bool,
) -> u64 {
    {
        let mut state = state.borrow_mut();
        state.loading_buffer = true;
        state.document = Some(document.clone());
        state.dirty = dirty;
        state.document_epoch = state.document_epoch.wrapping_add(1);
    }
    widgets.buffer.set_text(&document.body);
    widgets.editor.grab_focus();
    let status = if dirty {
        format!("{} · 未保存", document.relative_path)
    } else {
        document.relative_path.clone()
    };
    widgets.status_label.set_label(&status);
    let mut editor_state = state.borrow_mut();
    editor_state.loading_buffer = false;
    let epoch = editor_state.document_epoch;
    let context = editor_state.project.clone();
    drop(editor_state);
    let project_name = context
        .as_ref()
        .map(|context| project_folder_name(&context.root));
    let title = document_window_title(&document.relative_path, project_name.as_deref(), dirty);
    widgets.window.set_title(Some(&title));
    if let Some(context) = context {
        recent::touch_last_document(&context.root, &document.id);
        widgets
            .preview
            .set_document(context, document.id.clone(), epoch, document.body.clone());
    }
    frontmatter::refresh(widgets, state);
    // 内存快照切换不会经过异步打开回调里的 `set_busy(false)`，因此这里必须
    // 立即按当前文章的 dirty 状态同步保存按钮、编辑器和属性面板。
    sync_controls(widgets, state);
    #[cfg(feature = "e2e")]
    if std::env::var_os("CLOUDSTACK_E2E_PROPERTIES_OPEN").is_some() {
        widgets.frontmatter_split.set_show_sidebar(true);
    }
    epoch
}

fn project_folder_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

fn document_window_title(relative_path: &str, project_name: Option<&str>, dirty: bool) -> String {
    let document_name = if dirty {
        format!("* {relative_path}")
    } else {
        relative_path.to_owned()
    };
    match project_name.filter(|name| !name.is_empty()) {
        Some(project_name) => format!("{document_name} — 云栈 CloudStack — {project_name}"),
        None => format!("{document_name} — 云栈 CloudStack"),
    }
}

fn save_document(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    save_document_then(widgets, state, None);
}

fn save_document_then(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    on_saved: Option<Box<dyn FnOnce()>>,
) {
    // 交互式保存的“单飞”约束：手动保存通过本路径发起并先置 busy=true，
    // GTK 主线程串行分发事件下，第二次保存不会在第一次完成前插入执行。
    // 后续若放开并发或多任务保存，需引入保存任务身份与乱序 completion 防护。
    if state.borrow().busy {
        return;
    }
    let (context, document) = {
        let state = state.borrow();
        let (Some(context), Some(document)) = (&state.project, &state.document) else {
            return;
        };
        (context.clone(), document.clone())
    };

    let buffer = &widgets.buffer;
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    let raw_frontmatter = document.raw_frontmatter.clone();
    let body = text.to_string();
    let (saved_document_epoch, saved_generation) = {
        let state = state.borrow();
        (state.document_epoch, state.edit_generation)
    };
    let task_frontmatter = raw_frontmatter.clone();
    let task_body = body.clone();
    let task_context = context.clone();
    let task_document = document.clone();
    // 先把最新编辑快照排入草稿队列。保存成功后的删除也进入同一队列，
    // 因而不会被较早启动的自动草稿写入反超。
    drafts::flush_current(widgets, state);
    set_busy(widgets, state, true, "正在保存…");
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            posts::write_post(
                &task_context,
                &task_document.id,
                task_frontmatter.as_deref(),
                &task_body,
                &task_document.revision,
            )
        },
        move |result| {
            let mut continue_after_save = false;
            match result {
                Ok(revision) => {
                    let outcome = {
                        let mut editor_state_ref = state.borrow_mut();
                        let editor_state: &mut EditorState = &mut editor_state_ref;
                        let current_id = editor_state
                            .document
                            .as_ref()
                            .map(|current| current.id.as_str());
                        let outcome = classify_save_completion(
                            current_id,
                            &document.id,
                            editor_state.document_epoch,
                            saved_document_epoch,
                            editor_state.edit_generation,
                            saved_generation,
                        );
                        apply_successful_save(
                            outcome,
                            &mut editor_state.document,
                            &mut editor_state.unsaved_documents,
                            &mut editor_state.dirty,
                            &document.id,
                            &revision,
                            raw_frontmatter.clone(),
                            body.clone(),
                        );
                        if matches!(outcome, SaveCompletionOutcome::Clean) {
                            if let Err(error) = editor_state.pending_assets.reconcile_saved_post(
                                &context.root,
                                &document.id,
                                &body,
                            ) {
                                log::warn!("保存后清理待提交图片失败：{error}");
                            }
                        }
                        outcome
                    };
                    match outcome {
                        SaveCompletionOutcome::Clean => {
                            update_post_marker(&widgets, &state, &document.id);
                            widgets.status_label.set_label(&document.relative_path);
                            toast(&widgets, &i18n::text(UiMessage::SaveSuccess));
                            drafts::delete_for_post(&widgets, &state, context, document.id);
                            continue_after_save = true;
                        }
                        SaveCompletionOutcome::RevisionOnly => {
                            toast(&widgets, &i18n::text(UiMessage::SaveSuccessWithNewerEdits));
                        }
                        SaveCompletionOutcome::NotCurrent => {}
                    }
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
            if continue_after_save {
                git_panel::refresh(&widgets, &state);
                if let Some(on_saved) = on_saved {
                    on_saved();
                }
            }
        },
    );
}

fn mark_document_dirty(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let post_id = {
        let body = widgets
            .buffer
            .text(
                &widgets.buffer.start_iter(),
                &widgets.buffer.end_iter(),
                true,
            )
            .to_string();
        let mut state = state.borrow_mut();
        let Some(document) = state.document.clone() else {
            return;
        };
        state.dirty = true;
        state.edit_generation = state.edit_generation.wrapping_add(1);
        let mut snapshot = document;
        snapshot.body = body;
        let post_id = snapshot.id.clone();
        state.unsaved_documents.insert(post_id.clone(), snapshot);
        post_id
    };
    update_post_marker(widgets, state, &post_id);
    sync_controls(widgets, state);
    if let Some(document) = &state.borrow().document {
        widgets
            .status_label
            .set_label(&format!("{} · 未保存", document.relative_path));
    }
    drafts::schedule(widgets, state);
}

fn set_busy(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, busy: bool, message: &str) {
    state.borrow_mut().busy = busy;
    if busy && !message.is_empty() {
        widgets.status_label.set_label(message);
    } else if !busy {
        let editor_state = state.borrow();
        if let Some(document) = &editor_state.document {
            let suffix = if editor_state.dirty {
                " · 未保存"
            } else {
                ""
            };
            widgets
                .status_label
                .set_label(&format!("{}{suffix}", document.relative_path));
        } else if editor_state.project.is_some() {
            widgets
                .status_label
                .set_label(&i18n::text(UiMessage::ProjectOpenedStatus {
                    count: editor_state.posts.len(),
                }));
        } else {
            widgets
                .status_label
                .set_label(&i18n::text(UiMessage::ReadyStatus));
        }
    }
    sync_controls(widgets, state);
}

/// `sync_controls` 要往控件上写的所有布尔值，跟 `EditorState` 的具体形状
/// 解耦——只依赖 `controls_for` 算出来的这份纯数据快照，方便集中测试
/// busy/document/dirty 对控件可用性的影响，不需要真的搭一个 GTK 窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ControlModel {
    /// 打开项目入口（顶栏按钮 + 欢迎页），有未保存文章或正忙时都要挡住。
    open_enabled: bool,
    home_enabled: bool,
    new_post_enabled: bool,
    rename_enabled: bool,
    delete_enabled: bool,
    save_enabled: bool,
    editor_editable: bool,
    editor_cursor_visible: bool,
    frontmatter_panel_enabled: bool,
    properties_enabled: bool,
    git_project_available: bool,
    git_dirty: bool,
    git_primary_action: git_panel::EffectivePrimaryAction,
    post_list_enabled: bool,
    /// 只在没有文章展示时才强制收起 frontmatter 侧栏；文章重新出现时不会
    /// 由这里负责重新展开（那是 toggle-properties 动作自己的事）。
    hide_frontmatter_sidebar: bool,
}

fn controls_for(state: &EditorState) -> ControlModel {
    let has_project = state.project.is_some();
    let has_document = state.document.is_some();
    let has_unsaved_documents = !state.unsaved_documents.is_empty();
    // 有未保存文章挂在内存里时，跳项目/新建文章这类会离开当前上下文的
    // 操作要挡住；已经在编辑的文档本身（保存、frontmatter、post_list）
    // 只看 busy，不受这条限制。
    let stable = !state.busy && !has_unsaved_documents;
    ControlModel {
        open_enabled: stable,
        home_enabled: has_project && stable,
        new_post_enabled: has_project && stable,
        rename_enabled: has_document && stable,
        delete_enabled: has_document && stable,
        save_enabled: has_document && state.dirty && !state.busy,
        editor_editable: has_document && !state.busy,
        editor_cursor_visible: has_document,
        frontmatter_panel_enabled: has_document && !state.busy,
        properties_enabled: has_document && !state.busy,
        git_project_available: has_project,
        git_dirty: state.dirty,
        git_primary_action: git_panel::effective_primary_action(
            state.git_snapshot.as_ref(),
            state.busy,
            state.unsaved_documents.len(),
        ),
        post_list_enabled: has_project && !state.busy,
        hide_frontmatter_sidebar: !has_document,
    }
}

fn render_controls(widgets: &Widgets, model: &ControlModel) {
    widgets.open_button.set_sensitive(model.open_enabled);
    widgets.welcome_page.set_open_sensitive(model.open_enabled);
    widgets.home_button.set_sensitive(model.home_enabled);
    widgets.new_button.set_sensitive(model.new_post_enabled);
    widgets.rename_button.set_sensitive(model.rename_enabled);
    widgets.delete_button.set_sensitive(model.delete_enabled);
    widgets.save_button.set_sensitive(model.save_enabled);
    widgets.editor.set_editable(model.editor_editable);
    widgets
        .editor
        .set_cursor_visible(model.editor_cursor_visible);
    widgets
        .frontmatter_panel
        .set_sensitive(model.frontmatter_panel_enabled);
    widgets
        .properties_button
        .set_sensitive(model.properties_enabled);
    widgets
        .git_panel
        .set_project_available(model.git_project_available);
    widgets.git_panel.reflect_unsaved_editor(model.git_dirty);
    widgets
        .git_panel
        .set_primary_action(model.git_primary_action);
    widgets.post_list.set_sensitive(model.post_list_enabled);
    if model.hide_frontmatter_sidebar {
        widgets.frontmatter_split.set_show_sidebar(false);
    }
}

fn sync_controls(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let model = controls_for(&state.borrow());
    render_controls(widgets, &model);
}

fn has_unsaved_documents(state: &Rc<RefCell<EditorState>>) -> bool {
    !state.borrow().unsaved_documents.is_empty()
}

fn unsaved_document_count(state: &Rc<RefCell<EditorState>>) -> usize {
    state.borrow().unsaved_documents.len()
}

fn connect_close_guard(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    widgets.window.clone().connect_close_request(move |_| {
        if state.borrow().busy {
            toast(&widgets, "文件操作正在进行，请稍候再关闭");
            return glib::Propagation::Stop;
        }
        if !has_unsaved_documents(&state) {
            if let Err(error) = state.borrow_mut().pending_assets.discard_all() {
                log::warn!("退出时清理待提交图片失败：{error}");
            }
            return glib::Propagation::Proceed;
        }

        let unsaved_documents = {
            let state = state.borrow();
            let mut documents = state
                .unsaved_documents
                .values()
                .map(|document| document.relative_path.clone())
                .collect::<Vec<_>>();
            documents.sort();
            documents
        };
        let multiple = unsaved_documents.len() > 1;
        let body = if multiple {
            format!(
                "关闭窗口前请选择如何处理 {} 篇未保存文章：\n{}",
                unsaved_documents.len(),
                unsaved_documents.join("\n")
            )
        } else {
            format!(
                "文章“{}”尚未保存。关闭窗口前请选择如何处理它。",
                unsaved_documents
                    .first()
                    .map(String::as_str)
                    .unwrap_or("当前文章")
            )
        };
        let dialog = adw::AlertDialog::builder()
            .heading(if multiple {
                "有多篇文章尚未保存"
            } else {
                "文章尚未保存"
            })
            .body(body)
            .default_response("cancel")
            .close_response("cancel")
            .build();
        if multiple {
            dialog.add_responses(&[
                ("cancel", "继续编辑"),
                ("discard", "全部不保存"),
                ("save", "保存全部"),
            ]);
        } else {
            dialog.add_responses(&[
                ("cancel", "取消"),
                ("discard", "不保存并关闭"),
                ("save", "保存并关闭"),
            ]);
        }
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let close_widgets = widgets.clone();
        let close_state = Rc::clone(&state);
        dialog.connect_response(Some("discard"), move |_, _| {
            drafts::discard_and_close(&close_widgets, &close_state);
        });
        let save_widgets = widgets.clone();
        let save_state = Rc::clone(&state);
        dialog.connect_response(Some("save"), move |_, _| {
            drafts::save_and_close(&save_widgets, &save_state);
        });
        dialog.present(Some(&widgets.window));
        glib::Propagation::Stop
    });
}

/// `recent`/`settings` 子模块共用的应用数据目录解析——不含 legacy 目录回退，
/// 两者都是全新功能，没有旧版本数据要迁移。
fn app_data_dir() -> PathBuf {
    #[cfg(feature = "e2e")]
    if let Some(path) = std::env::var_os("CLOUDSTACK_E2E_DATA_DIR") {
        return PathBuf::from(path);
    }
    gtk::glib::user_data_dir().join(crate::APPLICATION_ID)
}

fn toast(widgets: &Widgets, message: &str) {
    widgets.toast_overlay.add_toast(adw::Toast::new(message));
}

fn show_error(widgets: &Widgets, message: &str) {
    let toast = adw::Toast::builder()
        .title(message)
        .timeout(8)
        .priority(adw::ToastPriority::High)
        .build();
    widgets.toast_overlay.add_toast(toast);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_split_preserves_a_visible_preview() {
        assert_eq!(initial_content_split_position(0, 1_400), 570);
        assert_eq!(initial_content_split_position(0, 700), 340);
        assert_eq!(initial_content_split_position(120, 400), 120);
        assert_eq!(initial_content_split_position(0, 0), 0);
    }

    #[test]
    fn unsaved_post_marker_is_visible_without_changing_the_path() {
        assert_eq!(post_list_label("nested/post.md", false), "nested/post.md");
        assert_eq!(post_list_label("nested/post.md", true), "* nested/post.md");
        assert_eq!(
            post_list_tooltip("nested/post.md", true),
            "nested/post.md（有未保存修改）"
        );
    }

    #[test]
    fn document_title_keeps_the_project_name_visible() {
        assert_eq!(
            document_window_title("nested/post.md", Some("test-blog"), false),
            "nested/post.md — 云栈 CloudStack — test-blog"
        );
        assert_eq!(
            document_window_title("nested/post.md", Some("test-blog"), true),
            "* nested/post.md — 云栈 CloudStack — test-blog"
        );
        assert_eq!(
            document_window_title("nested/post.md", None, false),
            "nested/post.md — 云栈 CloudStack"
        );
    }

    fn sample_project() -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("/tmp/test-blog"),
            content_root: PathBuf::from("/tmp/test-blog/content"),
            config_path: PathBuf::from("/tmp/test-blog/.cloudstack.json"),
            config: Default::default(),
        }
    }

    fn sample_document() -> PostDocument {
        PostDocument {
            id: "hello.md".into(),
            relative_path: "hello.md".into(),
            raw_frontmatter: None,
            body: String::new(),
            revision: "revision".into(),
        }
    }

    #[test]
    fn controls_for_default_state_only_allows_opening() {
        let model = controls_for(&EditorState::default());
        assert!(model.open_enabled);
        assert!(!model.home_enabled);
        assert!(!model.new_post_enabled);
        assert!(!model.rename_enabled);
        assert!(!model.delete_enabled);
        assert!(!model.save_enabled);
        assert!(!model.editor_editable);
        assert!(!model.editor_cursor_visible);
        assert!(!model.frontmatter_panel_enabled);
        assert!(!model.properties_enabled);
        assert!(!model.git_project_available);
        assert_eq!(
            model.git_primary_action,
            git_panel::EffectivePrimaryAction::None
        );
        assert!(!model.post_list_enabled);
        assert!(model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_project_without_document_enables_project_scoped_controls_only() {
        let state = EditorState {
            project: Some(sample_project()),
            ..Default::default()
        };
        let model = controls_for(&state);
        assert!(model.home_enabled);
        assert!(model.new_post_enabled);
        assert!(!model.rename_enabled, "没有打开的文章不该允许重命名");
        assert!(!model.delete_enabled, "没有打开的文章不该允许删除");
        assert!(!model.save_enabled);
        assert!(!model.editor_editable);
        assert!(model.git_project_available);
        assert_eq!(
            model.git_primary_action,
            git_panel::EffectivePrimaryAction::None
        );
        assert!(model.post_list_enabled);
        assert!(model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_dirty_document_enables_save() {
        let state = EditorState {
            project: Some(sample_project()),
            document: Some(sample_document()),
            dirty: true,
            ..Default::default()
        };
        let model = controls_for(&state);
        assert!(model.save_enabled);
        assert!(model.editor_editable);
        assert!(model.editor_cursor_visible);
        assert!(model.frontmatter_panel_enabled);
        assert!(model.properties_enabled);
        assert!(model.rename_enabled);
        assert!(model.delete_enabled);
        assert!(model.git_dirty);
        assert!(!model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_clean_document_disables_save_only() {
        let state = EditorState {
            project: Some(sample_project()),
            document: Some(sample_document()),
            dirty: false,
            ..Default::default()
        };
        let model = controls_for(&state);
        assert!(!model.save_enabled);
        assert!(model.editor_editable, "干净的文档仍然可以继续编辑");
    }

    #[test]
    fn controls_for_busy_disables_actionable_controls_but_not_read_only_ones() {
        let state = EditorState {
            project: Some(sample_project()),
            document: Some(sample_document()),
            dirty: true,
            busy: true,
            ..Default::default()
        };
        let model = controls_for(&state);
        assert!(!model.open_enabled);
        assert!(!model.home_enabled);
        assert!(!model.new_post_enabled);
        assert!(!model.rename_enabled);
        assert!(!model.delete_enabled);
        assert!(!model.save_enabled);
        assert!(!model.editor_editable);
        assert!(!model.frontmatter_panel_enabled);
        assert!(!model.properties_enabled);
        assert_eq!(
            model.git_primary_action,
            git_panel::EffectivePrimaryAction::None
        );
        assert!(!model.post_list_enabled);
        // busy 只挡"会触发新操作"的控件；纯展示性的判断不受它影响。
        assert!(
            model.editor_cursor_visible,
            "光标可见性只看有没有文档，跟 busy 无关"
        );
        assert!(
            model.git_project_available,
            "git 面板是否可用只看有没有项目，跟 busy 无关"
        );
        assert!(!model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_unsaved_documents_blocks_navigation_but_not_the_open_document() {
        let mut unsaved_documents = HashMap::new();
        unsaved_documents.insert("other.md".to_string(), sample_document());
        let state = EditorState {
            project: Some(sample_project()),
            document: Some(sample_document()),
            dirty: true,
            unsaved_documents,
            ..Default::default()
        };
        let model = controls_for(&state);
        // 有其它未保存的文章挂着：会离开当前上下文的操作要挡住……
        assert!(!model.open_enabled);
        assert!(!model.home_enabled);
        assert!(!model.new_post_enabled);
        assert!(!model.rename_enabled);
        assert!(!model.delete_enabled);
        // ……但当前正在编辑的这篇文章本身不受影响，不是 busy。
        assert!(model.save_enabled);
        assert!(model.editor_editable);
        assert!(model.frontmatter_panel_enabled);
        assert!(model.post_list_enabled);
        assert_eq!(
            model.git_primary_action,
            git_panel::EffectivePrimaryAction::SaveBeforeGit { unsaved_count: 1 }
        );
    }

    #[test]
    fn controls_for_clean_current_document_keeps_other_unsaved_state_separate() {
        let mut unsaved_documents = HashMap::new();
        unsaved_documents.insert("other.md".to_string(), sample_document());
        let state = EditorState {
            project: Some(sample_project()),
            document: Some(sample_document()),
            dirty: false,
            unsaved_documents,
            ..Default::default()
        };

        let model = controls_for(&state);
        assert!(
            !model.save_enabled,
            "当前文章干净时不应显示当前文章保存按钮"
        );
        assert!(!model.git_dirty, "Git 面板的未保存标记只反映当前文章");
        assert!(model.editor_editable);
        assert!(model.post_list_enabled);
        assert_eq!(
            model.git_primary_action,
            git_panel::EffectivePrimaryAction::SaveBeforeGit { unsaved_count: 1 }
        );
    }
}
