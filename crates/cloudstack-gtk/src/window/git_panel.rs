use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::model::{
    ChangeKind, FileChange, GitOperationResult, OperationReport, RepositorySnapshot,
    RepositoryTopology, RepositoryVisibility, SyncRelation, WorktreeState,
};
use cloudstack_core::services::git;
use gtk::glib;

use super::{
    has_unsaved_documents, open_project, set_busy, show_error, toast, unsaved_document_count,
    EditorState, Widgets,
};
use crate::tasks;

const MAX_DISPLAYED_CHANGES: usize = 100;

#[derive(Clone)]
pub(super) struct GitPanel {
    root: gtk::Box,
    summary_label: gtk::Label,
    toggle_button: gtk::Button,
    details: gtk::Box,
    repository_label: gtk::Label,
    sync_label: gtk::Label,
    worktree_label: gtk::Label,
    remote_label: gtk::Label,
    changes_title: gtk::Label,
    changes_list: gtk::Box,
    refresh_button: gtk::Button,
    fetch_button: gtk::Button,
    untrack_config_button: gtk::Button,
    publish_button: gtk::Button,
    split: Rc<RefCell<Option<gtk::Paned>>>,
    expanded: Rc<Cell<bool>>,
    expanded_height: Rc<Cell<i32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrimaryAction {
    None,
    Initialize,
    ConfigureIdentity,
    Commit,
    ConfigureRemote,
    PushUpstream,
    Push,
    PullFastForward,
}

impl GitPanel {
    pub(super) fn new() -> Self {
        let toggle_button = gtk::Button::builder()
            .icon_name("pan-up-symbolic")
            .tooltip_text("展开 Git 详情")
            .css_classes(["flat"])
            .build();
        let summary_label = gtk::Label::builder()
            .label("尚未打开项目")
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        let publish_button = gtk::Button::builder()
            .label("提交")
            .tooltip_text("执行建议的 Git 操作")
            .action_name("win.git-primary")
            .sensitive(false)
            .css_classes(["suggested-action"])
            .build();
        let summary = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        summary.set_margin_top(6);
        summary.set_margin_bottom(6);
        summary.set_margin_start(6);
        summary.set_margin_end(8);
        summary.append(&toggle_button);
        summary.append(&summary_label);
        summary.append(&publish_button);

        let title = gtk::Label::builder()
            .label("Git 详情")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("刷新 Git 状态")
            .action_name("win.refresh-git")
            .sensitive(false)
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.append(&title);

        let repository_label = status_label("尚未打开项目");
        let sync_label = status_label("同步状态未知");
        let worktree_label = status_label("工作区状态未知");
        let remote_heading = section_heading("远端");
        let remote_label = status_label("尚未配置远端");
        remote_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        remote_label.set_wrap(false);
        let changes_title = section_heading("改动");
        let changes_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        set_changes_placeholder(&changes_list, "没有改动");
        let fetch_button = gtk::Button::builder()
            .label("获取")
            .tooltip_text("执行 git fetch --prune")
            .action_name("win.fetch-git")
            .sensitive(false)
            .build();
        let untrack_config_button = gtk::Button::builder()
            .label("停止跟踪配置")
            .tooltip_text("从 Git 索引移除配置，但保留本地文件")
            .action_name("win.untrack-config")
            .visible(false)
            .build();
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.append(&untrack_config_button);
        actions.append(&fetch_button);
        actions.append(&refresh_button);

        let changes_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(72)
            .vexpand(true)
            .child(&changes_list)
            .build();

        let details = gtk::Box::new(gtk::Orientation::Vertical, 6);
        details.set_margin_top(4);
        details.set_margin_bottom(10);
        details.set_margin_start(12);
        details.set_margin_end(12);
        details.set_vexpand(true);
        details.append(&header);
        details.append(&repository_label);
        details.append(&sync_label);
        details.append(&worktree_label);
        details.append(&remote_heading);
        details.append(&remote_label);
        details.append(&changes_title);
        details.append(&changes_scroll);
        details.append(&actions);
        details.set_visible(false);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&summary);
        root.append(&details);

        let panel = Self {
            root,
            summary_label,
            toggle_button,
            details,
            repository_label,
            sync_label,
            worktree_label,
            remote_label,
            changes_title,
            changes_list,
            refresh_button,
            fetch_button,
            untrack_config_button,
            publish_button,
            split: Rc::new(RefCell::new(None)),
            expanded: Rc::new(Cell::new(false)),
            expanded_height: Rc::new(Cell::new(340)),
        };
        let panel_for_toggle = panel.clone();
        panel.toggle_button.connect_clicked(move |_| {
            panel_for_toggle.set_expanded(!panel_for_toggle.expanded.get());
        });
        panel
    }

    pub(super) fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub(super) fn bind_split(&self, split: &gtk::Paned) {
        self.split.replace(Some(split.clone()));
        let panel = self.clone();
        split.connect_realize(move |_| {
            let panel = panel.clone();
            glib::idle_add_local_once(move || {
                panel.apply_split_position(panel.expanded.get());
            });
        });

        let expanded = Rc::clone(&self.expanded);
        let expanded_height = Rc::clone(&self.expanded_height);
        split.connect_position_notify(move |split| {
            if expanded.get() {
                let height = split.max_position().saturating_sub(split.position());
                if height >= 180 {
                    expanded_height.set(height);
                }
            }
        });

        // Hiding the details changes max-position during a later layout pass. Keep
        // a collapsed panel pinned to the new maximum so no empty lower pane remains.
        let expanded = Rc::clone(&self.expanded);
        split.connect_max_position_notify(move |split| {
            if !expanded.get() && split.position() != split.max_position() {
                split.set_position(split.max_position());
            }
        });
    }

    pub(super) fn set_expanded(&self, expanded: bool) {
        self.expanded.set(expanded);
        self.details.set_visible(expanded);
        self.toggle_button.set_icon_name(if expanded {
            "pan-down-symbolic"
        } else {
            "pan-up-symbolic"
        });
        self.toggle_button.set_tooltip_text(Some(if expanded {
            "收起 Git 详情"
        } else {
            "展开 Git 详情"
        }));
        self.apply_split_position(expanded);
    }

    fn apply_split_position(&self, expanded: bool) {
        let Some(split) = self.split.borrow().clone() else {
            return;
        };
        let desired_height = self.expanded_height.get();
        glib::idle_add_local_once(move || {
            let minimum = split.min_position();
            let maximum = split.max_position();
            let position = git_split_position(minimum, maximum, expanded, desired_height);
            split.set_position(position);
        });
    }

    pub(super) fn set_loading(&self) {
        self.summary_label.set_label("正在读取 Git 状态…");
        self.repository_label.set_label("正在读取仓库状态…");
        self.sync_label.set_label("同步状态：读取中");
        self.worktree_label.set_label("工作区：读取中");
        self.remote_label.set_label("读取中…");
        self.changes_title.set_label("改动");
        set_changes_placeholder(&self.changes_list, "读取中…");
        self.refresh_button.set_sensitive(false);
        self.fetch_button.set_sensitive(false);
        self.untrack_config_button.set_sensitive(false);
        self.publish_button.set_label("读取中");
        self.publish_button.set_sensitive(false);
    }

    pub(super) fn apply(&self, snapshot: &RepositorySnapshot) {
        if !snapshot.environment.git_available {
            self.summary_label.set_label("未安装 Git");
            self.repository_label.set_label("未安装 Git");
            self.sync_label.set_label("请先通过 pacman 安装 git");
            self.worktree_label.set_label("工作区：未知");
            self.remote_label.set_label("不可用");
            set_changes_placeholder(&self.changes_list, "不可用");
            self.refresh_button.set_sensitive(false);
            self.fetch_button.set_sensitive(false);
            self.untrack_config_button.set_visible(false);
            self.publish_button.set_label("提交");
            self.publish_button.set_sensitive(false);
            return;
        }
        let branch = snapshot.status.branch.as_deref().unwrap_or("—");
        let summary = summary_text(snapshot);
        self.summary_label.set_label(&summary);
        self.summary_label.set_tooltip_text(Some(&summary));
        self.repository_label
            .set_label(&format!("{} · {branch}", topology_text(snapshot.topology)));
        self.sync_label.set_label(&sync_text(snapshot));
        self.worktree_label
            .set_label(&worktree_text(snapshot.worktree));
        self.remote_label.set_label(&remote_text(snapshot));
        self.remote_label
            .set_tooltip_text(Some(&remote_text(snapshot)));
        self.changes_title
            .set_label(&format!("改动 · {}", snapshot.status.changes.len()));
        populate_changes(&self.changes_list, snapshot);
        self.refresh_button
            .set_sensitive(snapshot.environment.git_available);
        self.fetch_button
            .set_sensitive(!snapshot.remotes.is_empty());
        self.untrack_config_button
            .set_visible(snapshot.config_tracked);
        self.untrack_config_button
            .set_sensitive(snapshot.config_tracked);
        let action = recommended_action(snapshot);
        self.publish_button.set_label(compact_action_label(action));
        self.publish_button
            .set_tooltip_text(Some(action_label(action)));
        self.publish_button
            .set_sensitive(action != PrimaryAction::None);
    }

    pub(super) fn set_error(&self, message: &str) {
        self.summary_label.set_label("Git 状态读取失败");
        self.repository_label.set_label("Git 状态读取失败");
        self.sync_label.set_label(message);
        self.worktree_label.set_label("工作区：未知");
        self.remote_label.set_label("未知");
        set_changes_placeholder(&self.changes_list, "未知");
        self.refresh_button.set_sensitive(true);
        self.fetch_button.set_sensitive(false);
        self.untrack_config_button.set_visible(false);
        self.publish_button.set_sensitive(false);
    }

    pub(super) fn set_project_available(&self, available: bool) {
        if !available {
            self.refresh_button.set_sensitive(false);
            self.repository_label.set_label("尚未打开项目");
            self.summary_label.set_label("尚未打开项目");
            self.sync_label.set_label("同步状态未知");
            self.worktree_label.set_label("工作区状态未知");
            self.remote_label.set_label("尚未配置远端");
            self.changes_title.set_label("改动");
            set_changes_placeholder(&self.changes_list, "没有改动");
            self.fetch_button.set_sensitive(false);
            self.untrack_config_button.set_visible(false);
            self.publish_button.set_sensitive(false);
        }
    }

    pub(super) fn reflect_unsaved_editor(&self, dirty: bool, available: bool) {
        if dirty {
            let current = self.summary_label.text();
            if !current.starts_with("未保存 · ") {
                self.summary_label.set_label(&format!("未保存 · {current}"));
            }
            self.publish_button.set_label("保存");
            self.publish_button
                .set_tooltip_text(Some("先保存正文，再打开提交窗口"));
            self.publish_button.set_sensitive(available);
        }
    }
}

pub(super) fn untrack_config(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    confirm_operation(
        widgets,
        state,
        "停止跟踪 CloudStack 配置？",
        "本地配置会从 Git 索引移除，并单独创建一个本地提交；文件本身会保留并加入此仓库的本地 exclude，不会自动推送。",
        "停止跟踪",
        Completion::Refresh,
        |context| git::stop_tracking_project_config(&context),
    );
}

fn git_split_position(minimum: i32, maximum: i32, expanded: bool, desired_height: i32) -> i32 {
    if maximum <= minimum || !expanded {
        return maximum.max(minimum);
    }
    maximum
        .saturating_sub(desired_height.max(180))
        .clamp(minimum, maximum)
}

pub(super) fn recommended_action(snapshot: &RepositorySnapshot) -> PrimaryAction {
    if !snapshot.environment.git_available
        || snapshot.worktree.has_conflicts
        || snapshot.topology == RepositoryTopology::ParentRepository
        || snapshot.topology == RepositoryTopology::Detached
        || snapshot.sync == SyncRelation::Diverged
    {
        return PrimaryAction::None;
    }
    if snapshot.identity.is_none() && snapshot.worktree.managed_changes > 0 {
        return PrimaryAction::ConfigureIdentity;
    }
    match snapshot.topology {
        RepositoryTopology::NotInitialized => PrimaryAction::Initialize,
        RepositoryTopology::NoCommit => PrimaryAction::Commit,
        RepositoryTopology::NoRemote => {
            if snapshot.worktree.managed_changes > 0 {
                PrimaryAction::Commit
            } else {
                PrimaryAction::ConfigureRemote
            }
        }
        RepositoryTopology::NoUpstream => {
            if snapshot.worktree.managed_changes > 0 {
                PrimaryAction::Commit
            } else {
                PrimaryAction::PushUpstream
            }
        }
        RepositoryTopology::Tracking => {
            if snapshot.status.behind > 0 {
                if snapshot.status.ahead == 0 && snapshot.worktree.is_clean() {
                    PrimaryAction::PullFastForward
                } else {
                    PrimaryAction::None
                }
            } else if snapshot.worktree.managed_changes > 0 {
                PrimaryAction::Commit
            } else if snapshot.status.ahead > 0 {
                PrimaryAction::Push
            } else {
                PrimaryAction::None
            }
        }
        RepositoryTopology::ParentRepository | RepositoryTopology::Detached => PrimaryAction::None,
    }
}

fn action_label(action: PrimaryAction) -> &'static str {
    match action {
        PrimaryAction::None => "无需操作",
        PrimaryAction::Initialize => "初始化 Git",
        PrimaryAction::ConfigureIdentity => "配置提交身份",
        PrimaryAction::Commit => "提交受管改动",
        PrimaryAction::ConfigureRemote => "配置远端",
        PrimaryAction::PushUpstream => "首次推送",
        PrimaryAction::Push => "推送提交",
        PrimaryAction::PullFastForward => "快进同步",
    }
}

fn compact_action_label(action: PrimaryAction) -> &'static str {
    match action {
        PrimaryAction::None => "已同步",
        PrimaryAction::Initialize => "初始化",
        PrimaryAction::ConfigureIdentity => "身份",
        PrimaryAction::Commit => "提交",
        PrimaryAction::ConfigureRemote => "远端",
        PrimaryAction::PushUpstream | PrimaryAction::Push => "推送",
        PrimaryAction::PullFastForward => "同步",
    }
}

fn status_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .css_classes(["dim-label"])
        .build()
}

fn section_heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .margin_top(6)
        .css_classes(["caption-heading"])
        .build()
}

fn summary_text(snapshot: &RepositorySnapshot) -> String {
    let branch = snapshot.status.branch.as_deref().unwrap_or("—");
    let destination = snapshot.status.upstream.as_deref().map_or_else(
        || branch.to_owned(),
        |upstream| format!("{branch} → {upstream}"),
    );
    let count = snapshot.status.changes.len();
    match snapshot.topology {
        RepositoryTopology::NotInitialized => format!("未初始化 · {count} 项改动"),
        RepositoryTopology::ParentRepository => "仅父目录有仓库".to_string(),
        RepositoryTopology::Detached => format!("分离 HEAD · {count} 项改动"),
        _ if count == 0 => format!("{destination} · 工作区干净"),
        _ => format!("{destination} · {count} 项改动"),
    }
}

fn remote_text(snapshot: &RepositorySnapshot) -> String {
    if snapshot.remotes.is_empty() {
        return "尚未配置远端".to_string();
    }
    snapshot
        .remotes
        .iter()
        .map(|remote| match &remote.url {
            Some(url) => format!("{}  {url}", remote.name),
            None => remote.name.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
fn changes_text(snapshot: &RepositorySnapshot) -> String {
    if snapshot.status.changes.is_empty() {
        return "没有改动".to_string();
    }
    snapshot
        .status
        .changes
        .iter()
        .map(change_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn populate_changes(list: &gtk::Box, snapshot: &RepositorySnapshot) {
    clear_box(list);
    if snapshot.status.changes.is_empty() {
        list.append(&status_label("没有改动"));
        return;
    }
    let displayed = prioritized_changes(&snapshot.status.changes);
    for change in &displayed {
        let state = gtk::Label::builder()
            .label(change_marker(change.kind))
            .width_chars(1)
            .xalign(0.5)
            .css_classes(["caption-heading", "accent"])
            .build();
        let path = gtk::Label::builder()
            .label(change_path(change))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .single_line_mode(true)
            .tooltip_text(change_text(change))
            .css_classes(if change.managed {
                ["monospace", "caption"]
            } else {
                ["monospace", "dim-label"]
            })
            .build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&state);
        row.append(&path);
        list.append(&row);
    }
    let omitted = snapshot
        .status
        .changes
        .len()
        .saturating_sub(displayed.len());
    if omitted > 0 {
        list.append(&status_label(&format!(
            "还有 {omitted} 项未展开；请完善 .gitignore 后刷新"
        )));
    }
}

fn prioritized_changes(changes: &[FileChange]) -> Vec<&FileChange> {
    changes
        .iter()
        .filter(|change| change.managed)
        .chain(changes.iter().filter(|change| !change.managed))
        .take(MAX_DISPLAYED_CHANGES)
        .collect()
}

fn set_changes_placeholder(list: &gtk::Box, text: &str) {
    clear_box(list);
    list.append(&status_label(text));
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn change_marker(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Modified => "M",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Untracked => "?",
        ChangeKind::Unmerged => "!",
    }
}

fn change_path(change: &FileChange) -> String {
    let path = change.old_path.as_deref().map_or_else(
        || change.path.clone(),
        |old_path| format!("{old_path} → {}", change.path),
    );
    let scope = if change.managed { "" } else { " · 非受管" };
    let staged = if change.staged { " · 已暂存" } else { "" };
    format!("{path}{scope}{staged}")
}

fn change_text(change: &FileChange) -> String {
    format!("{}  {}", change_marker(change.kind), change_path(change))
}

fn topology_text(topology: RepositoryTopology) -> &'static str {
    match topology {
        RepositoryTopology::NotInitialized => "未初始化",
        RepositoryTopology::ParentRepository => "仅父目录有仓库",
        RepositoryTopology::NoCommit => "尚无提交",
        RepositoryTopology::NoRemote => "没有远端",
        RepositoryTopology::NoUpstream => "没有 upstream",
        RepositoryTopology::Tracking => "已跟踪",
        RepositoryTopology::Detached => "分离 HEAD",
    }
}

fn sync_text(snapshot: &RepositorySnapshot) -> String {
    match snapshot.sync {
        SyncRelation::Unknown => "同步：尚不可比较".to_string(),
        SyncRelation::Synced => "同步：与远端一致".to_string(),
        SyncRelation::Ahead => format!("同步：领先 {} 个提交", snapshot.status.ahead),
        SyncRelation::Behind => format!("同步：落后 {} 个提交", snapshot.status.behind),
        SyncRelation::Diverged => format!(
            "同步：已分叉（领先 {} / 落后 {}）",
            snapshot.status.ahead, snapshot.status.behind
        ),
    }
}

fn worktree_text(worktree: WorktreeState) -> String {
    if worktree.has_conflicts {
        return "工作区：存在未解决冲突".to_string();
    }
    if worktree.is_clean() {
        return "工作区：干净".to_string();
    }
    let staged = if worktree.staged_changes > 0 {
        format!("，已暂存 {}", worktree.staged_changes)
    } else {
        String::new()
    };
    format!(
        "工作区：受管 {} / 其他 {}{staged}",
        worktree.managed_changes, worktree.unmanaged_changes
    )
}

pub(super) fn activate_primary(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    if state.borrow().busy {
        return;
    }
    let unsaved_count = unsaved_document_count(state);
    if unsaved_count > 1 {
        toast(widgets, "请先保存其他未保存文章，再执行 Git 操作");
        return;
    }
    if has_unsaved_documents(state) {
        if gtk::prelude::WidgetExt::activate_action(&widgets.window, "win.publish", None).is_err() {
            show_error(widgets, "无法保存并打开提交窗口");
        }
        return;
    }
    let Some(snapshot) = state.borrow().git_snapshot.clone() else {
        refresh(widgets, state);
        return;
    };
    match recommended_action(&snapshot) {
        PrimaryAction::None => {}
        PrimaryAction::Initialize => confirm_operation(
            widgets,
            state,
            "初始化 Git 仓库？",
            "将在项目根目录执行 git init -b main，不会暂存或提交任何文件。",
            "初始化",
            Completion::Refresh,
            |context| git::initialize(&context),
        ),
        PrimaryAction::ConfigureIdentity => show_identity_dialog(widgets, state),
        PrimaryAction::Commit => {
            if gtk::prelude::WidgetExt::activate_action(&widgets.window, "win.publish", None)
                .is_err()
            {
                show_error(widgets, "无法打开提交窗口");
            }
        }
        PrimaryAction::ConfigureRemote => show_remote_dialog(widgets, state, &snapshot),
        PrimaryAction::PushUpstream => confirm_operation(
            widgets,
            state,
            "首次推送到 origin？",
            "将执行 git push --set-upstream origin <当前分支>。",
            "推送",
            Completion::Refresh,
            |context| git::push_upstream(&context),
        ),
        PrimaryAction::Push => confirm_operation(
            widgets,
            state,
            "推送本地提交？",
            "只会推送当前分支已有的本地提交，不会自动合并或改写历史。",
            "推送",
            Completion::Refresh,
            |context| git::push(&context),
        ),
        PrimaryAction::PullFastForward => confirm_operation(
            widgets,
            state,
            "快进同步远端更新？",
            "仅执行 git pull --ff-only。若不能纯快进，将保持原状并停止。",
            "同步",
            Completion::ReloadProject,
            |context| git::pull_fast_forward(&context),
        ),
    }
}

fn show_identity_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let name_entry = gtk::Entry::builder().placeholder_text("Git 用户名").build();
    let email_entry = gtk::Entry::builder()
        .placeholder_text("name@example.com")
        .build();
    let save_button = gtk::Button::builder()
        .label("保存到当前仓库")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let note = gtk::Label::builder()
        .label("只写入当前项目的 .git/config，不修改全局 Git 身份。")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&note);
    content.append(&name_entry);
    content.append(&email_entry);
    content.append(&save_button);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("配置 Git 提交身份"))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title("配置 Git 提交身份")
        .content_width(520)
        .content_height(300)
        .child(&toolbar)
        .build();

    let sync_button = {
        let name_entry = name_entry.clone();
        let email_entry = email_entry.clone();
        let save_button = save_button.clone();
        move || {
            save_button.set_sensitive(
                !name_entry.text().trim().is_empty() && email_entry.text().trim().contains('@'),
            );
        }
    };
    let sync_button = Rc::new(sync_button);
    let callback = Rc::clone(&sync_button);
    name_entry.connect_changed(move |_| callback());
    let callback = Rc::clone(&sync_button);
    email_entry.connect_changed(move |_| callback());

    let save_widgets = widgets.clone();
    let save_state = Rc::clone(state);
    let save_dialog = dialog.clone();
    let save_name = name_entry.clone();
    let save_email = email_entry.clone();
    save_button.connect_clicked(move |_| {
        let name = save_name.text().trim().to_owned();
        let email = save_email.text().trim().to_owned();
        save_dialog.close();
        execute_operation(
            &save_widgets,
            &save_state,
            "配置 Git 提交身份",
            Completion::Refresh,
            move |context| git::configure_identity(&context, &name, &email),
        );
    });
    dialog.present(Some(&widgets.window));
    name_entry.grab_focus();
}

pub(super) fn fetch_remote(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    if state.borrow().busy {
        return;
    }
    execute_operation(
        widgets,
        state,
        "获取远端状态",
        Completion::Refresh,
        |context| git::fetch(&context),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Completion {
    Refresh,
    ReloadProject,
}

fn confirm_operation<Work>(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    heading: &str,
    body: &str,
    action_label: &str,
    completion: Completion,
    work: Work,
) where
    Work: FnOnce(
            cloudstack_core::ProjectContext,
        ) -> Result<GitOperationResult, cloudstack_core::AppError>
        + Send
        + 'static,
{
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .default_response("confirm")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "取消"), ("confirm", action_label)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
    let parent = widgets.window.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    let heading = heading.to_owned();
    let work = Rc::new(RefCell::new(Some(work)));
    dialog.connect_response(Some("confirm"), move |_, _| {
        let Some(work) = work.borrow_mut().take() else {
            return;
        };
        execute_operation(&widgets, &state, &heading, completion, work);
    });
    dialog.present(Some(&parent));
}

fn execute_operation<Work>(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    heading: &str,
    completion: Completion,
    work: Work,
) where
    Work: FnOnce(
            cloudstack_core::ProjectContext,
        ) -> Result<GitOperationResult, cloudstack_core::AppError>
        + Send
        + 'static,
{
    let has_unsaved = has_unsaved_documents(state);
    let context = {
        let state = state.borrow();
        if state.busy || (completion == Completion::ReloadProject && has_unsaved) {
            return;
        }
        let Some(context) = &state.project else {
            return;
        };
        context.clone()
    };
    let operation_dialog = OperationDialog::new(heading);
    operation_dialog.dialog.present(Some(&widgets.window));
    set_busy(widgets, state, true, heading);

    let project_root = context.root.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    let callback_dialog = Rc::clone(&operation_dialog);
    tasks::run(
        move || work(context),
        move |result| {
            let mut succeeded = false;
            match result {
                Ok(result) => {
                    callback_dialog
                        .trace_buffer
                        .set_text(&operation_log(&result.report));
                    if let Some(error) = result.error {
                        callback_dialog
                            .result_label
                            .set_label(&format!("操作停止：{error}"));
                        show_error(&widgets, "Git 操作未完成，请查看执行记录");
                    } else {
                        callback_dialog.result_label.set_label("操作已完成");
                        toast(&widgets, "Git 操作已完成");
                        succeeded = true;
                    }
                }
                Err(error) => {
                    callback_dialog
                        .result_label
                        .set_label(&format!("命令执行前已停止：{error}"));
                    callback_dialog.trace_buffer.set_text("没有执行外部命令。");
                    show_error(&widgets, &error.to_string());
                }
            }
            callback_dialog.finish();
            set_busy(&widgets, &state, false, "");
            if succeeded && completion == Completion::ReloadProject {
                open_project(&widgets, &state, &project_root);
            } else {
                refresh(&widgets, &state);
            }
        },
    );
}

struct OperationDialog {
    dialog: adw::Dialog,
    spinner: gtk::Spinner,
    result_label: gtk::Label,
    trace_buffer: gtk::TextBuffer,
}

impl OperationDialog {
    fn new(heading: &str) -> Rc<Self> {
        let spinner = gtk::Spinner::new();
        spinner.start();
        let result_label = gtk::Label::builder()
            .label("正在执行…")
            .xalign(0.0)
            .wrap(true)
            .build();
        let trace_buffer = gtk::TextBuffer::new(None);
        let trace_view = gtk::TextView::builder()
            .buffer(&trace_buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .build();
        let trace_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(260)
            .vexpand(true)
            .child(&trace_view)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.append(&spinner);
        content.append(&result_label);
        content.append(&trace_scroll);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some(heading))));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title(heading)
            .content_width(680)
            .content_height(430)
            .can_close(false)
            .child(&toolbar)
            .build();
        Rc::new(Self {
            dialog,
            spinner,
            result_label,
            trace_buffer,
        })
    }

    fn finish(&self) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.dialog.set_can_close(true);
    }
}

fn show_remote_dialog(
    widgets: &Widgets,
    state: &Rc<RefCell<EditorState>>,
    snapshot: &RepositorySnapshot,
) {
    let Some(context) = state.borrow().project.clone() else {
        return;
    };
    let url_entry = gtk::Entry::builder()
        .placeholder_text("git@github.com:user/repository.git")
        .build();
    let setup_credentials =
        gtk::CheckButton::with_label("为 github.com 配置 gh 凭据助手（修改全局 Git 配置）");
    setup_credentials.set_sensitive(snapshot.environment.gh_authenticated);
    let add_button = gtk::Button::builder()
        .label("添加 origin")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let url_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    url_group.append(
        &gtk::Label::builder()
            .label("使用现有远端")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    url_group.append(&url_entry);
    url_group.append(&setup_credentials);
    url_group.append(&add_button);

    let default_name = context
        .root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cloudstack-notes");
    let repo_entry = gtk::Entry::builder()
        .text(default_name)
        .placeholder_text("repository 或 owner/repository")
        .build();
    let private_check = gtk::CheckButton::with_label("创建为私有仓库");
    private_check.set_active(true);
    let create_button = gtk::Button::builder()
        .label("在 GitHub 创建并推送")
        .sensitive(snapshot.environment.gh_authenticated)
        .build();
    let gh_status = if !snapshot.environment.gh_available {
        "未检测到 gh，请先安装 github-cli。"
    } else if !snapshot.environment.gh_authenticated {
        "gh 尚未登录，请先在终端执行 gh auth login。"
    } else {
        "将调用 gh repo create；失败时保留已经完成的远端状态，不自动删除仓库。"
    };
    let github_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    github_group.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    github_group.append(
        &gtk::Label::builder()
            .label("创建 GitHub 仓库")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    github_group.append(
        &gtk::Label::builder()
            .label(gh_status)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    github_group.append(&repo_entry);
    github_group.append(&private_check);
    github_group.append(&create_button);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&url_group);
    content.append(&github_group);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("配置 Git 远端"))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title("配置 Git 远端")
        .content_width(620)
        .content_height(520)
        .child(&toolbar)
        .build();

    let button = add_button.clone();
    url_entry.connect_changed(move |entry| {
        button.set_sensitive(!entry.text().trim().is_empty());
    });

    let add_widgets = widgets.clone();
    let add_state = Rc::clone(state);
    let add_dialog = dialog.clone();
    let add_url = url_entry.clone();
    let add_setup = setup_credentials.clone();
    add_button.connect_clicked(move |_| {
        let url = add_url.text().trim().to_owned();
        let setup = add_setup.is_active();
        add_dialog.close();
        execute_operation(
            &add_widgets,
            &add_state,
            "配置 origin",
            Completion::Refresh,
            move |context| {
                let mut result = git::add_origin(&context, &url)?;
                if result.succeeded() && setup {
                    match git::setup_github_credentials(&context) {
                        Ok(second) => {
                            result.report.traces.extend(second.report.traces);
                            result.error = second.error;
                        }
                        Err(error) => result.error = Some(error.to_string()),
                    }
                }
                Ok(result)
            },
        );
    });

    let create_widgets = widgets.clone();
    let create_state = Rc::clone(state);
    let create_dialog = dialog.clone();
    create_button.connect_clicked(move |_| {
        let name = repo_entry.text().trim().to_owned();
        let visibility = if private_check.is_active() {
            RepositoryVisibility::Private
        } else {
            RepositoryVisibility::Public
        };
        create_dialog.close();
        execute_operation(
            &create_widgets,
            &create_state,
            "创建 GitHub 仓库",
            Completion::Refresh,
            move |context| git::create_github_repository(&context, &name, visibility),
        );
    });
    dialog.present(Some(&widgets.window));
    url_entry.grab_focus();
}

pub(super) fn operation_log(report: &OperationReport) -> String {
    if report.traces.is_empty() {
        return "尚未执行外部命令。".to_string();
    }
    report
        .traces
        .iter()
        .map(|trace| {
            let mut lines = vec![format!(
                "$ {}\n[退出: {} · {} ms]",
                trace.command,
                trace
                    .exit_code
                    .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                trace.duration_ms
            )];
            if !trace.stdout.trim().is_empty() {
                lines.push(trace.stdout.trim_end().to_string());
            }
            if !trace.stderr.trim().is_empty() {
                lines.push(trace.stderr.trim_end().to_string());
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn refresh(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let Some(context) = state.borrow().project.clone() else {
        state.borrow_mut().git_snapshot = None;
        widgets.git_panel.set_project_available(false);
        return;
    };
    widgets.git_panel.set_loading();
    let expected_root = context.root.clone();
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || git::snapshot(&context),
        move |result| {
            let still_current = state
                .borrow()
                .project
                .as_ref()
                .is_some_and(|current| current.root == expected_root);
            if !still_current {
                return;
            }
            match result {
                Ok(snapshot) => {
                    widgets.git_panel.apply(&snapshot);
                    state.borrow_mut().git_snapshot = Some(snapshot);
                }
                Err(error) => {
                    widgets.git_panel.set_error(&error.to_string());
                    show_error(&widgets, &format!("刷新 Git 状态失败：{error}"));
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::model::{GitEnvironment, GitIdentity, GitRemote, GitStatus};

    fn snapshot(
        topology: RepositoryTopology,
        sync: SyncRelation,
        managed: usize,
        unmanaged: usize,
        ahead: u32,
        behind: u32,
    ) -> RepositorySnapshot {
        RepositorySnapshot {
            environment: GitEnvironment {
                git_available: true,
                gh_available: true,
                gh_authenticated: true,
            },
            identity: Some(GitIdentity {
                name: "Test".into(),
                email: "test@example.invalid".into(),
            }),
            topology,
            sync,
            worktree: WorktreeState {
                managed_changes: managed,
                unmanaged_changes: unmanaged,
                staged_changes: 0,
                has_conflicts: false,
            },
            remotes: vec![GitRemote {
                name: "origin".into(),
                url: Some("https://github.com/example/cloudstack.git".into()),
            }],
            config_tracked: false,
            status: GitStatus {
                branch: Some("main".into()),
                upstream: Some("origin/main".into()),
                ahead,
                behind,
                changes: Vec::new(),
            },
        }
    }

    #[test]
    fn dirty_worktree_text_keeps_both_scopes_visible() {
        let text = worktree_text(WorktreeState {
            managed_changes: 2,
            unmanaged_changes: 1,
            staged_changes: 1,
            has_conflicts: false,
        });
        assert!(text.contains("受管 2"));
        assert!(text.contains("其他 1"));
        assert!(text.contains("已暂存 1"));
    }

    #[test]
    fn collapsed_summary_combines_tracking_and_change_count() {
        let mut current = snapshot(
            RepositoryTopology::Tracking,
            SyncRelation::Synced,
            2,
            1,
            0,
            0,
        );
        current.status.changes = vec![
            FileChange {
                path: "content/one.md".into(),
                old_path: None,
                kind: ChangeKind::Modified,
                staged: false,
                managed: true,
            },
            FileChange {
                path: "content/two.md".into(),
                old_path: None,
                kind: ChangeKind::Added,
                staged: true,
                managed: true,
            },
            FileChange {
                path: ".env".into(),
                old_path: None,
                kind: ChangeKind::Untracked,
                staged: false,
                managed: false,
            },
        ];

        assert_eq!(summary_text(&current), "main → origin/main · 3 项改动");
        let details = changes_text(&current);
        assert!(details.contains("M  content/one.md"));
        assert!(details.contains("A  content/two.md · 已暂存"));
        assert!(details.contains("?  .env · 非受管"));
    }

    #[test]
    fn displayed_changes_are_bounded_and_keep_managed_articles_first() {
        let mut changes = (0..150)
            .map(|index| FileChange {
                path: format!("node_modules/file-{index}"),
                old_path: None,
                kind: ChangeKind::Untracked,
                staged: false,
                managed: false,
            })
            .collect::<Vec<_>>();
        changes.push(FileChange {
            path: "src/content/blog/article.md".into(),
            old_path: None,
            kind: ChangeKind::Modified,
            staged: false,
            managed: true,
        });

        let displayed = prioritized_changes(&changes);
        assert_eq!(displayed.len(), MAX_DISPLAYED_CHANGES);
        assert_eq!(displayed[0].path, "src/content/blog/article.md");
    }

    #[test]
    fn split_position_collapses_or_restores_a_bounded_detail_height() {
        assert_eq!(git_split_position(100, 700, false, 340), 700);
        assert_eq!(git_split_position(100, 700, true, 340), 360);
        assert_eq!(git_split_position(500, 700, true, 340), 500);
    }

    #[test]
    fn primary_action_is_derived_from_orthogonal_state() {
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::NoRemote,
                SyncRelation::Unknown,
                1,
                0,
                0,
                0,
            )),
            PrimaryAction::Commit
        );
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::Tracking,
                SyncRelation::Behind,
                0,
                0,
                0,
                2,
            )),
            PrimaryAction::PullFastForward
        );
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::Tracking,
                SyncRelation::Behind,
                1,
                0,
                0,
                2,
            )),
            PrimaryAction::None
        );
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::Tracking,
                SyncRelation::Ahead,
                0,
                1,
                2,
                0,
            )),
            PrimaryAction::Push
        );
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::Tracking,
                SyncRelation::Ahead,
                1,
                0,
                2,
                0,
            )),
            PrimaryAction::Commit
        );
    }

    #[test]
    fn no_commit_with_managed_changes_recommends_commit() {
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::NoCommit,
                SyncRelation::Unknown,
                1,
                0,
                0,
                0,
            )),
            PrimaryAction::Commit
        );
    }

    #[test]
    fn no_commit_without_managed_changes_currently_uses_commit_path() {
        // 这是当前行为探针，不预先替产品决定“空仓库”最终应该显示什么。
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::NoCommit,
                SyncRelation::Unknown,
                0,
                0,
                0,
                0,
            )),
            PrimaryAction::Commit
        );
    }

    #[test]
    fn no_commit_with_only_unmanaged_changes_currently_uses_commit_path() {
        // 受管范围为空时，当前推荐动作仍由 NoCommit 分支决定；是否改变它
        // 留待产品语义确认后再调整。
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::NoCommit,
                SyncRelation::Unknown,
                0,
                1,
                0,
                0,
            )),
            PrimaryAction::Commit
        );
    }
}
