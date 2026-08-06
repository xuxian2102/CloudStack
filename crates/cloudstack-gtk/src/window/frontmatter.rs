use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::model::FieldSpec;
use cloudstack_core::services::frontmatter::{self as frontmatter_service, FieldValue};

use super::{mark_document_dirty, EditorState, Widgets};
use crate::i18n::{self, UiMessage};

pub(super) fn refresh(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    while let Some(child) = widgets.frontmatter_panel.first_child() {
        widgets.frontmatter_panel.remove(&child);
    }

    let title = gtk::Label::builder()
        .label(i18n::text(UiMessage::FrontmatterTitle))
        .xalign(0.0)
        .css_classes(["title-3"])
        .build();
    widgets.frontmatter_panel.append(&title);

    let (fields, post_id, raw_frontmatter) = {
        let state = state.borrow();
        let (Some(project), Some(document)) = (&state.session.project, &state.session.document)
        else {
            append_hint(widgets, &i18n::text(UiMessage::FrontmatterOpenHint));
            return;
        };
        (
            project.config.frontmatter.fields.clone(),
            document.id.clone(),
            document.raw_frontmatter.clone(),
        )
    };

    let Some(raw_frontmatter) = raw_frontmatter else {
        append_hint(widgets, &i18n::text(UiMessage::NoFrontmatterHint));
        let add_button = gtk::Button::builder()
            .label(i18n::text(UiMessage::AddFrontmatter))
            .css_classes(["suggested-action"])
            .halign(gtk::Align::Start)
            .build();
        let callback_widgets = widgets.clone();
        let callback_state = Rc::clone(state);
        add_button.connect_clicked(move |_| {
            let raw = frontmatter_service::initial_for_post(&fields, &post_id).unwrap_or_default();
            if let Some(document) = callback_state.borrow_mut().session.document.as_mut() {
                document.raw_frontmatter = Some(raw);
            }
            mark_document_dirty(&callback_widgets, &callback_state);
            refresh(&callback_widgets, &callback_state);
        });
        widgets.frontmatter_panel.append(&add_button);
        return;
    };

    if fields.is_empty() {
        append_hint(widgets, &i18n::text(UiMessage::NoEditableFieldsHint));
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
                group.add(&build_tags_row(widgets, state, field, values));
            }
        }
    }
    widgets.frontmatter_panel.append(&group);
    append_hint(widgets, &i18n::text(UiMessage::FrontmatterHiddenHint));
    append_remove_button(widgets, state);
}

fn build_date_row(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    field: &FieldSpec,
    value: &str,
) -> adw::PreferencesRow {
    let mut today = gtk::glib::DateTime::now_local()
        .map(|date| date.ymd())
        .unwrap_or((2000, 1, 1));
    if today.0 < 2000 {
        today = (2000, 1, 1);
    }
    let (year, month, day) = parse_date_parts(value, today).unwrap_or(today);
    let year_unit = i18n::text(UiMessage::YearUnit);
    let month_unit = i18n::text(UiMessage::MonthUnit);
    let day_unit = i18n::text(UiMessage::DayUnit);
    let year_values = (2000..=today.0.max(2000))
        .rev()
        .map(|value| format!("{value} {year_unit}"))
        .collect::<Vec<_>>();
    let year_refs = year_values.iter().map(String::as_str).collect::<Vec<_>>();
    let year_input = gtk::DropDown::from_strings(&year_refs);
    year_input.set_selected(u32::try_from(today.0.saturating_sub(year)).unwrap_or(0));
    let max_month = u32::try_from(if year == today.0 { today.1 } else { 12 }).unwrap_or(1);
    let month_values = (1..=max_month)
        .map(|value| format!("{value} {month_unit}"))
        .collect::<Vec<_>>();
    let month_refs = month_values.iter().map(String::as_str).collect::<Vec<_>>();
    let month_model = gtk::StringList::new(&month_refs);
    let month_input = gtk::DropDown::builder().model(&month_model).build();
    month_input.set_selected(u32::try_from(month.saturating_sub(1)).unwrap_or(0));
    let initial_days = day_strings(year, month, today, &day_unit);
    let initial_day_refs = initial_days.iter().map(String::as_str).collect::<Vec<_>>();
    let day_model = gtk::StringList::new(&initial_day_refs);
    let day_input = gtk::DropDown::builder().model(&day_model).build();
    day_input.set_selected(u32::try_from(day.saturating_sub(1)).unwrap_or(0));

    let title = gtk::Label::builder()
        .label(field_title(field))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    let date_label = date_subtitle(value)
        .map(str::to_owned)
        .unwrap_or_else(|| i18n::text(UiMessage::DateUnset));
    let status = gtk::Label::builder()
        .label(date_label)
        .xalign(1.0)
        .css_classes(["dim-label"])
        .build();
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.append(&title);
    heading.append(&status);
    let selectors = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    selectors.append(&year_input);
    selectors.append(&month_input);
    selectors.append(&day_input);
    let clear_button = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(i18n::text(UiMessage::ClearDateTooltip))
        .css_classes(["flat"])
        .build();
    selectors.append(&clear_button);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&heading);
    content.append(&selectors);
    let row = adw::PreferencesRow::new();
    row.set_child(Some(&content));

    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let callback_status = status.clone();
    let name = field.name.clone();
    let callback_year = year_input.clone();
    let callback_month = month_input.clone();
    let callback_month_model = month_model;
    let callback_day = day_input.clone();
    let callback_day_model = day_model;
    let changing_model = Rc::new(Cell::new(false));
    let callback_guard = Rc::clone(&changing_model);
    let apply_date: Rc<dyn Fn()> = Rc::new(move || {
        if callback_guard.get() {
            return;
        }
        let year = today
            .0
            .saturating_sub(i32::try_from(callback_year.selected()).unwrap_or(0));
        sync_date_options(
            &callback_month,
            &callback_month_model,
            &callback_day,
            &callback_day_model,
            &callback_guard,
            year,
            today,
        );
        let month = i32::try_from(callback_month.selected()).unwrap_or(0) + 1;
        let day = callback_day.selected().saturating_add(1);
        let value = format!("{year:04}-{month:02}-{day:02}");
        callback_status.set_label(&value);
        update_field(
            &callback_widgets,
            &callback_state,
            &name,
            FieldValue::Text(value),
        );
    });
    let apply = Rc::clone(&apply_date);
    year_input.connect_selected_notify(move |_| apply());
    let apply = Rc::clone(&apply_date);
    month_input.connect_selected_notify(move |_| apply());
    day_input.connect_selected_notify(move |_| apply_date());

    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let callback_status = status;
    let name = field.name.clone();
    clear_button.connect_clicked(move |_| {
        callback_status.set_label(&i18n::text(UiMessage::DateUnset));
        update_field(
            &callback_widgets,
            &callback_state,
            &name,
            FieldValue::Text(String::new()),
        );
    });
    row
}

fn sync_date_options(
    month_dropdown: &gtk::DropDown,
    month_model: &gtk::StringList,
    day_dropdown: &gtk::DropDown,
    day_model: &gtk::StringList,
    changing: &Cell<bool>,
    year: i32,
    today: (i32, i32, i32),
) {
    if changing.get() {
        return;
    }
    changing.set(true);
    let month_unit = i18n::text(UiMessage::MonthUnit);
    let day_unit = i18n::text(UiMessage::DayUnit);
    let max_month = u32::try_from(if year == today.0 { today.1 } else { 12 }).unwrap_or(1);
    resize_numbered_options(month_dropdown, month_model, max_month, &month_unit);
    let month = i32::try_from(month_dropdown.selected()).unwrap_or(0) + 1;
    let calendar_max_day = days_in_month(year, month).unwrap_or(31);
    let max_day = if year == today.0 && month == today.1 {
        u32::try_from(today.2).unwrap_or(1)
    } else {
        calendar_max_day
    };
    resize_numbered_options(day_dropdown, day_model, max_day, &day_unit);
    changing.set(false);
}

fn resize_numbered_options(
    dropdown: &gtk::DropDown,
    model: &gtk::StringList,
    maximum: u32,
    unit: &str,
) {
    let current_len = model.n_items();
    let selected = dropdown.selected();
    if maximum < current_len {
        model.splice(maximum, current_len - maximum, &[]);
    } else if maximum > current_len {
        let additions = (current_len + 1..=maximum)
            .map(|value| format!("{value} {unit}"))
            .collect::<Vec<_>>();
        let references = additions.iter().map(String::as_str).collect::<Vec<_>>();
        model.splice(current_len, 0, &references);
    }
    if selected == gtk::INVALID_LIST_POSITION {
        dropdown.set_selected(0);
    } else if selected >= maximum {
        dropdown.set_selected(maximum - 1);
    }
}

fn build_tags_row(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    field: &FieldSpec,
    values: Vec<String>,
) -> adw::PreferencesRow {
    let title = gtk::Label::builder()
        .label(field_title(field))
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let chips = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(6)
        .row_spacing(6)
        .max_children_per_line(6)
        .build();
    let entry = gtk::Entry::builder()
        .placeholder_text(i18n::text(UiMessage::TagsPlaceholder))
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&title);
    content.append(&chips);
    content.append(&entry);
    let row = adw::PreferencesRow::new();
    row.set_child(Some(&content));

    let values = Rc::new(RefCell::new(values));
    rebuild_tag_chips(&chips, &values, widgets, state, &field.name);
    let callback_chips = chips.clone();
    let callback_values = Rc::clone(&values);
    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let name = field.name.clone();
    let callback_entry = entry.clone();
    entry.connect_activate(move |_| {
        add_tags_from_entry(
            &callback_entry,
            &callback_chips,
            &callback_values,
            &callback_widgets,
            &callback_state,
            &name,
        );
    });
    let callback_chips = chips;
    let callback_values = values;
    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    let name = field.name.clone();
    entry.connect_changed(move |entry| {
        if entry.text().ends_with(',') {
            add_tags_from_entry(
                entry,
                &callback_chips,
                &callback_values,
                &callback_widgets,
                &callback_state,
                &name,
            );
        }
    });
    row
}

fn add_tags_from_entry(
    entry: &gtk::Entry,
    chips: &gtk::FlowBox,
    values: &Rc<RefCell<Vec<String>>>,
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    name: &str,
) {
    let additions = parse_tags(entry.text().as_str());
    if additions.is_empty() {
        entry.set_text("");
        return;
    }
    {
        let mut values = values.borrow_mut();
        for tag in additions {
            if !values.iter().any(|existing| existing == &tag) {
                values.push(tag);
            }
        }
    }
    entry.set_text("");
    update_field(
        widgets,
        state,
        name,
        FieldValue::Tags(values.borrow().clone()),
    );
    rebuild_tag_chips(chips, values, widgets, state, name);
}

fn rebuild_tag_chips(
    chips: &gtk::FlowBox,
    values: &Rc<RefCell<Vec<String>>>,
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    name: &str,
) {
    while let Some(child) = chips.first_child() {
        chips.remove(&child);
    }
    for tag in values.borrow().clone() {
        let content = adw::ButtonContent::builder()
            .label(&tag)
            .icon_name("window-close-symbolic")
            .build();
        let button = gtk::Button::builder()
            .child(&content)
            .tooltip_text(i18n::text(UiMessage::RemoveTagTooltip { tag: tag.clone() }))
            .build();
        let callback_chips = chips.clone();
        let callback_values = Rc::clone(values);
        let callback_widgets = widgets.clone();
        let callback_state = Rc::clone(state);
        let callback_name = name.to_owned();
        button.connect_clicked(move |_| {
            callback_values.borrow_mut().retain(|value| value != &tag);
            update_field(
                &callback_widgets,
                &callback_state,
                &callback_name,
                FieldValue::Tags(callback_values.borrow().clone()),
            );
            rebuild_tag_chips(
                &callback_chips,
                &callback_values,
                &callback_widgets,
                &callback_state,
                &callback_name,
            );
        });
        chips.insert(&button, -1);
    }
    chips.set_visible(!values.borrow().is_empty());
}

fn update_field(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    name: &str,
    value: FieldValue,
) {
    let raw_frontmatter = state
        .borrow()
        .session
        .document
        .as_ref()
        .and_then(|document| document.raw_frontmatter.clone());
    let Some(raw_frontmatter) = raw_frontmatter else {
        return;
    };
    match frontmatter_service::set_field(&raw_frontmatter, name, value) {
        Ok(raw) => {
            if let Some(document) = state.borrow_mut().session.document.as_mut() {
                document.raw_frontmatter = Some(raw);
            }
            mark_document_dirty(widgets, state);
        }
        Err(error) => super::show_user_facing_error(widgets, &error),
    }
}

fn append_remove_button(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let button = gtk::Button::builder()
        .label(i18n::text(UiMessage::RemoveFrontmatter))
        .css_classes(["destructive-action"])
        .halign(gtk::Align::Start)
        .build();
    let callback_widgets = widgets.clone();
    let callback_state = Rc::clone(state);
    button.connect_clicked(move |_| {
        let dialog = adw::AlertDialog::builder()
            .heading(i18n::text(UiMessage::RemoveFrontmatterHeading))
            .body(i18n::text(UiMessage::RemoveFrontmatterBody))
            .default_response("cancel")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", i18n::text(UiMessage::Cancel).as_str()),
            ("remove", i18n::text(UiMessage::Remove).as_str()),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let response_widgets = callback_widgets.clone();
        let response_state = Rc::clone(&callback_state);
        dialog.connect_response(Some("remove"), move |_, _| {
            if let Some(document) = response_state.borrow_mut().session.document.as_mut() {
                document.raw_frontmatter = None;
            }
            mark_document_dirty(&response_widgets, &response_state);
            refresh(&response_widgets, &response_state);
        });
        dialog.present(Some(&callback_widgets.window));
    });
    widgets.frontmatter_panel.append(&button);
}

fn parse_date_parts(value: &str, today: (i32, i32, i32)) -> Option<(i32, i32, i32)> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    let max_day = i32::try_from(days_in_month(year, month)?).ok()?;
    if parts.next().is_some()
        || !(2000..=today.0).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=max_day).contains(&day)
        || (year, month, day) > today
    {
        return None;
    }
    Some((year, month, day))
}

fn day_strings(year: i32, month: i32, today: (i32, i32, i32), unit: &str) -> Vec<String> {
    let calendar_max = days_in_month(year, month).unwrap_or(31);
    let maximum = if year == today.0 && month == today.1 {
        u32::try_from(today.2).unwrap_or(1)
    } else {
        calendar_max
    };
    (1..=maximum).map(|day| format!("{day} {unit}")).collect()
}

fn days_in_month(year: i32, month: i32) -> Option<u32> {
    use gtk::glib::DateMonth;

    const MONTHS: [DateMonth; 12] = [
        DateMonth::January,
        DateMonth::February,
        DateMonth::March,
        DateMonth::April,
        DateMonth::May,
        DateMonth::June,
        DateMonth::July,
        DateMonth::August,
        DateMonth::September,
        DateMonth::October,
        DateMonth::November,
        DateMonth::December,
    ];
    let month_index = usize::try_from(month.checked_sub(1)?).ok()?;
    let month = *MONTHS.get(month_index)?;
    let year = u16::try_from(year).ok()?;
    Some(u32::from(gtk::glib::Date::days_in_month(month, year)))
}

fn date_subtitle(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
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
        i18n::text(UiMessage::RequiredFieldTitle {
            name: field.name.clone(),
        })
    } else {
        i18n::text(UiMessage::FieldTitle {
            name: field.name.clone(),
        })
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
        assert_eq!(date_subtitle(""), None);
        assert_eq!(date_subtitle("2026-08-05"), Some("2026-08-05"));
    }

    #[test]
    fn date_parts_validate_month_lengths_and_leap_years() {
        let today = (2026, 8, 6);
        assert_eq!(parse_date_parts("2024-02-29", today), Some((2024, 2, 29)));
        assert_eq!(parse_date_parts("2025-02-29", today), None);
        assert_eq!(parse_date_parts("2026-11-31", today), None);
        assert_eq!(parse_date_parts("1999-12-31", today), None);
        assert_eq!(parse_date_parts("2000-01-01", today), Some((2000, 1, 1)));
        assert_eq!(parse_date_parts("2026-08-07", today), None);
        assert_eq!(parse_date_parts("2027-01-01", today), None);
        assert_eq!(days_in_month(2000, 2), Some(29));
        assert_eq!(days_in_month(1900, 2), Some(28));
        assert_eq!(days_in_month(2026, 0), None);
        assert_eq!(days_in_month(2026, 13), None);
    }
}
