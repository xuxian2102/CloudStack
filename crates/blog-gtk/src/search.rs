use gtk::{gdk, glib};
use sourceview::prelude::*;

#[derive(Clone)]
pub struct SearchPanel {
    root: gtk::Revealer,
    query: gtk::SearchEntry,
    replacement: gtk::Entry,
    count: gtk::Label,
    replace_row: gtk::Box,
    replace_toggle: gtk::ToggleButton,
    settings: sourceview::SearchSettings,
    context: sourceview::SearchContext,
    buffer: sourceview::Buffer,
    view: sourceview::View,
}

impl SearchPanel {
    pub fn new(buffer: &sourceview::Buffer, view: &sourceview::View) -> Self {
        let settings = sourceview::SearchSettings::builder()
            .wrap_around(true)
            .build();
        let context = sourceview::SearchContext::new(buffer, Some(&settings));
        context.set_highlight(true);

        let query = gtk::SearchEntry::builder()
            .placeholder_text("查找")
            .hexpand(true)
            .max_width_chars(60)
            .build();
        let count = gtk::Label::builder()
            .width_chars(9)
            .xalign(0.5)
            .css_classes(["dim-label", "numeric"])
            .build();
        let previous = icon_button("go-up-symbolic", "上一个匹配项 (Shift+F3)");
        let next = icon_button("go-down-symbolic", "下一个匹配项 (F3)");
        let case_sensitive = gtk::ToggleButton::builder()
            .label("Aa")
            .tooltip_text("区分大小写")
            .build();
        let replace_toggle = gtk::ToggleButton::builder()
            .icon_name("edit-find-replace-symbolic")
            .tooltip_text("显示替换栏")
            .build();
        let close = icon_button("window-close-symbolic", "关闭查找 (Escape)");

        let find_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        find_row.set_margin_top(6);
        find_row.set_margin_bottom(6);
        find_row.set_margin_start(8);
        find_row.set_margin_end(8);
        find_row.append(&query);
        find_row.append(&count);
        find_row.append(&previous);
        find_row.append(&next);
        find_row.append(&case_sensitive);
        find_row.append(&replace_toggle);
        find_row.append(&close);

        let replacement = gtk::Entry::builder()
            .placeholder_text("替换为")
            .hexpand(true)
            .build();
        let replace_one = gtk::Button::with_label("替换");
        replace_one.set_tooltip_text(Some("替换当前匹配项"));
        let replace_all = gtk::Button::with_label("全部替换");
        replace_all.set_tooltip_text(Some("替换文档中的所有匹配项"));
        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        replace_row.set_margin_bottom(6);
        replace_row.set_margin_start(8);
        replace_row.set_margin_end(8);
        replace_row.set_visible(false);
        replace_row.append(&replacement);
        replace_row.append(&replace_one);
        replace_row.append(&replace_all);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("toolbar");
        content.append(&find_row);
        content.append(&replace_row);

        let root = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(150)
            .child(&content)
            .build();

        let panel = Self {
            root,
            query,
            replacement,
            count,
            replace_row,
            replace_toggle,
            settings,
            context,
            buffer: buffer.clone(),
            view: view.clone(),
        };
        panel.connect_signals(
            &panel.count,
            &previous,
            &next,
            &case_sensitive,
            &close,
            &replace_one,
            &replace_all,
        );
        panel
    }

    pub fn widget(&self) -> &gtk::Revealer {
        &self.root
    }

    pub fn open(&self, show_replace: bool) {
        if let Some((start, end)) = self.buffer.selection_bounds() {
            let selected = self.buffer.text(&start, &end, true);
            if !selected.is_empty() && selected.len() <= 200 && !selected.contains('\n') {
                self.query.set_text(&selected);
            }
        }
        self.replace_toggle.set_active(show_replace);
        self.root.set_reveal_child(true);
        self.query.grab_focus();
        self.query.select_region(0, -1);
        let query = self.query.text();
        self.settings
            .set_search_text((!query.is_empty()).then_some(query.as_str()));
        self.refresh_match();
    }

    pub fn next(&self) {
        select_match(&self.context, &self.buffer, &self.view, Direction::Forward);
        update_count(&self.context, &self.buffer, &self.count);
    }

    pub fn previous(&self) {
        select_match(&self.context, &self.buffer, &self.view, Direction::Backward);
        update_count(&self.context, &self.buffer, &self.count);
    }

    fn close(&self) {
        self.root.set_reveal_child(false);
        self.settings.set_search_text(None);
        self.count.set_label("");
        self.view.grab_focus();
    }

    fn refresh_match(&self) {
        if self.settings.search_text().is_none() {
            self.count.set_label("");
            return;
        }
        let cursor = self
            .buffer
            .selection_bounds()
            .map(|(start, _)| start)
            .unwrap_or_else(|| self.buffer.iter_at_mark(&self.buffer.get_insert()));
        select_forward_from(&self.context, &self.buffer, &self.view, &cursor);
        update_count(&self.context, &self.buffer, &self.count);
    }

    #[allow(clippy::too_many_arguments)]
    fn connect_signals(
        &self,
        count: &gtk::Label,
        previous: &gtk::Button,
        next: &gtk::Button,
        case_sensitive: &gtk::ToggleButton,
        close: &gtk::Button,
        replace_one: &gtk::Button,
        replace_all: &gtk::Button,
    ) {
        let settings = self.settings.clone();
        let panel = self.clone();
        self.query.connect_search_changed(move |entry| {
            let text = entry.text();
            settings.set_search_text((!text.is_empty()).then_some(text.as_str()));
            panel.refresh_match();
        });

        let panel = self.clone();
        self.query.connect_next_match(move |_| panel.next());
        let panel = self.clone();
        self.query.connect_previous_match(move |_| panel.previous());
        let panel = self.clone();
        self.query.connect_stop_search(move |_| panel.close());

        let panel = self.clone();
        next.connect_clicked(move |_| panel.next());
        let panel = self.clone();
        previous.connect_clicked(move |_| panel.previous());

        let settings = self.settings.clone();
        let panel = self.clone();
        case_sensitive.connect_toggled(move |button| {
            settings.set_case_sensitive(button.is_active());
            panel.refresh_match();
        });

        let replace_row = self.replace_row.clone();
        self.replace_toggle.connect_toggled(move |button| {
            replace_row.set_visible(button.is_active());
        });

        let panel = self.clone();
        close.connect_clicked(move |_| panel.close());

        let panel = self.clone();
        replace_one.connect_clicked(move |_| panel.replace_current());
        let panel = self.clone();
        self.replacement
            .connect_activate(move |_| panel.replace_current());

        let panel = self.clone();
        replace_all.connect_clicked(move |_| panel.replace_all());

        let count_label = count.clone();
        let buffer = self.buffer.clone();
        self.context
            .connect_occurrences_count_notify(move |context| {
                update_count(context, &buffer, &count_label);
            });

        let escape = gtk::EventControllerKey::new();
        escape.set_propagation_phase(gtk::PropagationPhase::Capture);
        let panel = self.clone();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                panel.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.root.add_controller(escape);
    }

    fn replace_current(&self) {
        let replacement = self.replacement.text();
        let Some((mut start, mut end)) = self.buffer.selection_bounds() else {
            self.next();
            return;
        };
        if self.context.occurrence_position(&start, &end) <= 0 {
            self.next();
            return;
        }
        if let Err(error) = self.context.replace(&mut start, &mut end, &replacement) {
            log::warn!("替换文本失败：{error}");
            return;
        }
        self.next();
    }

    fn replace_all(&self) {
        let replacement = self.replacement.text();
        if let Err(error) = self.context.replace_all(&replacement) {
            log::warn!("全部替换失败：{error}");
        }
        update_count(&self.context, &self.buffer, &self.count);
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Backward,
}

fn select_match(
    context: &sourceview::SearchContext,
    buffer: &sourceview::Buffer,
    view: &sourceview::View,
    direction: Direction,
) {
    if context.settings().search_text().is_none() {
        return;
    }
    let cursor = match (direction, buffer.selection_bounds()) {
        (Direction::Forward, Some((_, end))) => end,
        (Direction::Backward, Some((start, _))) => start,
        (_, None) => buffer.iter_at_mark(&buffer.get_insert()),
    };
    let found = match direction {
        Direction::Forward => context.forward(&cursor),
        Direction::Backward => context.backward(&cursor),
    };
    if let Some((mut start, end, _)) = found {
        buffer.select_range(&start, &end);
        view.scroll_to_iter(&mut start, 0.15, false, 0.0, 0.0);
    }
}

fn select_forward_from(
    context: &sourceview::SearchContext,
    buffer: &sourceview::Buffer,
    view: &sourceview::View,
    cursor: &gtk::TextIter,
) {
    if let Some((mut start, end, _)) = context.forward(cursor) {
        buffer.select_range(&start, &end);
        view.scroll_to_iter(&mut start, 0.15, false, 0.0, 0.0);
    }
}

fn update_count(
    context: &sourceview::SearchContext,
    buffer: &sourceview::Buffer,
    label: &gtk::Label,
) {
    let total = context.occurrences_count();
    if context.settings().search_text().is_none() {
        label.set_label("");
    } else if total < 0 {
        label.set_label("…");
    } else if total == 0 {
        label.set_label("无匹配");
    } else {
        let position = buffer
            .selection_bounds()
            .map(|(start, end)| context.occurrence_position(&start, &end))
            .unwrap_or(0)
            .max(0);
        label.set_label(&format!("{position}/{total}"));
    }
}

fn icon_button(icon_name: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon_name)
        .tooltip_text(tooltip)
        .build()
}
