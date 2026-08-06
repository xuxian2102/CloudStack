use cloudstack_core::error::{AppError, ErrorPayload};

use super::UiMessage;

/// 用户主提示与开发诊断分离，避免把 core 的原始错误文本直接嵌入翻译文案。
#[derive(Debug, Clone)]
pub(crate) struct UserFacingError {
    pub(crate) message: UiMessage,
    pub(crate) diagnostic: Option<String>,
}

pub(crate) fn user_facing_message(
    message: UiMessage,
    diagnostic: impl Into<String>,
) -> UserFacingError {
    UserFacingError {
        message,
        diagnostic: Some(diagnostic.into()),
    }
}

pub(crate) fn user_facing_error(error: &AppError) -> UserFacingError {
    if matches!(error, AppError::Git(_)) {
        return git_error(error);
    }
    let message = match error {
        AppError::ExternalModificationConflict => UiMessage::ErrorRevisionConflict,
        AppError::InvalidProject(_) => UiMessage::ErrorInvalidProject,
        AppError::MissingProjectConfig => UiMessage::ErrorMissingProjectConfig,
        AppError::MissingContentDirectory(path) => {
            UiMessage::ErrorMissingContentDirectory { path: path.clone() }
        }
        AppError::Config(_) => UiMessage::ErrorProjectConfig,
        AppError::AlreadyExists(target) => UiMessage::ErrorArticleAlreadyExists {
            target: target.clone(),
        },
        AppError::InvalidPostId(path) => UiMessage::ErrorInvalidArticlePath { path: path.clone() },
        _ => UiMessage::ErrorGeneric,
    };

    user_facing_message(message, error.to_string())
}

pub(crate) fn git_error(error: &AppError) -> UserFacingError {
    let detail = match error {
        AppError::Git(detail) => detail.as_str(),
        _ => "",
    };
    let normalized = detail.to_ascii_lowercase();
    let message =
        if detail.contains(cloudstack_core::services::git::UNSUPPORTED_PATH_ENCODING_ERROR) {
            UiMessage::GitErrorUnsupportedPathEncoding
        } else if detail.contains("项目尚未初始化") || detail.contains("不是 git 仓库") {
            UiMessage::GitErrorRepositoryNotInitialized
        } else if detail.contains("没有配置 origin")
            || detail.contains("没有可获取的远端")
            || detail.contains("远端地址")
        {
            UiMessage::GitErrorNoRemote
        } else if detail.contains("没有 upstream") || detail.contains("上游") {
            UiMessage::GitErrorNoUpstream
        } else if detail.contains("未安装 GitHub CLI") {
            UiMessage::GitGhMissing
        } else if detail.contains("gh 尚未登录") || detail.contains("尚未登录") {
            UiMessage::GitGhNotAuthenticated
        } else if detail.contains("认证")
            || detail.contains("凭据")
            || detail.contains("SSH key")
            || normalized.contains("authentication")
            || normalized.contains("publickey")
        {
            UiMessage::GitErrorAuthentication
        } else if detail.contains("超时") || normalized.contains("timed out") {
            UiMessage::GitErrorTimeout
        } else if detail.contains("冲突") || detail.contains("分叉") {
            UiMessage::GitErrorConflict
        } else if detail.contains("远端包含本地没有") || normalized.contains("rejected") {
            UiMessage::GitErrorPushRejected
        } else {
            UiMessage::GitErrorOperation
        };
    user_facing_message(message, error.to_string())
}

pub(crate) fn git_payload_error(payload: &ErrorPayload) -> UserFacingError {
    let message = match payload.code() {
        "git_unresolved_conflicts" => UiMessage::GitErrorConflict,
        "git_nothing_to_commit" => UiMessage::GitErrorNothingToCommit,
        "git_stage_failed" => UiMessage::GitErrorStage,
        "git_commit_failed" => UiMessage::GitErrorCommit,
        "git_push_no_upstream" => UiMessage::GitErrorNoUpstream,
        "git_push_authentication_failed" => UiMessage::GitErrorAuthentication,
        "git_push_failed" | "git_push_failed_detail" => UiMessage::GitErrorPushRejected,
        _ => UiMessage::GitErrorOperation,
    };
    user_facing_message(message, payload.fallback().to_owned())
}

pub(crate) fn asset_save_error(error: &AppError) -> UserFacingError {
    user_facing_message(UiMessage::ErrorAssetSave, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_conflict_maps_to_localized_message() {
        let mapped = user_facing_error(&AppError::ExternalModificationConflict);
        assert_eq!(mapped.message, UiMessage::ErrorRevisionConflict);
        assert!(mapped
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("外部被修改")));
    }

    #[test]
    fn project_config_error_maps_without_leaking_detail() {
        let mapped = user_facing_error(&AppError::Config("invalid YAML".to_owned()));
        assert_eq!(mapped.message, UiMessage::ErrorProjectConfig);
        assert!(!matches!(mapped.message, UiMessage::ErrorGeneric));
        assert!(mapped
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("invalid YAML")));
    }

    #[test]
    fn post_path_conflict_keeps_target_as_a_translation_argument() {
        let mapped = user_facing_error(&AppError::AlreadyExists("notes/new.md".to_owned()));
        assert_eq!(
            mapped.message,
            UiMessage::ErrorArticleAlreadyExists {
                target: "notes/new.md".to_owned()
            }
        );
        assert!(mapped
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("notes/new.md")));
    }

    #[test]
    fn unknown_error_uses_generic_message() {
        let mapped = user_facing_error(&AppError::Io("permission denied".to_owned()));
        assert_eq!(mapped.message, UiMessage::ErrorGeneric);
        assert!(mapped.diagnostic.is_some());
    }

    #[test]
    fn diagnostic_is_not_inserted_into_primary_message() {
        let mapped = user_facing_error(&AppError::Config("secret detail".to_owned()));
        let rendered = super::super::text(mapped.message);
        assert!(!rendered.contains("secret detail"));
    }

    #[test]
    fn asset_save_uses_an_image_specific_primary_message() {
        let mapped = asset_save_error(&AppError::Io("permission denied".to_owned()));
        assert_eq!(mapped.message, UiMessage::ErrorAssetSave);
        assert!(mapped
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("permission denied")));
    }

    #[test]
    fn git_authentication_error_maps_to_a_localized_primary_message() {
        let mapped = git_error(&AppError::Git("认证失败".to_owned()));
        assert_eq!(mapped.message, UiMessage::GitErrorAuthentication);
        assert!(mapped
            .diagnostic
            .as_deref()
            .is_some_and(|detail| detail.contains("认证失败")));
    }

    #[test]
    fn git_push_rejection_does_not_expose_raw_fallback_in_primary_message() {
        let payload = ErrorPayload::git_push_failed_detail(
            "推送失败：remote rejected",
            "remote rejected: non-fast-forward",
        );
        let mapped = git_payload_error(&payload);
        assert_eq!(mapped.message, UiMessage::GitErrorPushRejected);
        let rendered = super::super::text(mapped.message);
        assert!(!rendered.contains("remote rejected"));
        assert_eq!(
            mapped.diagnostic.as_deref(),
            Some("推送失败：remote rejected")
        );
    }

    #[test]
    fn git_topology_errors_keep_actionable_categories() {
        assert_eq!(
            git_error(&AppError::Git("没有配置 origin 远端".to_owned())).message,
            UiMessage::GitErrorNoRemote
        );
        assert_eq!(
            git_error(&AppError::Git("当前分支没有 upstream".to_owned())).message,
            UiMessage::GitErrorNoUpstream
        );
        assert_eq!(
            git_error(&AppError::Git("未安装 GitHub CLI（gh）".to_owned())).message,
            UiMessage::GitGhMissing
        );
    }

    #[test]
    fn git_unsupported_path_encoding_gets_its_own_actionable_message() {
        let mapped = git_error(&AppError::Git(
            cloudstack_core::services::git::UNSUPPORTED_PATH_ENCODING_ERROR.to_owned(),
        ));
        assert_eq!(mapped.message, UiMessage::GitErrorUnsupportedPathEncoding);
    }
}
