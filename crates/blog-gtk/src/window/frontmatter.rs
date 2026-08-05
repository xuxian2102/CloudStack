use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use blog_editor_core::model::FieldSpec;
use blog_editor_core::services::frontmatter::{self as frontmatter_service, FieldValue};

use super::{mark_document_dirty, EditorState, Widgets};

pub(super) fn refresh(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    while let Some(child) = widgets.frontmatter_panel.first_child() {
        widgets.frontmatter_panel.remove(&child);
    }

    let title = gtk::Label::builder()
        .label("Frontmatter")
        .xalign(0.0)
        .css_classes(["title-3"])
        .build();
    widgets.frontmatter_panel.append(&title);

    let (fields, post_id, raw_frontmatter) = {
        let state = state.borrow();
        let (Some(project), Some(document)) = (&state.project, &state.document) else {
            append_hint(widgets, "打开文章后可在这里编辑元数据。");
            return;
        };
        (
            project.config.frontmatter.fields.clone(),
            document.id.clone(),
            document.raw_frontmatter.clone(),
        )
    };

    let Some(raw_frontmatter) = raw_frontmatter else {
        append_hint(
            widgets,
            "这篇文章没有 Frontmatter，可以像普通 Markdown 文件一样编辑。",
        );
        let add_button = gtk::Button::builder()
            .label("添加 Frontmatter")
            .css_classes(["suggested-action"])
            .halign(gtk::Align::Start)
            .build();
        let callback_widgets = widgets.clone();
        let callback_state = Rc::clone(state);
        add_button.connect_clicked(move |_| {
            let raw = frontmatter_service::initial_for_post(&fields, &post_id).unwrap_or_default();
            if let Some(document) = callback_state.borrow_mut().document.as_mut() {
                document.raw_frontmatter = Some(raw);
            }
            mark_document_dirty(&callback_widgets, &callback_state);
            refresh(&callback_widgets, &callback_state);
        });
        widgets.frontmatter_panel.append(&add_button);
        return;
    };

    if fields.is_empty() {
        append_hint(
            widgets,
            "项目尚未配置可编辑字段；现有 Frontmatter 会在保存时原样保留。",
        );
        append_remove_button(widgets, state);
        return;
    }

    let states = match frontmatter_service::read_fields(&raw_frontmatter, &fields) {
        Ok(states) => states,
        Err(error) => {
            let label = gtk::Label::builder()
                .label(format!(
                    "无法生成表单：{error}\n请用其他工具修正 YAML 后重新打开文章。"
                ))
                .xalign(0.0)
                .wrap(true)
                .css_classes(["error"])
                .build();
            widgets.frontmatter_panel.append(&label);
            append_remove_button(widgets, state);
            return;
        }
    };

    let group = adw::PreferencesGroup::new();
    for (field, field_state) in fields.iter().zip(states) {
        match field_state.value {
            FieldValue::Boolean(value) => {
                let row = adw::SwitchRow::builder()
                    .title(field_title(field))
                    .active(value)
                    .build();
                let callback_widgets = widgets.clone();
                let callback_state = Rc::clone(state);
                let name = field.name.clone();
                row.connect_active_notify(move |row| {
                    update_field(
                        &callback_widgets,
                        &callback_state,
                        &name,
                        FieldValue::Boolean(row.is_active()),
                    );
                });
                group.add(&row);
            }
            FieldValue::Text(value) if field.field_type == "date" => {
                group.add(&build_date_row(widgets, state, field, &value));
            }
            FieldValue::Text(value) => {
                let row = adw::EntryRow::builder()
                    .title(field_title(field))
                    .text(value)
                    .build();
                let callback_widgets = widgets.clone();
                let callback_state = Rc::clone(state);
                let name = field.name.clone();
                row.connect_changed(move |row| {
                    update_field(
                        &callback_widgets,
                        &callback_state,
                        &name,
                        FieldValue::Text(row.text().to_string()),
                    );
                });
                group.add(&row);
            }
            FieldValue::Tags(values) => {
                let row = adw::EntryRow::builder()
                    .title(field_title(field))
                    .text(values.join(", "))
                    .build();
                row.set_tooltip_text(Some("使用英文逗号分隔多个标签"));
                let callback_widgets = widgets.clone();
                let callback_state = Rc::clone(state);
                let name = field.name.clone();
                row.connect_changed(move |row| {
                    update_field(
                        &callback_widgets,
                        &callback_state,
                        &name,
                        FieldValue::Tags(parse_tags(row.text().as_str())),
                    );
                });
                group.add(&row);
            }
        }
    }
    widgets.frontmatter_panel.append(&group);
    append_hint(
        widgets,
        "Frontmatter 不显示在正文中；未配置字段、注释和顺序会原样保留。",
    );
    append_remove_button(widgets, state);
}

fn build_date_row(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    field: &FieldSpec,
    value: &str,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(field_title(field))
        .subtitle(date_subtitle(value))
        .build();
    let calendar = gtk::Calendar::new();
    if let Some(date) = parse_date(value) {
        calendar.select_day(&date);
    }
    let clear_button = gtk::Button::builder()
        .label("清除日期")
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(8)
        .build();
    let picker = gtk::Box::new(gtk::Orientation::Vertical, 4);
    picker.append(&calendar);
    picker.append(&clear_button);
    let popover = gtk::Popover::new();
    popover.set_child(Some(&picker));
    let button = gtk::MenuButton::builder()
        .icon_name("x-office-calendar-symbolic")
        .tooltip_text("选择日期")
        .valign(gtk::Align::Center)
        .build();
    button.set_popover(Some(&popover));
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));

    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let callback_row = row.clone();
    let callback_button = button.clone();
    let name = field.name.clone();
    calendar.connect_day_selected(move |calendar| {
        let Ok(value) = calendar.date().format("%Y-%m-%d") else {
            return;
        };
        callback_row.set_subtitle(value.as_str());
        update_field(
            &callback_widgets,
            &callback_state,
            &name,
            FieldValue::Text(value.to_string()),
        );
        callback_button.popdown();
    });

    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let callback_row = row.clone();
    let callback_button = button.clone();
    let name = field.name.clone();
    clear_button.connect_clicked(move |_| {
        callback_row.set_subtitle("未设置");
        update_field(
            &callback_widgets,
            &callback_state,
            &name,
            FieldValue::Text(String::new()),
        );
        callback_button.popdown();
    });
    row
}

fn update_field(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    name: &str,
    value: FieldValue,
) {
    let raw_frontmatter = state
        .borrow()
        .document
        .as_ref()
        .and_then(|document| document.raw_frontmatter.clone());
    let Some(raw_frontmatter) = raw_frontmatter else {
        return;
    };
    match frontmatter_service::set_field(&raw_frontmatter, name, value) {
        Ok(raw) => {
            if let Some(document) = state.borrow_mut().document.as_mut() {
                document.raw_frontmatter = Some(raw);
            }
            mark_document_dirty(widgets, state);
        }
        Err(error) => super::show_error(widgets, &error.to_string()),
    }
}

fn append_remove_button(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let button = gtk::Button::builder()
        .label("移除 Frontmatter")
        .css_classes(["destructive-action"])
        .halign(gtk::Align::Start)
        .build();
    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    button.connect_clicked(move |_| {
        let dialog = adw::AlertDialog::builder()
            .heading("移除 Frontmatter？")
            .body("这会删除全部文章元数据，包括未配置字段和注释；正文不会受影响。")
            .default_response("cancel")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", "取消"), ("remove", "移除")]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let response_widgets = callback_widgets.clone();
        let response_state = Rc::clone(&callback_state);
        dialog.connect_response(Some("remove"), move |_, _| {
            if let Some(document) = response_state.borrow_mut().document.as_mut() {
                document.raw_frontmatter = None;
            }
            mark_document_dirty(&response_widgets, &response_state);
            refresh(&response_widgets, &response_state);
        });
        dialog.present(Some(&callback_widgets.window));
    });
    widgets.frontmatter_panel.append(&button);
}

fn parse_date(value: &str) -> Option<gtk::glib::DateTime> {
    if value.is_empty() {
        return None;
    }
    gtk::glib::DateTime::from_iso8601(&format!("{value}T00:00:00Z"), None).ok()
}

fn date_subtitle(value: &str) -> &str {
    if value.is_empty() {
        "未设置"
    } else {
        value
    }
}

fn append_hint(widgets: &Widgets, text: &str) {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    widgets.frontmatter_panel.append(&label);
}

fn field_title(field: &FieldSpec) -> String {
    if field.required {
        format!("{} · 必填", field.name)
    } else {
        field.name.clone()
    }
}

fn parse_tags(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .filter(|tag| seen.insert((*tag).to_owned()))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_trimmed_deduplicated_and_empty_values_removed() {
        assert_eq!(
            parse_tags(" rust, GTK, rust, ,中文 "),
            ["rust", "GTK", "中文"]
        );
    }

    #[test]
    fn empty_date_has_a_clear_subtitle() {
        assert_eq!(date_subtitle(""), "未设置");
        assert_eq!(date_subtitle("2026-08-05"), "2026-08-05");
    }
}
