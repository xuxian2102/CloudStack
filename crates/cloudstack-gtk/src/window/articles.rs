use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::services::posts;

use super::{
    display_document, drafts, populate_post_list, set_busy, show_error, toast, EditorState, Widgets,
};
use crate::tasks;

pub(super) fn show_create_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let context = {
        let state = state.borrow();
        if state.busy || state.dirty {
            return;
        }
        let Some(context) = &state.project else {
            return;
        };
        context.clone()
    };
    let extension = context
        .config
        .extensions
        .first()
        .map(String::as_str)
        .unwrap_or(".md");
    let id_entry = gtk::Entry::builder()
        .text(format!("new-post{extension}"))
        .placeholder_text(format!("article{extension}"))
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("新建文章")
        .body("输入相对于内容目录的文件名，也可以包含子目录。")
        .extra_child(&id_entry)
        .default_response("create")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "取消"), ("create", "创建")]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);

    let parent = widgets.window.clone();
    let callback_widgets = widgets.clone();
    let callback_entry = id_entry.clone();
    let state = Rc::clone(state);
    dialog.connect_response(Some("create"), move |_, _| {
        let id = callback_entry.text().trim().to_owned();
        if id.is_empty() {
            show_error(&callback_widgets, "文章文件名不能为空");
            return;
        }
        create_post(&callback_widgets, &state, context.clone(), id);
    });
    dialog.present(Some(&parent));
    id_entry.grab_focus();
    id_entry.select_region(0, -1);
}

fn create_post(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: cloudstack_core::ProjectContext,
    id: String,
) {
    set_busy(widgets, state, true, "正在创建文章…");
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            let document = posts::create_post(&context, &id, None, "")?;
            let summaries = posts::list_posts(&context)?;
            Ok((document, summaries))
        },
        move |result| {
            match result {
                Ok((document, summaries)) => {
                    populate_post_list(&widgets, &summaries);
                    state.borrow_mut().posts = summaries;
                    display_document(&widgets, &state, document);
                    toast(&widgets, "文章已创建");
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
        },
    );
}

pub(super) fn show_rename_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let (context, document) = {
        let state = state.borrow();
        if state.busy || state.dirty {
            return;
        }
        let (Some(context), Some(document)) = (&state.project, &state.document) else {
            return;
        };
        (context.clone(), document.clone())
    };
    let id_entry = gtk::Entry::builder()
        .text(&document.id)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("重命名文章")
        .body("文章引用的同名目录图片会一并安全移动并更新 Markdown 路径。")
        .extra_child(&id_entry)
        .default_response("rename")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "取消"), ("rename", "重命名")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);

    let parent = widgets.window.clone();
    let callback_widgets = widgets.clone();
    let callback_entry = id_entry.clone();
    let state = Rc::clone(state);
    dialog.connect_response(Some("rename"), move |_, _| {
        let new_id = callback_entry.text().trim().to_owned();
        if new_id.is_empty() {
            show_error(&callback_widgets, "文章文件名不能为空");
            return;
        }
        if new_id == document.id {
            return;
        }
        rename_post(
            &callback_widgets,
            &state,
            context.clone(),
            document.clone(),
            new_id,
        );
    });
    dialog.present(Some(&parent));
    id_entry.grab_focus();
    id_entry.select_region(0, -1);
}

fn rename_post(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: cloudstack_core::ProjectContext,
    document: cloudstack_core::PostDocument,
    new_id: String,
) {
    set_busy(widgets, state, true, "正在重命名文章…");
    let old_id = document.id.clone();
    let task_context = context.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            let renamed =
                posts::rename_post(&task_context, &document.id, &new_id, &document.revision)?;
            let summaries = posts::list_posts(&task_context)?;
            Ok((renamed, summaries))
        },
        move |result| {
            match result {
                Ok((renamed, summaries)) => {
                    populate_post_list(&widgets, &summaries);
                    let mut editor_state = state.borrow_mut();
                    editor_state.posts = summaries;
                    editor_state
                        .pending_assets
                        .forget_post(&context.root, &old_id);
                    drop(editor_state);
                    drafts::delete_for_post(&widgets, &state, context.clone(), old_id.clone());
                    display_document(&widgets, &state, renamed);
                    toast(&widgets, "文章已重命名");
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
        },
    );
}

pub(super) fn show_delete_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let (context, document) = {
        let state = state.borrow();
        if state.busy || state.dirty {
            return;
        }
        let (Some(context), Some(document)) = (&state.project, &state.document) else {
            return;
        };
        (context.clone(), document.clone())
    };
    let dialog = adw::AlertDialog::builder()
        .heading("把文章移到废纸篓？")
        .body(format!(
            "{} 及正文实际引用的同名目录图片将移到系统废纸篓。",
            document.relative_path
        ))
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "取消"), ("delete", "移到废纸篓")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);

    let parent = widgets.window.clone();
    let callback_widgets = widgets.clone();
    let state = Rc::clone(state);
    dialog.connect_response(Some("delete"), move |_, _| {
        delete_post(&callback_widgets, &state, context.clone(), document.clone());
    });
    dialog.present(Some(&parent));
}

fn delete_post(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    context: cloudstack_core::ProjectContext,
    document: cloudstack_core::PostDocument,
) {
    set_busy(widgets, state, true, "正在删除文章…");
    let post_id = document.id.clone();
    let task_context = context.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            posts::delete_post(&task_context, &document.id, &document.revision)?;
            posts::list_posts(&task_context)
        },
        move |result| {
            match result {
                Ok(summaries) => {
                    populate_post_list(&widgets, &summaries);
                    let mut editor_state = state.borrow_mut();
                    editor_state.posts = summaries;
                    editor_state.document = None;
                    editor_state.dirty = false;
                    editor_state.document_epoch = editor_state.document_epoch.wrapping_add(1);
                    let epoch = editor_state.document_epoch;
                    editor_state
                        .pending_assets
                        .forget_post(&context.root, &post_id);
                    editor_state.loading_buffer = true;
                    drop(editor_state);
                    drafts::delete_for_post(&widgets, &state, context.clone(), post_id.clone());
                    widgets.buffer.set_text("从左侧选择一篇文章。\n");
                    state.borrow_mut().loading_buffer = false;
                    widgets.preview.clear(epoch);
                    super::frontmatter::refresh(&widgets, &state);
                    widgets.window.set_title(Some("云栈 CloudStack"));
                    toast(&widgets, "文章已移到系统废纸篓");
                }
                Err(error) => show_error(&widgets, &error.to_string()),
            }
            set_busy(&widgets, &state, false, "");
        },
    );
}
