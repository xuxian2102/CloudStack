use cloudstack_core::model::{PostDocument, ProjectContext};
use cloudstack_core::services::posts;

use super::DraftStorage;

#[derive(Debug)]
pub struct BatchFailure {
    pub relative_path: String,
    pub error: String,
}

/// 保存成功但清理对应自动恢复草稿失败：不影响保存结果，只是这篇文章的
/// 草稿本该被清掉却还留着。GTK 决定要不要展示/记日志、用什么措辞。
#[derive(Debug)]
pub struct DraftCleanupWarning {
    pub relative_path: String,
    pub error: String,
}

#[derive(Debug)]
pub struct BatchSaveReport {
    pub saved: Vec<PostDocument>,
    pub failed: Vec<BatchFailure>,
    pub cleanup_warnings: Vec<DraftCleanupWarning>,
}

#[derive(Debug)]
pub struct DiscardReport {
    pub discarded: Vec<String>,
    pub failed: Vec<BatchFailure>,
}

pub fn save_documents(
    storage: &DraftStorage,
    context: &ProjectContext,
    documents: Vec<PostDocument>,
) -> BatchSaveReport {
    let mut report = BatchSaveReport {
        saved: Vec::new(),
        failed: Vec::new(),
        cleanup_warnings: Vec::new(),
    };
    for document in documents {
        match posts::write_post(
            context,
            &document.id,
            document.raw_frontmatter.as_deref(),
            &document.body,
            document.format,
            &document.revision,
        ) {
            Ok(result) => {
                let mut saved = document;
                saved.revision = result.revision;
                saved.format = result.format;
                if let Err(error) = storage.delete(context, &saved.id) {
                    report.cleanup_warnings.push(DraftCleanupWarning {
                        relative_path: saved.relative_path.clone(),
                        error: error.to_string(),
                    });
                }
                report.saved.push(saved);
            }
            Err(error) => report.failed.push(BatchFailure {
                relative_path: document.relative_path,
                error: error.to_string(),
            }),
        }
    }
    report
}

pub fn discard_documents(
    storage: &DraftStorage,
    context: &ProjectContext,
    documents: Vec<PostDocument>,
) -> DiscardReport {
    let mut report = DiscardReport {
        discarded: Vec::new(),
        failed: Vec::new(),
    };
    for document in documents {
        match storage.delete(context, &document.id) {
            Ok(()) => report.discarded.push(document.id),
            Err(error) => report.failed.push(BatchFailure {
                relative_path: document.relative_path,
                error: error.to_string(),
            }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::model::ProjectConfig;
    use cloudstack_core::text::LineEnding;

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

    fn write_draft(storage: &DraftStorage, context: &ProjectContext, post_id: &str, body: &str) {
        storage
            .write(context, post_id, None, body.to_owned(), "revision".into())
            .unwrap();
    }

    #[test]
    fn batch_save_preserves_crlf_line_ending() {
        let (project, _app_data, context, storage) = fixture();
        std::fs::write(context.root.join("a.md"), "line1\r\nline2\r\n").unwrap();
        let mut document = posts::read_post(&context, "a.md").unwrap();
        assert_eq!(document.format.line_ending, LineEnding::CrLf);
        document.body = format!("{}line3\n", document.body);

        let report = save_documents(&storage, &context, vec![document]);

        assert!(report.failed.is_empty());
        assert_eq!(report.saved.len(), 1);
        assert_eq!(
            std::fs::read(project.path().join("a.md")).unwrap(),
            b"line1\r\nline2\r\nline3\r\n".to_vec(),
            "批量保存必须保留 CRLF 换行风格，不能规范化成 LF"
        );
        assert_eq!(report.saved[0].format.line_ending, LineEnding::CrLf);
    }

    #[test]
    fn batch_save_writes_each_snapshot_and_clears_its_draft() {
        let (project, _app_data, context, storage) = fixture();
        std::fs::write(context.root.join("b.md"), "old b\n").unwrap();
        let mut first = posts::read_post(&context, "a.md").unwrap();
        let mut second = posts::read_post(&context, "b.md").unwrap();
        first.body = "new a\n".into();
        second.body = "new b\n".into();
        write_draft(&storage, &context, "a.md", "new a\n");

        let report = save_documents(&storage, &context, vec![first, second]);

        assert!(report.failed.is_empty());
        assert_eq!(report.saved.len(), 2);
        assert_eq!(
            std::fs::read_to_string(project.path().join("a.md")).unwrap(),
            "new a\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("b.md")).unwrap(),
            "new b\n"
        );
        assert!(storage.read(&context, "a.md").unwrap().is_none());
    }

    #[test]
    fn batch_save_keeps_external_conflict_as_a_failed_article() {
        let (_project, _app_data, context, storage) = fixture();
        let mut document = posts::read_post(&context, "a.md").unwrap();
        document.body = "edited\n".into();
        std::fs::write(context.root.join("a.md"), "changed elsewhere\n").unwrap();

        let report = save_documents(&storage, &context, vec![document]);

        assert!(report.saved.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].relative_path, "a.md");
        assert!(report.failed[0].error.contains("外部"));
    }

    #[test]
    fn batch_save_keeps_success_and_failure_independent() {
        let (project, _app_data, context, storage) = fixture();
        std::fs::write(context.root.join("b.md"), "old b\n").unwrap();
        let mut saved = posts::read_post(&context, "a.md").unwrap();
        let mut failed = posts::read_post(&context, "b.md").unwrap();
        saved.body = "new a\n".into();
        failed.body = "new b\n".into();
        write_draft(&storage, &context, "b.md", &failed.body);
        std::fs::write(context.root.join("b.md"), "changed elsewhere\n").unwrap();

        let report = save_documents(&storage, &context, vec![saved, failed]);

        assert_eq!(report.saved.len(), 1);
        assert_eq!(report.saved[0].id, "a.md");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].relative_path, "b.md");
        assert_eq!(
            std::fs::read_to_string(project.path().join("a.md")).unwrap(),
            "new a\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("b.md")).unwrap(),
            "changed elsewhere\n"
        );
        assert!(storage.read(&context, "a.md").unwrap().is_none());
        assert!(storage.read(&context, "b.md").unwrap().is_some());
    }

    #[test]
    fn batch_discard_removes_all_recovery_drafts() {
        let (_project, _app_data, context, storage) = fixture();
        write_draft(&storage, &context, "a.md", "discard me");
        let document = posts::read_post(&context, "a.md").unwrap();

        let report = discard_documents(&storage, &context, vec![document]);

        assert_eq!(report.discarded, vec!["a.md"]);
        assert!(report.failed.is_empty());
        assert!(storage.read(&context, "a.md").unwrap().is_none());
    }
}
