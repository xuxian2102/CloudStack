//! 保存完成时的纯判定逻辑，从 `window.rs` 搬迁过来（不依赖 GTK/glib/gio/adw/
//! webkit，是往 `cloudstack-application` 方向拆分的第一步）。行为跟搬迁前
//! 完全一致，只是换了位置——不在这一步引入新的状态或判定分支。

use std::collections::HashMap;

use cloudstack_core::model::PostDocument;
use cloudstack_core::text::TextFileFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveCompletionOutcome {
    /// 保存完成时当前显示的已经不是这篇文章（文档/项目已切换），不碰任何状态。
    NotCurrent,
    /// 还是这篇文章，保存期间没有新编辑，正常清空 dirty。
    Clean,
    /// 还是这篇文章，但保存期间又有新编辑（generation 已推进）：只更新
    /// revision 和 format 这两个描述"新磁盘基线"的属性，dirty/unsaved_documents
    /// 里的 body/frontmatter/draft 都保留（不能用旧保存覆盖用户更新后的正文），
    /// 也不 reconcile pending 图片（那需要当前正文的真实快照，留给下一次 Clean
    /// 保存处理）。
    RevisionOnly,
}

pub fn classify_save_completion(
    current_document_id: Option<&str>,
    saved_document_id: &str,
    current_document_epoch: u64,
    saved_document_epoch: u64,
    current_generation: u64,
    saved_generation: u64,
) -> SaveCompletionOutcome {
    if current_document_epoch != saved_document_epoch
        || current_document_id != Some(saved_document_id)
    {
        return SaveCompletionOutcome::NotCurrent;
    }
    if current_generation == saved_generation {
        SaveCompletionOutcome::Clean
    } else {
        SaveCompletionOutcome::RevisionOnly
    }
}

/// 保存成功后按分类结果落地状态变更。不碰 pending_assets——Clean 时还要做
/// reconcile，那是会碰磁盘的副作用，留在调用方处理。
#[allow(clippy::too_many_arguments)]
pub fn apply_successful_save(
    outcome: SaveCompletionOutcome,
    document: &mut Option<PostDocument>,
    unsaved_documents: &mut HashMap<String, PostDocument>,
    dirty: &mut bool,
    document_id: &str,
    revision: &str,
    format: TextFileFormat,
    raw_frontmatter: Option<String>,
    body: String,
) {
    match outcome {
        SaveCompletionOutcome::NotCurrent => {}
        SaveCompletionOutcome::Clean => {
            if let Some(current) = document.as_mut() {
                current.raw_frontmatter = raw_frontmatter;
                current.body = body;
                current.revision = revision.to_owned();
                current.format = format;
            }
            unsaved_documents.remove(document_id);
            *dirty = false;
        }
        SaveCompletionOutcome::RevisionOnly => {
            if let Some(current) = document.as_mut() {
                current.revision = revision.to_owned();
                current.format = format;
            }
            if let Some(unsaved) = unsaved_documents.get_mut(document_id) {
                unsaved.revision = revision.to_owned();
                unsaved.format = format;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudstack_core::text::LineEnding;

    fn sample_document() -> PostDocument {
        PostDocument {
            id: "hello.md".into(),
            relative_path: "hello.md".into(),
            raw_frontmatter: Some("title: x".into()),
            body: "old body".into(),
            revision: "old-revision".into(),
            format: sample_format(),
        }
    }

    fn sample_format() -> TextFileFormat {
        TextFileFormat {
            line_ending: LineEnding::Lf,
            has_final_newline: true,
        }
    }

    #[test]
    fn classify_save_completion_is_not_current_when_document_switched() {
        assert_eq!(
            classify_save_completion(Some("other.md"), "hello.md", 1, 1, 5, 5),
            SaveCompletionOutcome::NotCurrent
        );
        assert_eq!(
            classify_save_completion(None, "hello.md", 1, 1, 5, 5),
            SaveCompletionOutcome::NotCurrent
        );
    }

    #[test]
    fn classify_save_completion_is_not_current_when_document_epoch_advanced() {
        assert_eq!(
            classify_save_completion(Some("hello.md"), "hello.md", 2, 1, 5, 5),
            SaveCompletionOutcome::NotCurrent
        );
    }

    #[test]
    fn classify_save_completion_is_clean_when_generation_unchanged() {
        assert_eq!(
            classify_save_completion(Some("hello.md"), "hello.md", 1, 1, 5, 5),
            SaveCompletionOutcome::Clean
        );
    }

    #[test]
    fn classify_save_completion_keeps_dirty_when_generation_advanced() {
        assert_eq!(
            classify_save_completion(Some("hello.md"), "hello.md", 1, 1, 6, 5),
            SaveCompletionOutcome::RevisionOnly
        );
    }

    #[test]
    fn apply_successful_save_clean_clears_dirty_and_removes_unsaved_entry() {
        let mut document = Some(sample_document());
        let mut unsaved_documents = HashMap::new();
        unsaved_documents.insert("hello.md".to_string(), sample_document());
        let mut dirty = true;

        let new_format = TextFileFormat {
            line_ending: LineEnding::CrLf,
            has_final_newline: false,
        };
        apply_successful_save(
            SaveCompletionOutcome::Clean,
            &mut document,
            &mut unsaved_documents,
            &mut dirty,
            "hello.md",
            "new-revision",
            new_format,
            Some("title: y".into()),
            "new body".into(),
        );

        let document = document.unwrap();
        assert_eq!(document.revision, "new-revision");
        assert_eq!(document.body, "new body");
        assert_eq!(document.raw_frontmatter.as_deref(), Some("title: y"));
        assert_eq!(document.format, new_format);
        assert!(!dirty);
        assert!(!unsaved_documents.contains_key("hello.md"));
    }

    #[test]
    fn apply_successful_save_revision_only_syncs_both_revisions_and_keeps_dirty() {
        let mut document = Some(sample_document());
        let mut unsaved_documents = HashMap::new();
        unsaved_documents.insert("hello.md".to_string(), sample_document());
        let mut dirty = true;

        let new_format = TextFileFormat {
            line_ending: LineEnding::CrLf,
            has_final_newline: false,
        };
        apply_successful_save(
            SaveCompletionOutcome::RevisionOnly,
            &mut document,
            &mut unsaved_documents,
            &mut dirty,
            "hello.md",
            "new-revision",
            new_format,
            Some("title: y".into()),
            "new body".into(),
        );

        let document = document.unwrap();
        assert_eq!(document.revision, "new-revision");
        // body/raw_frontmatter 不应该被这次保存覆盖——buffer 里已经有更新的内容了。
        assert_eq!(document.body, "old body");
        assert_eq!(document.raw_frontmatter.as_deref(), Some("title: x"));
        assert_eq!(
            document.format, new_format,
            "revision-only 时 format 也要跟着落盘结果更新"
        );
        assert!(dirty, "generation 已推进时不能清空 dirty");
        let unsaved = unsaved_documents
            .get("hello.md")
            .expect("generation 已推进时不能移除 unsaved 条目");
        assert_eq!(
            unsaved.revision, "new-revision",
            "unsaved_documents 里的 revision 也必须跟着更新，否则下一次批量保存会用过期 revision"
        );
        assert_eq!(
            unsaved.format, new_format,
            "unsaved snapshot 的 format 描述的是它继承的磁盘基线，必须和 revision 一起更新，\
             否则切换文章再切回来会展示一个已经过期的换行风格"
        );
        assert_eq!(
            unsaved.body, "old body",
            "unsaved snapshot 的 body 不能被这次保存覆盖"
        );
    }

    #[test]
    fn apply_successful_save_not_current_touches_nothing() {
        let mut document = Some(sample_document());
        let mut unsaved_documents = HashMap::new();
        unsaved_documents.insert("hello.md".to_string(), sample_document());
        let mut dirty = true;

        apply_successful_save(
            SaveCompletionOutcome::NotCurrent,
            &mut document,
            &mut unsaved_documents,
            &mut dirty,
            "hello.md",
            "new-revision",
            sample_format(),
            Some("title: y".into()),
            "new body".into(),
        );

        assert_eq!(document.unwrap().revision, "old-revision");
        assert_eq!(
            unsaved_documents.get("hello.md").unwrap().revision,
            "old-revision"
        );
        assert!(dirty);
    }
}
