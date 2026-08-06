//! 崩溃安全的操作日志（目前只覆盖 `posts::rename_post`）。
//!
//! 单个 `fs::rename` 是原子的，但"移动图片 → 移动文章 → 重写正文图片路径"这一
//! 整套动作不是。正常的 in-process 失败（比如某次 rename 权限不够）已经由
//! `rename_post` 自己做 best-effort 回滚处理；这里要兜的是更狠的情况——应用
//! 或者操作系统在文件移动到一半时被杀掉，回滚代码根本没有机会运行。
//!
//! 做法是在开始移动任何文件之前，把完整操作意图（谁改名成谁、哪些图片要跟着
//! 搬）写进一个 fsync 过的 journal 文件。下次打开同一个项目时扫描这个目录，
//! 把还没删除的 journal 代表的操作"继续做完"（forward-only，不做回滚）：
//!
//! * 图片移动、文章重命名两步都可以只看当前文件系统状态就判断"做没做过"，
//!   幂等地补上没做的部分；
//! * 正文重写复用 `rewrite_colocated_image_paths`，它本身对已经重写过的内容
//!   是幂等的（只找旧目录名前缀，重写后就再也找不到）。
//!
//! 之所以选择"继续做完"而不是"回滚"：回滚需要能撤销已经发生的文件系统改动，
//! 但如果连正常回滚都会失败（`rename_post` 里已经有这种情况——见
//! `finish_after_rollback`），崩溃恢复场景下更不能假设回滚一定能成功。把所有
//! journal 都导向"完成新状态"这一个方向，不需要区分"回滚式 journal"和"完成式
//! journal"两种语义，恢复逻辑更简单也更容易验证。

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::model::ProjectContext;
use crate::path_guard::resolve_post_path;
use crate::services::assets::asset_dir_for_post;
use crate::services::posts;

const MAX_OPERATION_JOURNAL_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RenameOperation {
    project_root: PathBuf,
    old_id: String,
    new_id: String,
    asset_moves: Vec<(PathBuf, PathBuf)>,
}

/// 一次成功恢复的重命名操作，供调用方（GTK 层）向用户展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredRename {
    pub old_id: String,
    pub new_id: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn operations_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("operations")
}

fn journal_file_name(operation: &RenameOperation) -> String {
    let mut hasher = Sha256::new();
    hasher.update(operation.project_root.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(operation.old_id.as_bytes());
    hasher.update([0]);
    hasher.update(operation.new_id.as_bytes());
    hasher.update([0]);
    hasher.update(now_ms().to_le_bytes());
    let key = format!("{:x}", hasher.finalize());
    format!("rename-{key}.json")
}

/// 崩溃安全窗口的起点：在调用方开始移动任何文件之前，把完整操作意图落盘并
/// fsync（同目录临时文件 + `persist` + 同步父目录，跟 `settings`/`drafts` 一样
/// 的套路）。返回落盘后的路径；调用方在操作确定落地到一致状态后必须调用
/// [`remove_rename_journal`] 删除它。
pub(crate) fn write_rename_journal(
    app_data_dir: &Path,
    ctx: &ProjectContext,
    old_id: &str,
    new_id: &str,
    asset_moves: &[(PathBuf, PathBuf)],
) -> Result<PathBuf, AppError> {
    let operation = RenameOperation {
        project_root: ctx.root.clone(),
        old_id: old_id.to_owned(),
        new_id: new_id.to_owned(),
        asset_moves: asset_moves.to_vec(),
    };
    let dir = operations_dir(app_data_dir);
    fs::create_dir_all(&dir)?;
    let mut bytes = serde_json::to_vec(&operation)
        .map_err(|error| AppError::Io(format!("操作日志序列化失败：{error}")))?;
    bytes.push(b'\n');

    let path = dir.join(journal_file_name(&operation));
    let mut temporary = tempfile::NamedTempFile::new_in(&dir)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| AppError::Io(error.to_string()))?;
    fs::File::open(&dir)?.sync_all()?;
    Ok(path)
}

/// 操作已经确定落地到一致状态（成功完成，或者进程仍存活时的 best-effort 回滚
/// 完全成功）才应该调用。文件已经不存在也当作成功——幂等，不是错误。
pub(crate) fn remove_rename_journal(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::error!("删除重命名操作日志失败（{}）：{error}", path.display());
        }
    }
}

fn quarantine_journal(path: &Path, reason: &str) {
    let target = path.with_extension(format!("json.corrupt-{}", now_ms()));
    match fs::rename(path, &target) {
        Ok(()) => log::warn!(
            "重命名操作日志已损坏（{reason}），已备份到 {}",
            target.display()
        ),
        Err(error) => log::warn!("重命名操作日志已损坏（{reason}），备份失败：{error}"),
    }
}

fn read_rename_journal(path: &Path) -> Result<RenameOperation, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_OPERATION_JOURNAL_BYTES {
        return Err("操作日志文件异常过大".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// 幂等的"继续完成"：每一步都先看当前文件系统状态判断"做没做过"，只补没做的
/// 部分，可以在任意中间态上安全地重复调用。
fn apply_rename_recovery(
    ctx: &ProjectContext,
    operation: &RenameOperation,
) -> Result<(), AppError> {
    for (source, target) in &operation.asset_moves {
        if target.exists() {
            continue; // 已经搬过
        }
        if !source.exists() {
            // 两边都不存在：这一步没法恢复了（比如用户在崩溃后手动清理过），
            // 交给后面的正文重写按当前实际的文件系统状态处理，不中止整个恢复。
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(source, target)?;
    }

    let old_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, &operation.old_id)?;
    let new_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, &operation.new_id)?;
    if old_path.exists() && !new_path.exists() {
        if let Some(parent) = new_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&old_path, &new_path)?;
    }

    if new_path.is_file() {
        posts::reapply_colocated_image_rewrite(&new_path, &operation.old_id, &operation.new_id)?;
    }

    if let Ok(old_asset_dir) = asset_dir_for_post(ctx, &operation.old_id) {
        posts::remove_dir_if_empty(&old_asset_dir);
    }

    Ok(())
}

/// 打开项目时调用：扫描属于这个项目 root 的未完成重命名操作日志，逐个恢复，
/// 成功后删除对应 journal。解析失败的 journal 视为损坏，隔离后跳过，不阻塞
/// 项目打开；恢复本身失败（比如目标路径这次又被别的东西占用了）保留 journal，
/// 记日志，下次打开项目时还会再试一次。不属于这个项目 root 的 journal 原样
/// 保留，留给它自己的项目下次打开时处理。
pub fn recover_pending_renames(app_data_dir: &Path, ctx: &ProjectContext) -> Vec<RecoveredRename> {
    let dir = operations_dir(app_data_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut recovered = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let operation = match read_rename_journal(&path) {
            Ok(operation) => operation,
            Err(reason) => {
                quarantine_journal(&path, &reason);
                continue;
            }
        };
        if operation.project_root != ctx.root {
            continue;
        }
        match apply_rename_recovery(ctx, &operation) {
            Ok(()) => {
                remove_rename_journal(&path);
                recovered.push(RecoveredRename {
                    old_id: operation.old_id,
                    new_id: operation.new_id,
                });
            }
            Err(error) => log::error!(
                "恢复重命名操作失败（{} → {}）：{error}；journal 保留，下次打开项目时重试",
                operation.old_id,
                operation.new_id
            ),
        }
    }
    recovered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectConfig;
    use std::fs;

    fn ctx_in(root: &Path) -> ProjectContext {
        ProjectContext {
            root: root.to_path_buf(),
            content_root: root.join("src/content/blog"),
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig {
                content_dir: "src/content/blog".into(),
                ..ProjectConfig::default()
            },
        }
    }

    fn make_post(ctx: &ProjectContext, id: &str, body: &str) {
        if let Some(parent) = ctx.content_root.join(id).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(ctx.content_root.join(id), body).unwrap();
    }

    #[test]
    fn write_then_recover_completes_when_nothing_was_moved_yet() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(
            &ctx,
            "hello.md",
            "---\ntitle: x\n---\n![](hello/cover.png)\n",
        );
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        fs::write(ctx.content_root.join("hello/cover.png"), b"png-bytes").unwrap();

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();
        assert!(journal.is_file());

        // 模拟进程在 journal 落盘后、还没来得及移动任何文件时就被杀掉：
        // 文章和图片都还在原来的位置。
        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert_eq!(
            recovered,
            vec![RecoveredRename {
                old_id: "hello.md".into(),
                new_id: "world.md".into(),
            }]
        );
        assert!(!journal.exists(), "恢复成功后 journal 应该被删除");
        assert!(ctx.content_root.join("world.md").is_file());
        assert!(!ctx.content_root.join("hello.md").exists());
        assert!(ctx.content_root.join("world/cover.png").is_file());
        assert!(!ctx.content_root.join("hello").exists());
        let text = fs::read_to_string(ctx.content_root.join("world.md")).unwrap();
        assert!(text.contains("world/cover.png"), "正文应该已经重写：{text}");
    }

    #[test]
    fn recover_finishes_when_assets_moved_but_post_was_not() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(
            &ctx,
            "hello.md",
            "---\ntitle: x\n---\n![](hello/cover.png)\n",
        );

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        // 模拟"图片已经搬完，文章还没搬"这个中间态。
        fs::create_dir_all(ctx.content_root.join("world")).unwrap();
        fs::write(ctx.content_root.join("world/cover.png"), b"png-bytes").unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert_eq!(recovered.len(), 1);
        assert!(!journal.exists());
        assert!(ctx.content_root.join("world.md").is_file());
        assert!(ctx.content_root.join("world/cover.png").is_file());
        let text = fs::read_to_string(ctx.content_root.join("world.md")).unwrap();
        assert!(text.contains("world/cover.png"));
    }

    #[test]
    fn recover_finishes_when_post_moved_but_body_not_rewritten() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        // 模拟"图片、文章都已经搬完，但正文还引用着旧目录名"这个中间态。
        fs::create_dir_all(ctx.content_root.join("world")).unwrap();
        fs::write(ctx.content_root.join("world/cover.png"), b"png-bytes").unwrap();
        fs::write(
            ctx.content_root.join("world.md"),
            "---\ntitle: x\n---\n![](hello/cover.png)\n",
        )
        .unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert_eq!(recovered.len(), 1);
        assert!(!journal.exists());
        let text = fs::read_to_string(ctx.content_root.join("world.md")).unwrap();
        assert!(text.contains("world/cover.png"), "正文应该被补写：{text}");
        assert!(!text.contains("hello/cover.png"));
    }

    #[test]
    fn recover_is_a_noop_when_everything_already_finished() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        // 模拟"全部完成，只是最后删 journal 那一步没跑到"。
        fs::create_dir_all(ctx.content_root.join("world")).unwrap();
        fs::write(ctx.content_root.join("world/cover.png"), b"png-bytes").unwrap();
        fs::write(
            ctx.content_root.join("world.md"),
            "---\ntitle: x\n---\n![](world/cover.png)\n",
        )
        .unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert_eq!(recovered.len(), 1);
        assert!(!journal.exists());
        let text = fs::read_to_string(ctx.content_root.join("world.md")).unwrap();
        assert_eq!(text, "---\ntitle: x\n---\n![](world/cover.png)\n");
    }

    #[test]
    fn recover_ignores_journals_belonging_to_a_different_project() {
        let project_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let other_root = other_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        let other_ctx = ctx_in(&other_root);
        make_post(&other_ctx, "hello.md", "---\ntitle: x\n---\nbody\n");

        let journal =
            write_rename_journal(app_data.path(), &other_ctx, "hello.md", "world.md", &[]).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(journal.is_file(), "不属于当前项目的 journal 不应该被处理");
    }

    #[test]
    fn recover_quarantines_a_corrupted_journal_without_blocking_others() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(&ctx, "a.md", "---\ntitle: a\n---\nbody\n");
        make_post(&ctx, "b.md", "---\ntitle: b\n---\nbody\n");

        let dir = operations_dir(app_data.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("rename-broken.json"), b"not json").unwrap();
        write_rename_journal(app_data.path(), &ctx, "b.md", "renamed-b.md", &[]).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].old_id, "b.md");
        assert!(!dir.join("rename-broken.json").exists());
        let quarantined = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("rename-broken.json.corrupt-")
            });
        assert!(quarantined, "损坏的 journal 应该被改名隔离");
    }
}
