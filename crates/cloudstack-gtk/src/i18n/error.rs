use cloudstack_core::error::AppError;

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
}
