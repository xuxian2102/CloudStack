mod articles;
mod drafts;
mod frontmatter;
mod git_panel;
mod publish;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::model::{PostDocument, PostSummary, ProjectContext, RepositorySnapshot};
use cloudstack_core::services::assets::PendingAssetManager;
use cloudstack_core::services::{assets, posts, project};
use gtk::{gdk, gio, glib};
use sourceview::prelude::*;

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
    draft_queue: drafts::DraftQueue,
    pending_assets: PendingAssetManager,
    git_snapshot: Option<RepositorySnapshot>,
}

#[derive(Clone)]
struct Widgets {
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    post_list: gtk::ListBox,
    git_panel: git_panel::GitPanel,
    open_button: gtk::Button,
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
    publish_button: gtk::Button,
    save_button: gtk::Button,
    project_label: gtk::Label,
    status_label: gtk::Label,
}

pub fn present(application: &adw::Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }

    glib::set_application_name("云栈 CloudStack");
    sourceview::init();

    let state = Rc::new(RefCell::new(EditorState::default()));
    let widgets = build_window(application);
    connect_editor(&widgets, &state);
    connect_image_paste(&widgets, &state);
    connect_actions(application, &widgets, &state);
    connect_post_list(&widgets, &state);
    connect_close_guard(&widgets, &state);

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
        .title("云栈 CloudStack")
        .default_width(1180)
        .default_height(760)
        .build();

    let open_button = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("打开博客项目 (Ctrl+O)")
        .action_name("win.open-project")
        .build();
    let save_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("保存文章 (Ctrl+S)")
        .action_name("win.save")
        .sensitive(false)
        .build();
    let properties_button = gtk::Button::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text("文章属性")
        .action_name("win.toggle-properties")
        .sensitive(false)
        .build();
    let publish_button = gtk::Button::builder()
        .icon_name("send-to-symbolic")
        .tooltip_text("提交与推送")
        .action_name("win.publish")
        .sensitive(false)
        .build();

    let project_label = gtk::Label::builder()
        .label("尚未打开项目")
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(48)
        .build();

    let header = adw::HeaderBar::new();
    header.pack_start(&open_button);
    header.pack_start(&project_label);
    header.pack_end(&publish_button);
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
        .label("文章")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    let new_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("新建文章")
        .action_name("win.new-post")
        .sensitive(false)
        .build();
    let rename_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("重命名当前文章")
        .action_name("win.rename-post")
        .sensitive(false)
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("删除当前文章")
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
    buffer.set_text("打开一个博客项目以开始编辑。\n\n项目根目录需要包含 .cloudstack.json（旧项目也支持 .blog-editor.json）。\n");

    let editor_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&editor)
        .build();
    let toast_overlay = adw::ToastOverlay::new();
    let search_panel = SearchPanel::new(&buffer, &editor);
    let preview = crate::preview::Preview::new(&buffer, &editor, &editor_scroll, &toast_overlay);
    let status_label = gtk::Label::builder()
        .label("就绪")
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

    toast_overlay.set_child(Some(&split));
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toast_overlay));
    window.set_content(Some(&toolbar_view));

    Widgets {
        window,
        toast_overlay,
        post_list,
        git_panel,
        open_button,
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
        publish_button,
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
        if !is_paste || state.borrow().document.is_none() {
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
            if state.borrow().dirty {
                toast(&widgets, "请先保存当前文章，再切换到其他文章");
                return;
            }
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

fn select_project(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let state_snapshot = state.borrow();
    if state_snapshot.busy {
        return;
    }
    if state_snapshot.dirty {
        toast(widgets, "当前文章尚未保存，暂时不能切换项目");
        return;
    }
    drop(state_snapshot);

    let dialog = gtk::FileDialog::builder()
        .title("选择博客项目目录")
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
                    show_error(&widgets, "只能打开本地项目目录");
                    return;
                };
                open_project(&widgets, &state, &path);
            }
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(error) => show_error(&widgets, &format!("打开目录失败：{error}")),
        },
    );
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
        move || {
            let context = project::open_project(&path)?;
            let post_summaries = posts::list_posts(&context)?;
            Ok((context, post_summaries))
        },
        move |result| {
            match result {
                Ok((context, post_summaries)) => {
                    populate_post_list(&widgets, &post_summaries);
                    widgets
                        .project_label
                        .set_label(&context.root.display().to_string());
                    widgets
                        .status_label
                        .set_label(&format!("已打开项目 · {} 篇文章", post_summaries.len()));
                    let mut editor_state = state.borrow_mut();
                    editor_state.loading_buffer = true;
                    editor_state.project = Some(context);
                    editor_state.git_snapshot = None;
                    editor_state.posts = post_summaries;
                    editor_state.document = None;
                    editor_state.dirty = false;
                    editor_state.document_epoch = editor_state.document_epoch.wrapping_add(1);
                    let epoch = editor_state.document_epoch;
                    drop(editor_state);
                    widgets.buffer.set_text("从左侧选择一篇文章。\n");
                    state.borrow_mut().loading_buffer = false;
                    widgets.preview.clear(epoch);
                    widgets.frontmatter_split.set_show_sidebar(false);
                    widgets.window.set_title(Some("云栈 CloudStack"));
                    frontmatter::refresh(&widgets, &state);
                    git_panel::refresh(&widgets, &state);
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
            #[cfg(feature = "e2e")]
            if std::env::var_os("CLOUDSTACK_E2E_OPEN_FIRST").is_some() {
                let first_post = state.borrow().posts.first().map(|post| post.id.clone());
                if let Some(post_id) = first_post {
                    load_document(&widgets, &state, &post_id);
                }
            }
        },
    );
}

fn populate_post_list(widgets: &Widgets, post_summaries: &[PostSummary]) {
    while let Some(child) = widgets.post_list.first_child() {
        widgets.post_list.remove(&child);
    }
    for post in post_summaries {
        let label = gtk::Label::builder()
            .label(&post.relative_path)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .tooltip_text(&post.relative_path)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(10)
            .margin_end(10)
            .build();
        widgets.post_list.append(&label);
    }
}

fn load_document(widgets: &Widgets, state: &Rc<RefCell<EditorState>>, post_id: &str) {
    if state.borrow().busy {
        return;
    }
    let context = match state.borrow().project.clone() {
        Some(context) => context,
        None => return,
    };
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
                    let epoch = display_document(&widgets, &state, document.clone());
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
) -> u64 {
    {
        let mut state = state.borrow_mut();
        state.loading_buffer = true;
        state.document = Some(document.clone());
        state.dirty = false;
        state.document_epoch = state.document_epoch.wrapping_add(1);
    }
    widgets.buffer.set_text(&document.body);
    widgets.editor.grab_focus();
    widgets.status_label.set_label(&document.relative_path);
    widgets.window.set_title(Some(&format!(
        "{} — 云栈 CloudStack",
        document.relative_path
    )));
    let mut editor_state = state.borrow_mut();
    editor_state.loading_buffer = false;
    let epoch = editor_state.document_epoch;
    let context = editor_state.project.clone();
    drop(editor_state);
    if let Some(context) = context {
        widgets
            .preview
            .set_document(context, document.id.clone(), epoch, document.body.clone());
    }
    frontmatter::refresh(widgets, state);
    epoch
}

fn save_document(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    save_document_then(widgets, state, None);
}

fn save_document_then(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    on_saved: Option<Box<dyn FnOnce()>>,
) {
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
                    let saved_current = {
                        let mut editor_state = state.borrow_mut();
                        if !editor_state
                            .document
                            .as_ref()
                            .is_some_and(|current| current.id == document.id)
                        {
                            false
                        } else {
                            if let Some(current) = editor_state.document.as_mut() {
                                current.raw_frontmatter = raw_frontmatter;
                                current.body = body;
                                current.revision = revision;
                            }
                            editor_state
                                .pending_assets
                                .confirm_post(&context.root, &document.id);
                            editor_state.dirty = false;
                            true
                        }
                    };
                    if saved_current {
                        widgets.status_label.set_label(&document.relative_path);
                        toast(&widgets, "文章已保存");
                        drafts::delete_for_post(&widgets, &state, context, document.id);
                        continue_after_save = true;
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
    {
        let mut state = state.borrow_mut();
        if state.document.is_none() {
            return;
        }
        state.dirty = true;
    }
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
                .set_label(&format!("已打开项目 · {} 篇文章", editor_state.posts.len()));
        } else {
            widgets.status_label.set_label("就绪");
        }
    }
    sync_controls(widgets, state);
}

fn sync_controls(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let state = state.borrow();
    let has_project = state.project.is_some();
    let has_document = state.document.is_some();
    let stable = !state.busy && !state.dirty;
    widgets.open_button.set_sensitive(stable);
    widgets.new_button.set_sensitive(has_project && stable);
    widgets.rename_button.set_sensitive(has_document && stable);
    widgets.delete_button.set_sensitive(has_document && stable);
    widgets
        .save_button
        .set_sensitive(has_document && state.dirty && !state.busy);
    widgets.editor.set_editable(has_document && !state.busy);
    widgets.editor.set_cursor_visible(has_document);
    widgets
        .frontmatter_panel
        .set_sensitive(has_document && !state.busy);
    widgets
        .properties_button
        .set_sensitive(has_document && !state.busy);
    widgets
        .publish_button
        .set_sensitive(has_project && !state.busy);
    widgets.git_panel.set_project_available(has_project);
    widgets
        .git_panel
        .reflect_unsaved_editor(state.dirty, has_project && !state.busy);
    if !has_document {
        widgets.frontmatter_split.set_show_sidebar(false);
    }
}

fn connect_close_guard(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    widgets.window.clone().connect_close_request(move |_| {
        if state.borrow().busy {
            toast(&widgets, "文件操作正在进行，请稍候再关闭");
            return glib::Propagation::Stop;
        }
        if !state.borrow().dirty {
            if let Err(error) = state.borrow_mut().pending_assets.discard_all() {
                log::warn!("退出时清理待提交图片失败：{error}");
            }
            return glib::Propagation::Proceed;
        }

        let dialog = adw::AlertDialog::builder()
            .heading("放弃未保存的修改？")
            .body("关闭窗口会丢失当前文章中尚未保存的修改。")
            .default_response("cancel")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", "继续编辑"), ("discard", "放弃并关闭")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        let close_widgets = widgets.clone();
        let close_state = Rc::clone(&state);
        dialog.connect_response(Some("discard"), move |_, _| {
            drafts::discard_and_close(&close_widgets, &close_state);
        });
        dialog.present(Some(&widgets.window));
        glib::Propagation::Stop
    });
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
}
