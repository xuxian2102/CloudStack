use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::model::{DraftDocument, ProjectContext};
use crate::path_guard::resolve_post_path;

const MAX_DRAFT_BYTES: usize = 20 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDraft {
    project_root: String,
    draft: DraftDocument,
}

fn draft_path(app_data_dir: &Path, ctx: &ProjectContext, post_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(ctx.root.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(post_id.as_bytes());
    let key = format!("{:x}", hasher.finalize());
    app_data_dir.join("drafts").join(format!("{key}.json"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn write(
    app_data_dir: &Path,
    ctx: &ProjectContext,
    post_id: &str,
    raw_frontmatter: Option<String>,
    body: String,
    base_revision: String,
) -> Result<(), AppError> {
    let post_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    if !post_path.is_file() {
        return Err(AppError::NotFound(post_id.to_owned()));
    }
    if raw_frontmatter.as_ref().map_or(0, String::len) + body.len() > MAX_DRAFT_BYTES {
        return Err(AppError::Io("草稿超过 20 MiB，拒绝写入恢复日志".into()));
    }

    let stored = StoredDraft {
        project_root: ctx.root.display().to_string(),
        draft: DraftDocument {
            post_id: post_id.to_owned(),
            raw_frontmatter,
            body,
            base_revision,
            saved_at_ms: now_ms(),
        },
    };
    let mut bytes = serde_json::to_vec(&stored)
        .map_err(|error| AppError::Io(format!("草稿序列化失败：{error}")))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_DRAFT_BYTES {
        return Err(AppError::Io("草稿恢复日志超过 20 MiB，拒绝写入".into()));
    }

    let path = draft_path(app_data_dir, ctx, post_id);
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("草稿路径没有父目录".into()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| AppError::Io(error.to_string()))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn read(
    app_data_dir: &Path,
    ctx: &ProjectContext,
    post_id: &str,
) -> Result<Option<DraftDocument>, AppError> {
    let _ = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    let path = draft_path(app_data_dir, ctx, post_id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > MAX_DRAFT_BYTES {
        return Err(AppError::Io(format!(
            "草稿恢复日志异常过大：{}",
            path.display()
        )));
    }
    let stored: StoredDraft = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::Io(format!("草稿恢复日志损坏：{error}")))?;
    if stored.project_root != ctx.root.display().to_string() || stored.draft.post_id != post_id {
        return Err(AppError::Io("草稿恢复日志身份不匹配".into()));
    }
    Ok(Some(stored.draft))
}

pub fn delete(app_data_dir: &Path, ctx: &ProjectContext, post_id: &str) -> Result<(), AppError> {
    let _ = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    let path = draft_path(app_data_dir, ctx, post_id);
    match fs::remove_file(&path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectConfig;

    fn context() -> (tempfile::TempDir, tempfile::TempDir, ProjectContext) {
        let project = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let content_root = project.path().canonicalize().unwrap();
        fs::write(content_root.join("a.md"), "disk\n").unwrap();
        let ctx = ProjectContext {
            root: content_root.clone(),
            config_path: content_root.join(".cloudstack.json"),
            content_root,
            config: ProjectConfig::default(),
        };
        (project, app_data, ctx)
    }

    #[test]
    fn draft_roundtrip_and_delete_are_project_scoped() {
        let (_project, app_data, ctx) = context();
        write(
            app_data.path(),
            &ctx,
            "a.md",
            Some("title: Draft\n".into()),
            "unsaved body\n".into(),
            "revision-1".into(),
        )
        .unwrap();

        let restored = read(app_data.path(), &ctx, "a.md").unwrap().unwrap();
        assert_eq!(restored.post_id, "a.md");
        assert_eq!(restored.body, "unsaved body\n");
        assert_eq!(restored.base_revision, "revision-1");
        assert!(restored.saved_at_ms > 0);

        let mode = fs::metadata(draft_path(app_data.path(), &ctx, "a.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        delete(app_data.path(), &ctx, "a.md").unwrap();
        assert!(read(app_data.path(), &ctx, "a.md").unwrap().is_none());
    }

    #[test]
    fn missing_article_cannot_create_a_draft() {
        let (_project, app_data, ctx) = context();
        assert!(matches!(
            write(
                app_data.path(),
                &ctx,
                "missing.md",
                None,
                "body".into(),
                "revision".into(),
            ),
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn oversized_draft_is_rejected_without_creating_a_log() {
        let (_project, app_data, ctx) = context();
        let body = "x".repeat(MAX_DRAFT_BYTES + 1);

        assert!(write(app_data.path(), &ctx, "a.md", None, body, "revision".into(),).is_err());
        assert!(!draft_path(app_data.path(), &ctx, "a.md").exists());
    }
}
