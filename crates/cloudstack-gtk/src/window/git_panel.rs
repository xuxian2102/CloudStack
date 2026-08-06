use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use cloudstack_core::error::AppError;
use cloudstack_core::model::{
    ChangeKind, FileChange, GitOperationResult, OperationReport, RepositorySnapshot,
    RepositoryTopology, RepositoryVisibility, SyncRelation, WorktreeState,
};
use cloudstack_core::services::git;
use gtk::glib;

use super::{
    drafts, has_unsaved_documents, open_project, set_busy, show_error, show_user_facing,
    show_user_facing_error, toast, EditorState, Widgets,
};
use crate::i18n::{self, UiMessage};
use crate::tasks;
use cloudstack_application::git::{
    effective_action, recommended_action, EffectiveGitAction, PrimaryGitAction,
};
use cloudstack_application::should_apply_git_refresh;

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

impl GitPanel {
    pub(super) fn new() -> Self {
        let toggle_button = gtk::Button::builder()
            .icon_name("pan-up-symbolic")
            .tooltip_text(i18n::text(UiMessage::GitExpandDetails))
            .css_classes(["flat"])
            .build();
        let summary_label = gtk::Label::builder()
            .label(i18n::text(UiMessage::GitProjectNotOpen))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        let publish_button = gtk::Button::builder()
            .label(i18n::text(UiMessage::GitActionCommit))
            .tooltip_text(i18n::text(UiMessage::GitPrimaryActionTooltip))
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
            .label(i18n::text(UiMessage::GitDetailsTitle))
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(i18n::text(UiMessage::GitRefreshTooltip))
            .action_name("win.refresh-git")
            .sensitive(false)
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.append(&title);

        let repository_label = status_label(&i18n::text(UiMessage::GitProjectNotOpen));
        let sync_label = status_label(&i18n::text(UiMessage::GitSyncUnknown));
        let worktree_label = status_label(&i18n::text(UiMessage::GitWorktreeUnknown));
        let remote_heading = section_heading(&i18n::text(UiMessage::GitRemoteTitle));
        let remote_label = status_label(&i18n::text(UiMessage::GitRemoteNotConfigured));
        remote_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        remote_label.set_wrap(false);
        let changes_title = section_heading(&i18n::text(UiMessage::GitChangesTitle));
        let changes_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        set_changes_placeholder(&changes_list, &i18n::text(UiMessage::GitNoChanges));
        let fetch_button = gtk::Button::builder()
            .label(i18n::text(UiMessage::GitFetch))
            .tooltip_text(i18n::text(UiMessage::GitFetchTooltip))
            .action_name("win.fetch-git")
            .sensitive(false)
            .build();
        let untrack_config_button = gtk::Button::builder()
            .label(i18n::text(UiMessage::GitUntrackConfig))
            .tooltip_text(i18n::text(UiMessage::GitUntrackConfigTooltip))
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
        let tooltip = if expanded {
            i18n::text(UiMessage::GitCollapseDetails)
        } else {
            i18n::text(UiMessage::GitExpandDetails)
        };
        self.toggle_button.set_tooltip_text(Some(&tooltip));
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
        self.summary_label
            .set_label(&i18n::text(UiMessage::GitLoadingSummary));
        self.repository_label
            .set_label(&i18n::text(UiMessage::GitLoadingRepository));
        self.sync_label
            .set_label(&i18n::text(UiMessage::GitLoadingSync));
        self.worktree_label
            .set_label(&i18n::text(UiMessage::GitLoadingWorktree));
        self.remote_label
            .set_label(&i18n::text(UiMessage::GitLoadingRemote));
        self.changes_title
            .set_label(&i18n::text(UiMessage::GitChangesTitle));
        set_changes_placeholder(
            &self.changes_list,
            &i18n::text(UiMessage::GitLoadingChanges),
        );
        self.refresh_button.set_sensitive(false);
        self.fetch_button.set_sensitive(false);
        self.untrack_config_button.set_sensitive(false);
        self.publish_button
            .set_label(&i18n::text(UiMessage::GitLoadingChanges));
        self.publish_button.set_sensitive(false);
    }

    pub(super) fn apply(&self, snapshot: &RepositorySnapshot) {
        if !snapshot.environment.git_available {
            self.summary_label
                .set_label(&i18n::text(UiMessage::GitNotInstalled));
            self.repository_label
                .set_label(&i18n::text(UiMessage::GitNotInstalled));
            self.sync_label
                .set_label(&i18n::text(UiMessage::GitInstallHint));
            self.worktree_label
                .set_label(&i18n::text(UiMessage::GitWorktreeUnknown));
            self.remote_label
                .set_label(&i18n::text(UiMessage::GitUnavailable));
            set_changes_placeholder(&self.changes_list, &i18n::text(UiMessage::GitUnavailable));
            self.refresh_button.set_sensitive(false);
            self.fetch_button.set_sensitive(false);
            self.untrack_config_button.set_visible(false);
            self.set_primary_action(EffectiveGitAction::None);
            return;
        }
        let branch = snapshot.status.branch.as_deref().unwrap_or("—");
        let summary = localized_summary_text(snapshot);
        self.summary_label.set_label(&summary);
        self.summary_label.set_tooltip_text(Some(&summary));
        self.repository_label
            .set_label(&localized_repository_text(snapshot, branch));
        self.sync_label.set_label(&localized_sync_text(snapshot));
        self.worktree_label
            .set_label(&localized_worktree_text(snapshot.worktree));
        let remote = localized_remote_text(snapshot);
        self.remote_label.set_label(&remote);
        self.remote_label.set_tooltip_text(Some(&remote));
        self.changes_title
            .set_label(&i18n::text(UiMessage::GitChangesCount {
                count: snapshot.status.changes.len(),
            }));
        populate_changes(&self.changes_list, snapshot);
        self.refresh_button
            .set_sensitive(snapshot.environment.git_available);
        self.fetch_button
            .set_sensitive(!snapshot.remotes.is_empty());
        self.untrack_config_button
            .set_visible(snapshot.config_tracked);
        self.untrack_config_button
            .set_sensitive(snapshot.config_tracked);
        self.set_primary_action(EffectiveGitAction::Git(recommended_action(snapshot)));
    }

    pub(super) fn set_error(&self, message: &str) {
        self.summary_label
            .set_label(&i18n::text(UiMessage::GitStatusReadFailed));
        self.repository_label
            .set_label(&i18n::text(UiMessage::GitStatusReadFailed));
        self.sync_label.set_label(message);
        self.worktree_label
            .set_label(&i18n::text(UiMessage::GitWorktreeUnknown));
        self.remote_label
            .set_label(&i18n::text(UiMessage::GitUnknown));
        set_changes_placeholder(&self.changes_list, &i18n::text(UiMessage::GitUnknown));
        self.refresh_button.set_sensitive(true);
        self.fetch_button.set_sensitive(false);
        self.untrack_config_button.set_visible(false);
        self.set_primary_action(EffectiveGitAction::None);
    }

    pub(super) fn set_project_available(&self, available: bool) {
        if !available {
            self.refresh_button.set_sensitive(false);
            self.repository_label
                .set_label(&i18n::text(UiMessage::GitProjectNotOpen));
            self.summary_label
                .set_label(&i18n::text(UiMessage::GitProjectNotOpen));
            self.sync_label
                .set_label(&i18n::text(UiMessage::GitSyncUnknown));
            self.worktree_label
                .set_label(&i18n::text(UiMessage::GitWorktreeUnknown));
            self.remote_label
                .set_label(&i18n::text(UiMessage::GitRemoteNotConfigured));
            self.changes_title
                .set_label(&i18n::text(UiMessage::GitChangesTitle));
            set_changes_placeholder(&self.changes_list, &i18n::text(UiMessage::GitNoChanges));
            self.fetch_button.set_sensitive(false);
            self.untrack_config_button.set_visible(false);
            self.set_primary_action(EffectiveGitAction::None);
        }
    }

    pub(super) fn reflect_unsaved_editor(&self, dirty: bool) {
        let prefix = i18n::text(UiMessage::GitUnsavedPrefix);
        let prefix_with_separator = format!("{prefix} · ");
        if dirty {
            let current = self.summary_label.text();
            if !current.starts_with(&prefix_with_separator) {
                self.summary_label
                    .set_label(&format!("{prefix_with_separator}{current}"));
            }
        } else {
            let current = self.summary_label.text();
            if let Some(current) = current.strip_prefix(&prefix_with_separator) {
                self.summary_label.set_label(current);
            }
        }
    }

    pub(super) fn set_primary_action(&self, action: EffectiveGitAction) {
        match action {
            EffectiveGitAction::None => {
                self.publish_button
                    .set_label(&i18n::text(UiMessage::GitNoAction));
                self.publish_button
                    .set_tooltip_text(Some(&i18n::text(UiMessage::GitNoActionTooltip)));
                self.publish_button.set_sensitive(false);
            }
            EffectiveGitAction::NoChanges => {
                self.publish_button
                    .set_label(&i18n::text(UiMessage::GitNoCommittableChanges));
                self.publish_button
                    .set_tooltip_text(Some(&i18n::text(UiMessage::GitNoCommittableChangesTooltip)));
                self.publish_button.set_sensitive(false);
            }
            EffectiveGitAction::SaveBeforeGit { unsaved_count } => {
                let label = i18n::text(UiMessage::GitSaveBeforeAction {
                    count: unsaved_count,
                });
                let tooltip = i18n::text(UiMessage::GitSaveBeforeActionTooltip {
                    count: unsaved_count,
                });
                self.publish_button.set_label(&label);
                self.publish_button.set_tooltip_text(Some(&tooltip));
                self.publish_button.set_sensitive(true);
            }
            EffectiveGitAction::Git(action) => {
                let label = compact_action_label(action);
                let tooltip = action_label(action);
                self.publish_button.set_label(&label);
                self.publish_button.set_tooltip_text(Some(&tooltip));
                self.publish_button
                    .set_sensitive(action != PrimaryGitAction::None);
            }
        }
    }
}

pub(super) fn untrack_config(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    confirm_operation(
        widgets,
        state,
        &i18n::text(UiMessage::GitUntrackHeading),
        &i18n::text(UiMessage::GitUntrackBody),
        &i18n::text(UiMessage::GitUntrackAction),
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

fn action_label(action: PrimaryGitAction) -> String {
    let message = match action {
        PrimaryGitAction::None => UiMessage::GitActionNone,
        PrimaryGitAction::Initialize => UiMessage::GitActionInitializeTooltip,
        PrimaryGitAction::ConfigureIdentity => UiMessage::GitActionConfigureIdentityTooltip,
        PrimaryGitAction::Commit => UiMessage::GitActionCommitTooltip,
        PrimaryGitAction::ConfigureRemote => UiMessage::GitActionConfigureRemoteTooltip,
        PrimaryGitAction::PushUpstream => UiMessage::GitActionPushUpstreamTooltip,
        PrimaryGitAction::Push => UiMessage::GitActionPushTooltip,
        PrimaryGitAction::PullFastForward => UiMessage::GitActionPullFastForwardTooltip,
    };
    i18n::text(message)
}

fn compact_action_label(action: PrimaryGitAction) -> String {
    let message = match action {
        PrimaryGitAction::None => UiMessage::GitNoAction,
        PrimaryGitAction::Initialize => UiMessage::GitActionInitialize,
        PrimaryGitAction::ConfigureIdentity => UiMessage::GitActionConfigureIdentity,
        PrimaryGitAction::Commit => UiMessage::GitActionCommit,
        PrimaryGitAction::ConfigureRemote => UiMessage::GitActionConfigureRemote,
        PrimaryGitAction::PushUpstream => UiMessage::GitActionPushUpstream,
        PrimaryGitAction::Push => UiMessage::GitActionPush,
        PrimaryGitAction::PullFastForward => UiMessage::GitActionPullFastForward,
    };
    i18n::text(message)
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

#[cfg(test)]
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

fn localized_summary_text(snapshot: &RepositorySnapshot) -> String {
    let branch = snapshot.status.branch.as_deref().unwrap_or("—");
    let destination = snapshot.status.upstream.as_deref().map_or_else(
        || branch.to_owned(),
        |upstream| format!("{branch} → {upstream}"),
    );
    let count = snapshot.status.changes.len();
    match snapshot.topology {
        RepositoryTopology::NotInitialized => {
            i18n::text(UiMessage::GitSummaryNotInitialized { count })
        }
        RepositoryTopology::ParentRepository => i18n::text(UiMessage::GitSummaryParentRepository),
        RepositoryTopology::Detached => i18n::text(UiMessage::GitSummaryDetached { count }),
        _ if count == 0 => i18n::text(UiMessage::GitSummaryClean { destination }),
        _ => i18n::text(UiMessage::GitSummaryChanges { destination, count }),
    }
}

fn localized_repository_text(snapshot: &RepositorySnapshot, branch: &str) -> String {
    i18n::text(UiMessage::GitRepositoryStatus {
        topology: localized_topology_text(snapshot.topology),
        branch: branch.to_owned(),
    })
}

fn localized_topology_text(topology: RepositoryTopology) -> String {
    let message = match topology {
        RepositoryTopology::NotInitialized => UiMessage::GitTopologyNotInitialized,
        RepositoryTopology::ParentRepository => UiMessage::GitTopologyParentRepository,
        RepositoryTopology::NoCommit => UiMessage::GitTopologyNoCommit,
        RepositoryTopology::NoRemote => UiMessage::GitTopologyNoRemote,
        RepositoryTopology::NoUpstream => UiMessage::GitTopologyNoUpstream,
        RepositoryTopology::Tracking => UiMessage::GitTopologyTracking,
        RepositoryTopology::Detached => UiMessage::GitBranchDetached,
    };
    i18n::text(message)
}

fn localized_remote_text(snapshot: &RepositorySnapshot) -> String {
    if snapshot.remotes.is_empty() {
        return i18n::text(UiMessage::GitRemoteNotConfigured);
    }
    snapshot
        .remotes
        .iter()
        .map(|remote| match &remote.url {
            Some(url) => i18n::text(UiMessage::GitRemoteEntry {
                name: remote.name.clone(),
                url: url.clone(),
            }),
            None => i18n::text(UiMessage::GitRemoteName {
                name: remote.name.clone(),
            }),
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
        list.append(&status_label(&i18n::text(UiMessage::GitNoChanges)));
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
            .label(localized_change_path_details(change))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .single_line_mode(true)
            .tooltip_text(localized_change_path_details(change))
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
        list.append(&status_label(&i18n::text(UiMessage::GitChangesOmitted {
            count: omitted,
        })));
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

#[cfg(test)]
fn change_path(change: &FileChange) -> String {
    let path = change.old_path.as_deref().map_or_else(
        || change.path.clone(),
        |old_path| format!("{old_path} → {}", change.path),
    );
    let scope = if change.managed { "" } else { " · 非受管" };
    let staged = if change.staged { " · 已暂存" } else { "" };
    format!("{path}{scope}{staged}")
}

fn localized_change_path(change: &FileChange) -> String {
    change.old_path.as_deref().map_or_else(
        || change.path.clone(),
        |old_path| {
            i18n::text(UiMessage::GitRenamedPath {
                old_path: old_path.to_owned(),
                path: change.path.clone(),
            })
        },
    )
}

fn localized_change_path_details(change: &FileChange) -> String {
    i18n::text(UiMessage::GitChangePath {
        scope: if change.managed {
            String::new()
        } else {
            i18n::text(UiMessage::GitUnmanagedSuffix)
        },
        staged: if change.staged {
            i18n::text(UiMessage::GitStagedSuffix)
        } else {
            String::new()
        },
        path: localized_change_path(change),
    })
}

#[cfg(test)]
fn change_text(change: &FileChange) -> String {
    format!("{}  {}", change_marker(change.kind), change_path(change))
}

fn localized_sync_text(snapshot: &RepositorySnapshot) -> String {
    match snapshot.sync {
        SyncRelation::Unknown => i18n::text(UiMessage::GitSyncUnknown),
        SyncRelation::Synced => i18n::text(UiMessage::GitSyncSynced),
        SyncRelation::Ahead => i18n::text(UiMessage::GitSyncAhead {
            count: snapshot.status.ahead,
        }),
        SyncRelation::Behind => i18n::text(UiMessage::GitSyncBehind {
            count: snapshot.status.behind,
        }),
        SyncRelation::Diverged => i18n::text(UiMessage::GitSyncDiverged {
            ahead: snapshot.status.ahead,
            behind: snapshot.status.behind,
        }),
    }
}

#[cfg(test)]
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

fn localized_worktree_text(worktree: WorktreeState) -> String {
    if worktree.has_conflicts {
        return i18n::text(UiMessage::GitWorktreeConflict);
    }
    if worktree.is_clean() {
        return i18n::text(UiMessage::GitWorktreeClean);
    }
    if worktree.staged_changes > 0 {
        i18n::text(UiMessage::GitWorktreeChanges {
            managed: worktree.managed_changes,
            unmanaged: worktree.unmanaged_changes,
            staged: worktree.staged_changes,
        })
    } else {
        i18n::text(UiMessage::GitWorktreeChangesNoStaged {
            managed: worktree.managed_changes,
            unmanaged: worktree.unmanaged_changes,
        })
    }
}

pub(super) fn activate_primary(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let action = {
        let state = state.borrow();
        effective_action(
            state.git_snapshot.as_ref(),
            state.busy,
            state.unsaved_documents.len(),
        )
    };
    let snapshot = state.borrow().git_snapshot.clone();
    match action {
        EffectiveGitAction::None => {
            if snapshot.is_none() {
                refresh(widgets, state);
            }
        }
        EffectiveGitAction::NoChanges => {}
        EffectiveGitAction::SaveBeforeGit { .. } => {
            drafts::save_all(widgets, state);
        }
        EffectiveGitAction::Git(action) => {
            let Some(snapshot) = snapshot else {
                return;
            };
            match action {
                PrimaryGitAction::None => {}
                PrimaryGitAction::Initialize => confirm_operation(
                    widgets,
                    state,
                    &i18n::text(UiMessage::GitInitHeading),
                    &i18n::text(UiMessage::GitInitBody),
                    &i18n::text(UiMessage::GitInitAction),
                    Completion::Refresh,
                    |context| git::initialize(&context),
                ),
                PrimaryGitAction::ConfigureIdentity => show_identity_dialog(widgets, state),
                PrimaryGitAction::Commit => {
                    if gtk::prelude::WidgetExt::activate_action(
                        &widgets.window,
                        "win.publish",
                        None,
                    )
                    .is_err()
                    {
                        show_error(widgets, &i18n::text(UiMessage::GitOpenPublishFailed));
                    }
                }
                PrimaryGitAction::ConfigureRemote => show_remote_dialog(widgets, state, &snapshot),
                PrimaryGitAction::PushUpstream => confirm_operation(
                    widgets,
                    state,
                    &i18n::text(UiMessage::GitPushUpstreamHeading),
                    &i18n::text(UiMessage::GitPushUpstreamBody),
                    &i18n::text(UiMessage::GitPushAction),
                    Completion::Refresh,
                    |context| git::push_upstream(&context),
                ),
                PrimaryGitAction::Push => confirm_operation(
                    widgets,
                    state,
                    &i18n::text(UiMessage::GitPushHeading),
                    &i18n::text(UiMessage::GitPushBody),
                    &i18n::text(UiMessage::GitPushAction),
                    Completion::Refresh,
                    |context| git::push(&context),
                ),
                PrimaryGitAction::PullFastForward => confirm_operation(
                    widgets,
                    state,
                    &i18n::text(UiMessage::GitPullHeading),
                    &i18n::text(UiMessage::GitPullBody),
                    &i18n::text(UiMessage::GitPullAction),
                    Completion::ReloadProject,
                    |context| git::pull_fast_forward(&context),
                ),
            }
        }
    }
}

fn show_identity_dialog(widgets: &Widgets, state: &Rc<RefCell<EditorState>>) {
    let name_entry = gtk::Entry::builder()
        .placeholder_text(i18n::text(UiMessage::GitIdentityPlaceholder))
        .build();
    let email_entry = gtk::Entry::builder()
        .placeholder_text("name@example.com") // i18n-allow: example value
        .build();
    let save_button = gtk::Button::builder()
        .label(i18n::text(UiMessage::GitIdentitySave))
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let note = gtk::Label::builder()
        .label(i18n::text(UiMessage::GitIdentityNote))
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
    let title = i18n::text(UiMessage::GitIdentityTitle);
    header.set_title_widget(Some(&gtk::Label::new(Some(&title))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(i18n::text(UiMessage::GitIdentityTitle))
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
        let heading = i18n::text(UiMessage::GitIdentityTitle);
        execute_operation(
            &save_widgets,
            &save_state,
            &heading,
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
    let heading = i18n::text(UiMessage::GitFetchOperation);
    execute_operation(widgets, state, &heading, Completion::Refresh, |context| {
        git::fetch(&context)
    });
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
    let cancel = i18n::text(UiMessage::Cancel);
    dialog.add_responses(&[("cancel", cancel.as_str()), ("confirm", action_label)]);
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
                        let mapped = i18n::git_error(&AppError::Git(error));
                        callback_dialog.result_label.set_label(&format!(
                            "{}：{}",
                            i18n::text(UiMessage::GitStopStage {
                                stage: "operation".into()
                            }),
                            i18n::text(mapped.message.clone())
                        ));
                        show_user_facing(&widgets, mapped);
                    } else {
                        let done = i18n::text(UiMessage::GitOperationDone);
                        callback_dialog.result_label.set_label(&done);
                        toast(&widgets, &done);
                        succeeded = true;
                    }
                }
                Err(error) => {
                    let mapped = i18n::git_error(&error);
                    callback_dialog.result_label.set_label(&format!(
                        "{}：{}",
                        i18n::text(UiMessage::GitStopStage {
                            stage: "before command".into(),
                        }),
                        i18n::text(mapped.message.clone())
                    ));
                    callback_dialog
                        .trace_buffer
                        .set_text(&i18n::text(UiMessage::GitNoExternalCommand));
                    show_user_facing(&widgets, mapped);
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
            .label(i18n::text(UiMessage::GitOperationExecuting))
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
        .placeholder_text(i18n::text(UiMessage::GitRemoteUrlPlaceholder))
        .build();
    let setup_credentials =
        gtk::CheckButton::with_label(&i18n::text(UiMessage::GitGhCredentialOption));
    setup_credentials.set_sensitive(snapshot.environment.gh_authenticated);
    let add_button = gtk::Button::builder()
        .label(i18n::text(UiMessage::GitAddOrigin))
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let url_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    url_group.append(
        &gtk::Label::builder()
            .label(i18n::text(UiMessage::GitExistingRemote))
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
        .placeholder_text(i18n::text(UiMessage::GitRepositoryNamePlaceholder))
        .build();
    let private_check = gtk::CheckButton::with_label(&i18n::text(UiMessage::GitPrivateRepository));
    private_check.set_active(true);
    let create_button = gtk::Button::builder()
        .label(i18n::text(UiMessage::GitCreateAndPush))
        .sensitive(snapshot.environment.gh_authenticated)
        .build();
    let gh_status = if !snapshot.environment.gh_available {
        i18n::text(UiMessage::GitGhMissing)
    } else if !snapshot.environment.gh_authenticated {
        i18n::text(UiMessage::GitGhNotAuthenticated)
    } else {
        i18n::text(UiMessage::GitGhCreateNote)
    };
    let github_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    github_group.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    github_group.append(
        &gtk::Label::builder()
            .label(i18n::text(UiMessage::GitCreateGithubTitle))
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
    let title = i18n::text(UiMessage::GitRemoteConfigTitle);
    header.set_title_widget(Some(&gtk::Label::new(Some(&title))));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(title)
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
        let heading = i18n::text(UiMessage::GitConfigureOriginOperation);
        execute_operation(
            &add_widgets,
            &add_state,
            &heading,
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
        let heading = i18n::text(UiMessage::GitCreateGithubOperation);
        execute_operation(
            &create_widgets,
            &create_state,
            &heading,
            Completion::Refresh,
            move |context| git::create_github_repository(&context, &name, visibility),
        );
    });
    dialog.present(Some(&widgets.window));
    url_entry.grab_focus();
}

pub(super) fn operation_log(report: &OperationReport) -> String {
    if report.traces.is_empty() {
        return i18n::text(UiMessage::GitNoExternalCommand);
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
    let expected_generation = {
        let mut editor_state = state.borrow_mut();
        editor_state.git_refresh_generation = editor_state.git_refresh_generation.wrapping_add(1);
        editor_state.git_refresh_generation
    };
    let widgets = widgets.clone();
    let state = Rc::clone(state);
    tasks::run(
        move || git::snapshot(&context),
        move |result| {
            let still_current = {
                let editor_state = state.borrow();
                should_apply_git_refresh(
                    editor_state
                        .project
                        .as_ref()
                        .map(|current| current.root.as_path()),
                    &expected_root,
                    editor_state.git_refresh_generation,
                    expected_generation,
                )
            };
            if !still_current {
                return;
            }
            match result {
                Ok(snapshot) => {
                    widgets.git_panel.apply(&snapshot);
                    state.borrow_mut().git_snapshot = Some(snapshot);
                    super::sync_controls(&widgets, &state);
                }
                Err(error) => {
                    widgets
                        .git_panel
                        .set_error(&i18n::text(UiMessage::GitStatusReadFailed));
                    super::sync_controls(&widgets, &state);
                    show_user_facing_error(&widgets, &error);
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
}
