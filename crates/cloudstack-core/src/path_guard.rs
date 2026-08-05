use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

/// 把前端传来的 PostId 解析为 content root 下的绝对路径。
///
/// 规则：必须是相对路径、不含 `..`/`.` 组件、扩展名在白名单内；
/// 已存在的路径 canonicalize 后必须仍在 content root 内（防符号链接逃逸），
/// 尚不存在的路径则校验其最深已存在祖先目录——剩余组件都是 Normal，无法再逃逸。
///
/// 前提：content_root 已经过 canonicalize（services::project::open_project 保证）。
pub fn resolve_post_path(
    content_root: &Path,
    extensions: &[String],
    id: &str,
) -> Result<PathBuf, AppError> {
    let invalid = || AppError::InvalidPostId(id.to_owned());

    if id.is_empty() || id.contains('\\') {
        return Err(invalid());
    }
    let rel = Path::new(id);
    if rel.is_absolute() {
        return Err(invalid());
    }
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(invalid());
    }
    let ext = rel.extension().and_then(|e| e.to_str());
    let ext_ok = ext.is_some_and(|ext| {
        extensions
            .iter()
            .any(|allowed| allowed.strip_prefix('.').unwrap_or(allowed) == ext)
    });
    if !ext_ok {
        return Err(invalid());
    }

    let full = content_root.join(rel);
    if full.exists() {
        let canon = full.canonicalize()?;
        if !canon.starts_with(content_root) {
            return Err(invalid());
        }
    } else {
        let mut ancestor = full.parent().ok_or_else(invalid)?;
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or_else(invalid)?;
        }
        let canon = ancestor.canonicalize()?;
        if !canon.starts_with(content_root) {
            return Err(invalid());
        }
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md_only() -> Vec<String> {
        vec![".md".into()]
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let canon = dir.path().canonicalize().unwrap();
        (dir, canon)
    }

    #[test]
    fn accepts_plain_and_nested_ids() {
        let (_dir, root) = root();
        assert!(resolve_post_path(&root, &md_only(), "hello.md").is_ok());
        // 嵌套目录尚不存在也应通过（用于新建）
        assert!(resolve_post_path(&root, &md_only(), "2026/aug/post.md").is_ok());
    }

    #[test]
    fn rejects_traversal_absolute_and_wrong_extension() {
        let (_dir, root) = root();
        for id in [
            "../escape.md",
            "a/../../escape.md",
            "/etc/passwd.md",
            "./hello.md",
            "notes.txt",
            "noext",
            "",
            "a\\b.md",
        ] {
            assert!(
                matches!(
                    resolve_post_path(&root, &md_only(), id),
                    Err(AppError::InvalidPostId(_))
                ),
                "应当拒绝：{id:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let (_dir, root) = root();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.md"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();

        assert!(matches!(
            resolve_post_path(&root, &md_only(), "link/secret.md"),
            Err(AppError::InvalidPostId(_))
        ));
        // 链接目录下的新文件同样拒绝（最深已存在祖先是链接本身）
        assert!(matches!(
            resolve_post_path(&root, &md_only(), "link/new.md"),
            Err(AppError::InvalidPostId(_))
        ));
    }
}
