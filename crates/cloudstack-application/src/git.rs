//! Git 主操作决策：给定当前仓库快照和编辑器状态，用户下一步应该做什么。
//! 不依赖 GTK——GTK 只负责把这里算出的动作渲染成按钮文字/提示/可用状态。

use cloudstack_core::model::{RepositorySnapshot, RepositoryTopology, SyncRelation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryGitAction {
    None,
    Initialize,
    ConfigureIdentity,
    Commit,
    ConfigureRemote,
    PushUpstream,
    Push,
    PullFastForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveGitAction {
    None,
    NoChanges,
    SaveBeforeGit { unsaved_count: usize },
    Git(PrimaryGitAction),
}

pub fn recommended_action(snapshot: &RepositorySnapshot) -> PrimaryGitAction {
    if !snapshot.environment.git_available
        || snapshot.worktree.has_conflicts
        || snapshot.topology == RepositoryTopology::ParentRepository
        || snapshot.topology == RepositoryTopology::Detached
        || snapshot.sync == SyncRelation::Diverged
    {
        return PrimaryGitAction::None;
    }
    if snapshot.identity.is_none() && snapshot.worktree.managed_changes > 0 {
        return PrimaryGitAction::ConfigureIdentity;
    }
    match snapshot.topology {
        RepositoryTopology::NotInitialized => PrimaryGitAction::Initialize,
        RepositoryTopology::NoCommit => {
            if snapshot.worktree.managed_changes > 0 {
                PrimaryGitAction::Commit
            } else {
                PrimaryGitAction::None
            }
        }
        RepositoryTopology::NoRemote => {
            if snapshot.worktree.managed_changes > 0 {
                PrimaryGitAction::Commit
            } else {
                PrimaryGitAction::ConfigureRemote
            }
        }
        RepositoryTopology::NoUpstream => {
            if snapshot.worktree.managed_changes > 0 {
                PrimaryGitAction::Commit
            } else {
                PrimaryGitAction::PushUpstream
            }
        }
        RepositoryTopology::Tracking => {
            if snapshot.status.behind > 0 {
                if snapshot.status.ahead == 0 && snapshot.worktree.is_clean() {
                    PrimaryGitAction::PullFastForward
                } else {
                    PrimaryGitAction::None
                }
            } else if snapshot.worktree.managed_changes > 0 {
                PrimaryGitAction::Commit
            } else if snapshot.status.ahead > 0 {
                PrimaryGitAction::Push
            } else {
                PrimaryGitAction::None
            }
        }
        RepositoryTopology::ParentRepository | RepositoryTopology::Detached => {
            PrimaryGitAction::None
        }
    }
}

pub fn effective_action(
    snapshot: Option<&RepositorySnapshot>,
    busy: bool,
    unsaved_count: usize,
) -> EffectiveGitAction {
    if busy {
        return EffectiveGitAction::None;
    }
    if unsaved_count > 0 {
        return EffectiveGitAction::SaveBeforeGit { unsaved_count };
    }
    let Some(snapshot) = snapshot else {
        return EffectiveGitAction::None;
    };
    if snapshot.topology == RepositoryTopology::NoCommit
        && snapshot.worktree.managed_changes == 0
        && !snapshot.worktree.has_conflicts
    {
        return EffectiveGitAction::NoChanges;
    }
    EffectiveGitAction::Git(recommended_action(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::model::{
        GitEnvironment, GitIdentity, GitRemote, GitStatus, WorktreeState,
    };

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
            PrimaryGitAction::Commit
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
            PrimaryGitAction::PullFastForward
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
            PrimaryGitAction::None
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
            PrimaryGitAction::Push
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
            PrimaryGitAction::Commit
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
            PrimaryGitAction::Commit
        );
    }

    #[test]
    fn no_commit_without_managed_changes_has_no_recommended_action() {
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::NoCommit,
                SyncRelation::Unknown,
                0,
                0,
                0,
                0,
            )),
            PrimaryGitAction::None
        );
    }

    #[test]
    fn no_commit_with_only_unmanaged_changes_has_no_recommended_action() {
        assert_eq!(
            recommended_action(&snapshot(
                RepositoryTopology::NoCommit,
                SyncRelation::Unknown,
                0,
                1,
                0,
                0,
            )),
            PrimaryGitAction::None
        );
    }

    #[test]
    fn effective_action_without_managed_changes_explains_disabled_button() {
        let snapshot = snapshot(
            RepositoryTopology::NoCommit,
            SyncRelation::Unknown,
            0,
            0,
            0,
            0,
        );
        assert_eq!(
            effective_action(Some(&snapshot), false, 0),
            EffectiveGitAction::NoChanges
        );
    }

    #[test]
    fn unsaved_documents_override_repository_action() {
        let snapshot = snapshot(
            RepositoryTopology::Tracking,
            SyncRelation::Ahead,
            0,
            0,
            1,
            0,
        );
        assert_eq!(
            effective_action(Some(&snapshot), false, 2),
            EffectiveGitAction::SaveBeforeGit { unsaved_count: 2 }
        );
    }

    #[test]
    fn busy_disables_effective_action_before_other_state() {
        let snapshot = snapshot(
            RepositoryTopology::Tracking,
            SyncRelation::Ahead,
            0,
            0,
            1,
            0,
        );
        assert_eq!(
            effective_action(Some(&snapshot), true, 2),
            EffectiveGitAction::None
        );
    }

    #[test]
    fn clean_editor_uses_repository_action() {
        let snapshot = snapshot(
            RepositoryTopology::Tracking,
            SyncRelation::Ahead,
            0,
            0,
            1,
            0,
        );
        assert_eq!(
            effective_action(Some(&snapshot), false, 0),
            EffectiveGitAction::Git(PrimaryGitAction::Push)
        );
    }
}
