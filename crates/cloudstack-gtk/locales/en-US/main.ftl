save-success = Article saved
save-success-with-newer-edits = Article saved, but newer edits are still unsaved
batch-save-success-continue-git = Unsaved articles saved; continue with the Git operation

git-no-action = Up to date
git-no-action-tooltip = There is no Git operation to perform
git-primary-action-tooltip = Perform the suggested Git operation
git-no-committable-changes = No changes to commit
git-no-committable-changes-tooltip = There are no managed changes to commit
git-save-before-action =
    { $count ->
        [one] Save 1 article first
       *[other] Save { $count } articles first
    }
git-save-before-action-tooltip =
    { $count ->
        [one] Save 1 unsaved article before the Git operation
       *[other] Save { $count } unsaved articles before the Git operation
    }

git-action-none = No action
git-action-initialize = Initialize
git-action-configure-identity = Identity
git-action-commit = Commit
git-action-configure-remote = Remote
git-action-push-upstream = Push
git-action-push = Push
git-action-pull-fast-forward = Sync
git-action-initialize-tooltip = Initialize Git
git-action-configure-identity-tooltip = Configure commit identity
git-action-commit-tooltip = Commit managed changes
git-action-configure-remote-tooltip = Configure remote
git-action-push-upstream-tooltip = Push for the first time
git-action-push-tooltip = Push commits
git-action-pull-fast-forward-tooltip = Fast-forward sync

settings-color-scheme-system = Follow system
settings-color-scheme-light = Light
settings-color-scheme-dark = Dark
settings-color-scheme-title = Color scheme
settings-auto-reopen-title = Open the most recent project on startup
settings-auto-reopen-subtitle = Skip the welcome page and open the last project directly
settings-restore-document-title = Reopen the last document in a project
settings-restore-document-subtitle = Remember the last document opened in each project
settings-appearance-group = Appearance
settings-open-project-group = Open project
settings-general-page = General
settings-dialog-title = Settings
