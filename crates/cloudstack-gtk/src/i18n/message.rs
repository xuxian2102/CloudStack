use std::borrow::Cow;
use std::collections::HashMap;

use fluent_bundle::FluentValue;

/// 用户可见消息的语义标识。实际文案只存在于 `locales/*/*.ftl`，不放进 Rust。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UiMessage {
    SaveSuccess,
    SaveSuccessWithNewerEdits,
    BatchSaveSuccessContinueGit,
    GitNoAction,
    GitNoActionTooltip,
    GitPrimaryActionTooltip,
    GitNoCommittableChanges,
    GitNoCommittableChangesTooltip,
    GitSaveBeforeAction { count: usize },
    GitSaveBeforeActionTooltip { count: usize },
    GitActionNone,
    GitActionInitialize,
    GitActionConfigureIdentity,
    GitActionCommit,
    GitActionConfigureRemote,
    GitActionPushUpstream,
    GitActionPush,
    GitActionPullFastForward,
    GitActionInitializeTooltip,
    GitActionConfigureIdentityTooltip,
    GitActionCommitTooltip,
    GitActionConfigureRemoteTooltip,
    GitActionPushUpstreamTooltip,
    GitActionPushTooltip,
    GitActionPullFastForwardTooltip,
    SettingsColorSchemeSystem,
    SettingsColorSchemeLight,
    SettingsColorSchemeDark,
    SettingsColorSchemeTitle,
    SettingsAutoReopenTitle,
    SettingsAutoReopenSubtitle,
    SettingsRestoreDocumentTitle,
    SettingsRestoreDocumentSubtitle,
    SettingsAppearanceGroup,
    SettingsOpenProjectGroup,
    SettingsGeneralPage,
    SettingsDialogTitle,
}

impl UiMessage {
    pub(crate) fn id(&self) -> &'static str {
        match self {
            Self::SaveSuccess => "save-success",
            Self::SaveSuccessWithNewerEdits => "save-success-with-newer-edits",
            Self::BatchSaveSuccessContinueGit => "batch-save-success-continue-git",
            Self::GitNoAction => "git-no-action",
            Self::GitNoActionTooltip => "git-no-action-tooltip",
            Self::GitPrimaryActionTooltip => "git-primary-action-tooltip",
            Self::GitNoCommittableChanges => "git-no-committable-changes",
            Self::GitNoCommittableChangesTooltip => "git-no-committable-changes-tooltip",
            Self::GitSaveBeforeAction { .. } => "git-save-before-action",
            Self::GitSaveBeforeActionTooltip { .. } => "git-save-before-action-tooltip",
            Self::GitActionNone => "git-action-none",
            Self::GitActionInitialize => "git-action-initialize",
            Self::GitActionConfigureIdentity => "git-action-configure-identity",
            Self::GitActionCommit => "git-action-commit",
            Self::GitActionConfigureRemote => "git-action-configure-remote",
            Self::GitActionPushUpstream => "git-action-push-upstream",
            Self::GitActionPush => "git-action-push",
            Self::GitActionPullFastForward => "git-action-pull-fast-forward",
            Self::GitActionInitializeTooltip => "git-action-initialize-tooltip",
            Self::GitActionConfigureIdentityTooltip => "git-action-configure-identity-tooltip",
            Self::GitActionCommitTooltip => "git-action-commit-tooltip",
            Self::GitActionConfigureRemoteTooltip => "git-action-configure-remote-tooltip",
            Self::GitActionPushUpstreamTooltip => "git-action-push-upstream-tooltip",
            Self::GitActionPushTooltip => "git-action-push-tooltip",
            Self::GitActionPullFastForwardTooltip => "git-action-pull-fast-forward-tooltip",
            Self::SettingsColorSchemeSystem => "settings-color-scheme-system",
            Self::SettingsColorSchemeLight => "settings-color-scheme-light",
            Self::SettingsColorSchemeDark => "settings-color-scheme-dark",
            Self::SettingsColorSchemeTitle => "settings-color-scheme-title",
            Self::SettingsAutoReopenTitle => "settings-auto-reopen-title",
            Self::SettingsAutoReopenSubtitle => "settings-auto-reopen-subtitle",
            Self::SettingsRestoreDocumentTitle => "settings-restore-document-title",
            Self::SettingsRestoreDocumentSubtitle => "settings-restore-document-subtitle",
            Self::SettingsAppearanceGroup => "settings-appearance-group",
            Self::SettingsOpenProjectGroup => "settings-open-project-group",
            Self::SettingsGeneralPage => "settings-general-page",
            Self::SettingsDialogTitle => "settings-dialog-title",
        }
    }

    pub(crate) fn args(&self) -> Option<HashMap<Cow<'static, str>, FluentValue<'static>>> {
        let count = match self {
            Self::GitSaveBeforeAction { count } | Self::GitSaveBeforeActionTooltip { count } => {
                *count
            }
            _ => return None,
        };
        Some(HashMap::from([(
            Cow::Borrowed("count"),
            FluentValue::from(count),
        )]))
    }
}

#[cfg(test)]
pub(crate) fn message_samples() -> Vec<UiMessage> {
    vec![
        UiMessage::SaveSuccess,
        UiMessage::SaveSuccessWithNewerEdits,
        UiMessage::BatchSaveSuccessContinueGit,
        UiMessage::GitNoAction,
        UiMessage::GitNoActionTooltip,
        UiMessage::GitPrimaryActionTooltip,
        UiMessage::GitNoCommittableChanges,
        UiMessage::GitNoCommittableChangesTooltip,
        UiMessage::GitSaveBeforeAction { count: 2 },
        UiMessage::GitSaveBeforeActionTooltip { count: 2 },
        UiMessage::GitActionNone,
        UiMessage::GitActionInitialize,
        UiMessage::GitActionConfigureIdentity,
        UiMessage::GitActionCommit,
        UiMessage::GitActionConfigureRemote,
        UiMessage::GitActionPushUpstream,
        UiMessage::GitActionPush,
        UiMessage::GitActionPullFastForward,
        UiMessage::GitActionInitializeTooltip,
        UiMessage::GitActionConfigureIdentityTooltip,
        UiMessage::GitActionCommitTooltip,
        UiMessage::GitActionConfigureRemoteTooltip,
        UiMessage::GitActionPushUpstreamTooltip,
        UiMessage::GitActionPushTooltip,
        UiMessage::GitActionPullFastForwardTooltip,
        UiMessage::SettingsColorSchemeSystem,
        UiMessage::SettingsColorSchemeLight,
        UiMessage::SettingsColorSchemeDark,
        UiMessage::SettingsColorSchemeTitle,
        UiMessage::SettingsAutoReopenTitle,
        UiMessage::SettingsAutoReopenSubtitle,
        UiMessage::SettingsRestoreDocumentTitle,
        UiMessage::SettingsRestoreDocumentSubtitle,
        UiMessage::SettingsAppearanceGroup,
        UiMessage::SettingsOpenProjectGroup,
        UiMessage::SettingsGeneralPage,
        UiMessage::SettingsDialogTitle,
    ]
}
