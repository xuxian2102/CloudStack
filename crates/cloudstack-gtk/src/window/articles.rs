use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::services::posts;

use super::{
    app_data_dir, display_document, drafts, git_panel, has_unsaved_documents, populate_post_list,
    set_busy, show_error, show_user_facing_error, toast, EditorState, Widgets,
};
use crate::i18n::{self, UiMessage};
use crate::tasks;

pub(super) fn show_create_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let has_unsaved = has_unsaved_documents(state);
    let context = {
        let state = state.borrow();
        if state.session.busy || has_unsaved {
            return;
        }
        let Some(context) = &state.session.project else {
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
    let default_name = i18n::text(UiMessage::CreateArticleDefaultName {
        extension: extension.to_owned(),
    });
    let placeholder = i18n::text(UiMessage::CreateArticlePlaceholder {
        extension: extension.to_owned(),
    });
    let id_entry = gtk::Entry::builder()
        .text(default_name)
        .placeholder_text(placeholder)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(i18n::text(UiMessage::CreateArticleHeading))
        .body(i18n::text(UiMessage::CreateArticleBody))
        .extra_child(&id_entry)
        .default_response("create")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", i18n::text(UiMessage::Cancel).as_str()),
        ("create", i18n::text(UiMessage::Create).as_str()),
    ]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);

    let parent = widgets.window.clone();
    let callback_widgets = widgets.clone();
    let callback_entry = id_entry.clone();
    let state = Rc::clone(state);
    dialog.connect_response(Some("create"), move |_, _| {
        let id = callback_entry.text().trim().to_owned();
        if id.is_empty() {
            let message = i18n::text(UiMessage::ArticleFilenameEmpty);
            show_error(&callback_widgets, &message);
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
    let busy_message = i18n::text(UiMessage::CreatingArticleStatus);
    set_busy(widgets, state, true, &busy_message);
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
                    populate_post_list(&widgets, &state, &summaries);
                    state.borrow_mut().session.posts = summaries;
                    display_document(&widgets, &state, document, false);
                    toast(&widgets, &i18n::text(UiMessage::ArticleCreated));
                }
                Err(error) => super::show_user_facing_error(&widgets, &error),
            }
            set_busy(&widgets, &state, false, "");
            git_panel::refresh(&widgets, &state);
        },
    );
}

pub(super) fn show_rename_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let has_unsaved = has_unsaved_documents(state);
    let (context, document) = {
        let state = state.borrow();
        if state.session.busy || has_unsaved {
            return;
        }
        let (Some(context), Some(document)) = (&state.session.project, &state.session.document)
        else {
            return;
        };
        (context.clone(), document.clone())
    };
    let id_entry = gtk::Entry::builder()
        .text(&document.id)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(i18n::text(UiMessage::RenameArticleHeading))
        .body(i18n::text(UiMessage::RenameArticleBody))
        .extra_child(&id_entry)
        .default_response("rename")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", i18n::text(UiMessage::Cancel).as_str()),
        ("rename", i18n::text(UiMessage::Rename).as_str()),
    ]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);

    let parent = widgets.window.clone();
    let callback_widgets = widgets.clone();
    let callback_entry = id_entry.clone();
    let state = Rc::clone(state);
    dialog.connect_response(Some("rename"), move |_, _| {
        let new_id = callback_entry.text().trim().to_owned();
        if new_id.is_empty() {
            let message = i18n::text(UiMessage::ArticleFilenameEmpty);
            show_error(&callback_widgets, &message);
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
    let busy_message = i18n::text(UiMessage::RenamingArticleStatus);
    set_busy(widgets, state, true, &busy_message);
    let old_id = document.id.clone();
    let task_context = context.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || {
            let renamed = posts::rename_post(
                &task_context,
                &document.id,
                &new_id,
                &document.revision,
                &app_data_dir(),
            )?;
            let summaries = posts::list_posts(&task_context)?;
            Ok((renamed, summaries))
        },
        move |result| {
            match result {
                Ok((renamed, summaries)) => {
                    populate_post_list(&widgets, &state, &summaries);
                    let mut editor_state = state.borrow_mut();
                    editor_state.session.posts = summaries;
                    editor_state
                        .pending_assets
                        .forget_post(&context.root, &old_id);
                    drop(editor_state);
                    drafts::delete_for_post(&widgets, &state, context.clone(), old_id.clone());
                    display_document(&widgets, &state, renamed, false);
                    toast(&widgets, &i18n::text(UiMessage::ArticleRenamed));
                }
                Err(error) => super::show_user_facing_error(&widgets, &error),
            }
            set_busy(&widgets, &state, false, "");
            git_panel::refresh(&widgets, &state);
        },
    );
}

pub(super) fn show_delete_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let has_unsaved = has_unsaved_documents(state);
    let (context, document) = {
        let state = state.borrow();
        if state.session.busy || has_unsaved {
            return;
        }
        let (Some(context), Some(document)) = (&state.session.project, &state.session.document)
        else {
            return;
        };
        (context.clone(), document.clone())
    };
    let delete_body = i18n::text(UiMessage::DeleteArticleBody {
        path: document.relative_path.clone(),
    });
    let dialog = adw::AlertDialog::builder()
        .heading(i18n::text(UiMessage::DeleteArticleHeading))
        .body(delete_body)
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", i18n::text(UiMessage::Cancel).as_str()),
        ("delete", i18n::text(UiMessage::MoveToTrash).as_str()),
    ]);
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
    let busy_message = i18n::text(UiMessage::DeletingArticleStatus);
    set_busy(widgets, state, true, &busy_message);
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
                    populate_post_list(&widgets, &state, &summaries);
                    let mut editor_state = state.borrow_mut();
                    editor_state.session.posts = summaries;
                    editor_state.session.document = None;
                    editor_state.session.dirty = false;
                    editor_state.session.document_epoch =
                        editor_state.session.document_epoch.wrapping_add(1);
                    let epoch = editor_state.session.document_epoch;
                    editor_state
                        .pending_assets
                        .forget_post(&context.root, &post_id);
                    editor_state.loading_buffer = true;
                    drop(editor_state);
                    drafts::delete_for_post(&widgets, &state, context.clone(), post_id.clone());
                    let empty_post_list = i18n::text(UiMessage::InitialPostListText);
                    widgets.buffer.set_text(&format!("{empty_post_list}\n"));
                    state.borrow_mut().loading_buffer = false;
                    widgets.preview.clear(epoch);
                    super::frontmatter::refresh(&widgets, &state);
                    widgets
                        .window
                        .set_title(Some(&i18n::text(UiMessage::AppName)));
                    toast(&widgets, &i18n::text(UiMessage::ArticleDeleted));
                }
                Err(error) => show_user_facing_error(&widgets, &error),
            }
            set_busy(&widgets, &state, false, "");
            git_panel::refresh(&widgets, &state);
        },
    );
}
