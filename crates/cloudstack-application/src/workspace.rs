//! 打开一个 workspace 该怎样协调：解析项目配置、恢复崩溃遗留的重命名、
//! 列出文章，或者转成"需要初始化"/"需要修复内容目录"的引导流程。不碰
//! GTK/glib——应用数据目录的发现（依赖 `glib::user_data_dir()`）由调用方在
//! 主线程算好，作为普通 `&Path` 传进来；GTK 之后再决定用哪个线程执行、
//! 怎样把 outcome 安装进 `EditorState` 和界面。

use std::path::{Path, PathBuf};

use cloudstack_core::model::{PostSummary, ProjectContext};
use cloudstack_core::services::operations::RecoveredRename;
use cloudstack_core::services::{git, operations, posts, project};
use cloudstack_core::AppError;

#[derive(Debug)]
pub enum OpenWorkspaceOutcome {
    Opened {
        context: ProjectContext,
        post_summaries: Vec<PostSummary>,
        recovered_renames: Vec<RecoveredRename>,
    },
    NeedsInitialization {
        root: PathBuf,
        suggested_content_dir: String,
    },
    NeedsContentRepair {
        root: PathBuf,
        content_dir: String,
    },
}

/// 打开顺序固定为 `open_project → ensure_local_config_excluded →
/// recover_pending_renames → list_posts`：
/// - 必须先恢复重命名再列文章——上次意外退出可能留下半完成的重命名，不先
///   续完，文章列表可能同时看到消失的旧 id 和还没出现的新 id；
/// - local-exclude 写入失败是 best-effort（`let _ =`），不阻止打开一个本来
///   有效的项目；
/// - 缺配置/缺内容目录不是 fatal 错误，转成对应的引导 outcome；其余错误
///   原样上抛。
pub fn open_workspace(root: &Path, app_data_dir: &Path) -> Result<OpenWorkspaceOutcome, AppError> {
    let root = root.to_path_buf();
    match project::open_project(&root) {
        Ok(context) => {
            let _ = git::ensure_local_config_excluded(&context);
            let recovered_renames = operations::recover_pending_renames(app_data_dir, &context);
            let post_summaries = posts::list_posts(&context)?;
            Ok(OpenWorkspaceOutcome::Opened {
                context,
                post_summaries,
                recovered_renames,
            })
        }
        Err(AppError::MissingProjectConfig) => {
            let suggested_content_dir = project::suggest_content_dir(&root)?;
            Ok(OpenWorkspaceOutcome::NeedsInitialization {
                root,
                suggested_content_dir,
            })
        }
        Err(AppError::MissingContentDirectory(content_dir)) => {
            Ok(OpenWorkspaceOutcome::NeedsContentRepair { root, content_dir })
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn app_data() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_config_requests_initialization_with_existing_content_suggestion() {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir_all(project.path().join("content")).unwrap();

        let outcome = open_workspace(project.path(), app_data().path()).unwrap();

        match outcome {
            OpenWorkspaceOutcome::NeedsInitialization {
                suggested_content_dir,
                ..
            } => assert_eq!(suggested_content_dir, "content"),
            other => panic!("expected NeedsInitialization, got {other:?}"),
        }
    }

    #[test]
    fn missing_content_directory_requests_repair() {
        let project = tempfile::tempdir().unwrap();
        project::initialize_project(project.path(), "notes", false).unwrap();
        fs::remove_dir_all(project.path().join("notes")).unwrap();

        let outcome = open_workspace(project.path(), app_data().path()).unwrap();

        match outcome {
            OpenWorkspaceOutcome::NeedsContentRepair { content_dir, .. } => {
                assert_eq!(content_dir, "notes");
            }
            other => panic!("expected NeedsContentRepair, got {other:?}"),
        }
    }

    #[test]
    fn opening_workspace_lists_posts() {
        let project = tempfile::tempdir().unwrap();
        project::initialize_project(project.path(), "notes", false).unwrap();
        let notes = project.path().join("notes");
        fs::write(notes.join("a.md"), "a\n").unwrap();
        fs::create_dir_all(notes.join("nested")).unwrap();
        fs::write(notes.join("nested/b.md"), "b\n").unwrap();

        let outcome = open_workspace(project.path(), app_data().path()).unwrap();

        match outcome {
            OpenWorkspaceOutcome::Opened {
                context,
                post_summaries,
                recovered_renames,
            } => {
                assert_eq!(context.root, project.path().canonicalize().unwrap());
                let ids = post_summaries
                    .iter()
                    .map(|post| post.id.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(ids, vec!["a.md", "nested/b.md"]);
                assert!(recovered_renames.is_empty());
            }
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn opening_git_workspace_excludes_cloudstack_config_locally() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        assert!(std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        project::initialize_project(&root, "notes", false).unwrap();

        let outcome = open_workspace(&root, app_data().path()).unwrap();
        assert!(matches!(outcome, OpenWorkspaceOutcome::Opened { .. }));

        let exclude = fs::read_to_string(root.join(".git/info/exclude")).unwrap();
        assert!(exclude
            .lines()
            .any(|line| line.trim() == ".cloudstack.json"));
    }

    #[test]
    fn rename_recovery_runs_before_post_listing() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        project::initialize_project(&root, "notes", false).unwrap();
        fs::write(root.join("notes/old.md"), "old\n").unwrap();

        let app_data = app_data();
        let operations_dir = app_data.path().join("operations");
        fs::create_dir_all(&operations_dir).unwrap();
        let journal = serde_json::json!({
            "project_root": root.to_str().unwrap(),
            "old_id": "old.md",
            "new_id": "new.md",
            "asset_names": [],
        });
        fs::write(
            operations_dir.join("rename-test.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        let outcome = open_workspace(&root, app_data.path()).unwrap();

        match outcome {
            OpenWorkspaceOutcome::Opened {
                post_summaries,
                recovered_renames,
                ..
            } => {
                assert_eq!(
                    recovered_renames,
                    vec![RecoveredRename {
                        old_id: "old.md".into(),
                        new_id: "new.md".into(),
                    }]
                );
                let ids = post_summaries
                    .iter()
                    .map(|post| post.id.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(ids, vec!["new.md"]);
            }
            other => panic!("expected Opened, got {other:?}"),
        }

        assert!(root.join("notes/new.md").exists());
        assert!(!root.join("notes/old.md").exists());
        assert!(fs::read_dir(&operations_dir).unwrap().next().is_none());
    }
}
