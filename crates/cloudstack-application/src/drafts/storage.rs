use std::path::PathBuf;

use cloudstack_core::model::{DraftDocument, ProjectContext};
use cloudstack_core::services::drafts;
use cloudstack_core::AppError;

/// 新旧两个自动恢复草稿目录之间的读写协调：读取时 primary 优先、primary
/// 没有时回退到 legacy；写入 primary 成功后清理同一篇文章残留的 legacy
/// 草稿；删除时两个目录都必须尝试执行，避免一个目录的错误让另一个目录
/// 留下会再次出现的旧草稿。单一目录内部的路径隔离、大小上限、原子替换、
/// 身份校验仍由 `cloudstack_core::services::drafts` 负责。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStorage {
    primary: PathBuf,
    legacy: Option<PathBuf>,
}

impl DraftStorage {
    pub fn new(primary: PathBuf, legacy: Option<PathBuf>) -> Self {
        Self { primary, legacy }
    }

    pub fn read(
        &self,
        context: &ProjectContext,
        post_id: &str,
    ) -> Result<Option<DraftDocument>, AppError> {
        if let Some(draft) = drafts::read(&self.primary, context, post_id)? {
            return Ok(Some(draft));
        }
        match &self.legacy {
            Some(legacy) => drafts::read(legacy, context, post_id),
            None => Ok(None),
        }
    }

    pub fn write(
        &self,
        context: &ProjectContext,
        post_id: &str,
        raw_frontmatter: Option<String>,
        body: String,
        base_revision: String,
    ) -> Result<(), AppError> {
        drafts::write(
            &self.primary,
            context,
            post_id,
            raw_frontmatter,
            body,
            base_revision,
        )?;
        if let Some(legacy) = &self.legacy {
            drafts::delete(legacy, context, post_id)?;
        }
        Ok(())
    }

    pub fn delete(&self, context: &ProjectContext, post_id: &str) -> Result<(), AppError> {
        let primary_result = drafts::delete(&self.primary, context, post_id);
        let legacy_result = self
            .legacy
            .as_ref()
            .map_or(Ok(()), |legacy| drafts::delete(legacy, context, post_id));
        primary_result.and(legacy_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::model::ProjectConfig;

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        ProjectContext,
        DraftStorage,
    ) {
        let project = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "disk\n").unwrap();
        let context = ProjectContext {
            root: root.clone(),
            content_root: root.clone(),
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig::default(),
        };
        let storage = DraftStorage::new(
            app_data.path().join("dev.xuxian.cloudstack"),
            Some(app_data.path().join("dev.xuxian.blogeditor")),
        );
        (project, app_data, context, storage)
    }

    fn write_at(path: &std::path::Path, context: &ProjectContext, body: &str) {
        drafts::write(
            path,
            context,
            "a.md",
            None,
            body.to_owned(),
            "revision".into(),
        )
        .unwrap();
    }

    #[test]
    fn primary_draft_wins_and_legacy_is_the_fallback() {
        let (_project, _app_data, context, storage) = fixture();
        let legacy = storage.legacy.as_ref().unwrap();
        write_at(legacy, &context, "legacy");
        assert_eq!(
            storage.read(&context, "a.md").unwrap().unwrap().body,
            "legacy"
        );

        write_at(&storage.primary, &context, "primary");
        assert_eq!(
            storage.read(&context, "a.md").unwrap().unwrap().body,
            "primary"
        );
    }

    #[test]
    fn writing_primary_removes_the_matching_legacy_draft() {
        let (_project, _app_data, context, storage) = fixture();
        let legacy = storage.legacy.as_ref().unwrap();
        write_at(legacy, &context, "legacy");

        storage
            .write(&context, "a.md", None, "primary".into(), "revision".into())
            .unwrap();

        assert!(drafts::read(legacy, &context, "a.md").unwrap().is_none());
        assert_eq!(
            storage.read(&context, "a.md").unwrap().unwrap().body,
            "primary"
        );
    }

    #[test]
    fn deleting_a_draft_clears_both_storage_locations() {
        let (_project, _app_data, context, storage) = fixture();
        let legacy = storage.legacy.as_ref().unwrap();
        write_at(&storage.primary, &context, "primary");
        write_at(legacy, &context, "legacy");

        storage.delete(&context, "a.md").unwrap();

        assert!(drafts::read(&storage.primary, &context, "a.md")
            .unwrap()
            .is_none());
        assert!(drafts::read(legacy, &context, "a.md").unwrap().is_none());
    }
}
