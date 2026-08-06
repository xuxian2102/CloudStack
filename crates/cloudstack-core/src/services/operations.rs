//! 崩溃安全的操作日志（目前只覆盖 `posts::rename_post`）。
//!
//! 单个 `fs::rename` 是原子的，但"移动图片 → 移动文章 → 重写正文图片路径"这一
//! 整套动作不是。正常的 in-process 失败（比如某次 rename 权限不够）已经由
//! `rename_post` 自己做 best-effort 回滚处理；这里要兜的是更狠的情况——应用
//! 进程在文件移动到一半时被杀掉（`kill -9`、崩溃、用户在系统层面强制结束），
//! 回滚代码根本没有机会运行。
//!
//! 做法是在开始移动任何文件之前，把完整操作意图（谁改名成谁、哪些图片要跟着
//! 搬）写进一个 fsync 过的 journal 文件。下次打开同一个项目时扫描这个目录，
//! 把还没删除的 journal 代表的操作"继续做完"（forward-only，不做回滚）：
//!
//! * 图片移动、文章重命名两步都只看当前文件系统状态判断"做没做过"，幂等地
//!   补上没做的部分——但只有明确是"还没做"（source 是普通文件、target 不存在）
//!   或"已经做完"（source 不存在、target 是普通文件）这两种状态才继续；其余
//!   状态（双方都存在、双方都不存在、类型不是普通文件/是符号链接）一律整体
//!   中止本次恢复、保留 journal，不去猜测哪一种是"正确"的现实状态；
//!   （用 `symlink_metadata` 而不是 `exists()`，避免符号链接、断链符号链接被
//!   误判成普通文件。）
//! * 正文重写复用 `rewrite_colocated_image_paths`，它本身对已经重写过的内容
//!   是幂等的（只找旧目录名前缀，重写后就再也找不到）。
//!
//! journal 只记录 `old_id`/`new_id`/图片文件名，不记录绝对路径——恢复时用
//! 当前的 `ProjectContext` 重新推导 asset 目录（重新走一遍 `resolve_post_path`
//! 里的路径守卫），把 journal 当成不可信的持久化输入：文件损坏、跨版本升级、
//! 用户手动编辑，都不应该让恢复逻辑直接对着 journal 里存的任意路径操作。
//! `asset_dir_for_post` 本身只做路径拼接、不校验结果，所以恢复逻辑用
//! `validate_asset_directory` 在使用它的返回值之前再补一层校验——防止资产
//! 目录这一级本身在崩溃后被换成指向项目外的符号链接，把图片移动到项目边界
//! 之外。
//!
//! 之所以选择"继续做完"而不是"回滚"：回滚需要能撤销已经发生的文件系统改动，
//! 但如果连正常回滚都会失败（`rename_post` 里已经有这种情况——见
//! `finish_after_rollback`），崩溃恢复场景下更不能假设回滚一定能成功。把所有
//! journal 都导向"完成新状态"这一个方向，不需要区分"回滚式 journal"和"完成式
//! journal"两种语义，恢复逻辑更简单也更容易验证。
//!
//! **范围说明**：这里保证的是"应用进程被杀掉"这一档的崩溃安全——journal 自身
//! 的写入是 fsync 过的原子替换，但 `fs::rename` 之后没有对受影响的目录逐一
//! `fsync`，journal 删除后也没有再同步 `operations/` 目录本身。真正做到内核
//! 崩溃/断电也不丢状态（power-loss safety）还需要在每次目录项变更后都跟一次
//! 目录 fsync，这里暂时没有做——对桌面博客编辑器而言，进程被杀比内核崩溃/断电
//! 常见得多，这个取舍是有意的，不是遗漏。

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::model::ProjectContext;
use crate::path_guard::resolve_post_path;
use crate::services::assets::asset_dir_for_post;
use crate::services::posts;

const MAX_OPERATION_JOURNAL_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RenameOperation {
    project_root: PathBuf,
    old_id: String,
    new_id: String,
    /// 图片的文件名，不是路径——恢复时在当前 `ctx` 下重新推导所在目录。
    asset_names: Vec<String>,
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
    // 只存文件名，不存 source/target 的绝对路径；两者的文件名本来就相同
    // （`plan_colocated_asset_moves` 只换目录、不改名）。
    let asset_names = asset_moves
        .iter()
        .map(|(source, _)| {
            source
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| {
                    AppError::Io(format!("图片路径没有合法文件名：{}", source.display()))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let operation = RenameOperation {
        project_root: ctx.root.clone(),
        old_id: old_id.to_owned(),
        new_id: new_id.to_owned(),
        asset_names,
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
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut limited = file.take(MAX_OPERATION_JOURNAL_BYTES + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_OPERATION_JOURNAL_BYTES {
        return Err("操作日志文件异常过大".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveRecoveryState {
    /// source 是普通文件、target 不存在：这一步还没做，可以安全地继续。
    Pending,
    /// source 不存在、target 是普通文件：这一步已经做完，跳过。
    Completed,
}

/// `NotFound` 才是"不存在"；权限错误等其他 IO 错误必须原样上抛，不能被静默
/// 当成"不存在"参与状态判断（那会让一个本来该中止的恢复被错误地继续下去）。
fn metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>, AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// 用 `symlink_metadata` 而不是 `exists()`，避免符号链接/断链符号链接被误判成
/// 普通文件。只有两种状态可以安全判断该怎么做；其余一律报错——双方都存在是
/// 冲突（不知道该信哪一个），双方都不存在是缺失（这一步的输入凭空消失了），
/// 类型不对（目录/符号链接）说明有别的东西占用了这个位置，都不该被静默当成
/// "已经完成"。`kind` 只用来让错误信息说清楚是图片还是文章路径出的问题。
fn classify_move_state(
    source: &Path,
    target: &Path,
    kind: &str,
) -> Result<MoveRecoveryState, AppError> {
    let source_meta = metadata_if_exists(source)?;
    let target_meta = metadata_if_exists(target)?;
    let is_plain_file = |meta: &Option<fs::Metadata>| {
        meta.as_ref()
            .is_some_and(|m| m.is_file() && !m.file_type().is_symlink())
    };

    match (source_meta.is_some(), target_meta.is_some()) {
        (true, false) if is_plain_file(&source_meta) => Ok(MoveRecoveryState::Pending),
        (false, true) if is_plain_file(&target_meta) => Ok(MoveRecoveryState::Completed),
        _ => Err(AppError::Io(format!(
            "重命名恢复中止：{kind}路径处于无法安全判断的状态（{} / {}），已保留操作日志待下次重试",
            source.display(),
            target.display()
        ))),
    }
}

/// 校验资产目录本身没有通过符号链接逃出 `content_root`——跟 `path_guard::
/// resolve_post_path` 对文章路径的校验是同一个思路，只是这里校验的是目录：
/// 已存在必须是非符号链接的普通目录、canonical 路径在 content_root 下；
/// 不存在则向上找最深已存在祖先做同样的校验，防止后续 `create_dir_all` 顺着
/// 祖先符号链接在项目外建目录。`asset_dir_for_post` 本身只做路径拼接，不做
/// 这层校验，所以恢复逻辑必须在使用它的返回值之前自己补上。
fn validate_asset_directory(ctx: &ProjectContext, dir: &Path) -> Result<(), AppError> {
    let invalid = || AppError::Io(format!("资产目录路径不安全，已中止恢复：{}", dir.display()));
    match metadata_if_exists(dir)? {
        Some(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(invalid());
            }
            if !dir.canonicalize()?.starts_with(&ctx.content_root) {
                return Err(invalid());
            }
        }
        None => {
            let mut ancestor = dir.parent().ok_or_else(invalid)?;
            while !ancestor.exists() {
                ancestor = ancestor.parent().ok_or_else(invalid)?;
            }
            if !ancestor.canonicalize()?.starts_with(&ctx.content_root) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn validate_bare_file_name(name: &str) -> Result<(), AppError> {
    let is_bare =
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']);
    if is_bare {
        Ok(())
    } else {
        Err(AppError::Io(format!(
            "操作日志包含非法的图片文件名：{name:?}"
        )))
    }
}

/// 幂等的"继续完成"：先对 journal 里记录的每一步做只读分类，任何一步处于
/// 无法安全判断的状态就整体中止、不执行任何移动——journal 代表的是单个原子
/// 操作，不能移动了一半才发现后面的路径有问题。分类全部通过后才真正执行还
/// 没做的部分。资产目录本身也是当场用当前 `ctx` 重新推导（重新走一遍
/// `resolve_post_path`/`asset_dir_for_post` 里的路径守卫），不信任 journal
/// 里可能是旧版本或者被篡改过的绝对路径。
///
/// 没有图片意图（`asset_names` 为空）时完全不碰同名 stem 目录——纯文章重命名
/// 不应该因为一个跟这次操作无关的同名目录（哪怕它现在是个符号链接）被挡住。
fn apply_rename_recovery(
    ctx: &ProjectContext,
    operation: &RenameOperation,
) -> Result<(), AppError> {
    let mut asset_moves = Vec::with_capacity(operation.asset_names.len());
    let mut old_asset_dir = None;

    if !operation.asset_names.is_empty() {
        let resolved_old_asset_dir = asset_dir_for_post(ctx, &operation.old_id)?;
        let new_asset_dir = asset_dir_for_post(ctx, &operation.new_id)?;
        validate_asset_directory(ctx, &resolved_old_asset_dir)?;
        validate_asset_directory(ctx, &new_asset_dir)?;

        // 篡改/损坏的 journal 可能包含重复文件名：预扫描阶段两项都会分类成
        // Pending，真正执行时第一项成功、第二项才因为 target 已存在而失败，
        // 变成"校验全部通过后仍然只部分执行"。在这里一次性拒绝，不留这个口子。
        let mut seen = std::collections::BTreeSet::new();
        for name in &operation.asset_names {
            validate_bare_file_name(name)?;
            if !seen.insert(name.as_str()) {
                return Err(AppError::Io(format!(
                    "操作日志包含重复的图片文件名：{name:?}"
                )));
            }
            let source = resolved_old_asset_dir.join(name);
            let target = new_asset_dir.join(name);
            let state = classify_move_state(&source, &target, "图片")?;
            asset_moves.push((source, target, state));
        }
        old_asset_dir = Some(resolved_old_asset_dir);
    }

    let old_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, &operation.old_id)?;
    let new_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, &operation.new_id)?;
    let post_state = classify_move_state(&old_path, &new_path, "文章")?;

    for (source, target, state) in &asset_moves {
        if *state == MoveRecoveryState::Pending {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(source, target)?;
        }
    }
    if post_state == MoveRecoveryState::Pending {
        if let Some(parent) = new_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&old_path, &new_path)?;
    }

    // 走到这里，两种分类结果都已经确保 new_path 现在是普通文件。
    posts::reapply_colocated_image_rewrite(&new_path, &operation.old_id, &operation.new_id)?;
    if let Some(old_asset_dir) = old_asset_dir {
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

    #[test]
    fn recover_aborts_and_keeps_journal_when_an_asset_path_conflicts() {
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
        fs::write(ctx.content_root.join("hello/cover.png"), b"hello bytes").unwrap();

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        // 矛盾状态：source 和 target 同时存在（比如崩溃后用户手动整理过文件）。
        fs::create_dir_all(ctx.content_root.join("world")).unwrap();
        fs::write(ctx.content_root.join("world/cover.png"), b"world bytes").unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty(), "冲突状态不应该被当成恢复成功");
        assert!(journal.is_file(), "冲突状态必须保留 journal，不能删掉");
        assert_eq!(
            fs::read(ctx.content_root.join("hello/cover.png")).unwrap(),
            b"hello bytes",
            "冲突状态下不应该动 source"
        );
        assert_eq!(
            fs::read(ctx.content_root.join("world/cover.png")).unwrap(),
            b"world bytes",
            "冲突状态下不应该动 target"
        );
    }

    #[test]
    fn recover_aborts_and_keeps_journal_when_an_asset_path_is_missing_on_both_sides() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(
            &ctx,
            "hello.md",
            "---\ntitle: x\n---\n![](hello/cover.png)\n",
        );
        // hello/cover.png 从未被创建过：source、target 两边都不存在。

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(journal.is_file());
        assert!(
            ctx.content_root.join("hello.md").is_file(),
            "无法安全判断资产状态时，文章也不应该被移动"
        );
        assert!(!ctx.content_root.join("world.md").exists());
    }

    #[test]
    fn recover_aborts_and_keeps_journal_when_post_paths_conflict() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(&ctx, "hello.md", "---\ntitle: x\n---\nbody\n");
        fs::write(
            ctx.content_root.join("world.md"),
            "---\ntitle: y\n---\nsomeone else's content\n",
        )
        .unwrap();

        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &[]).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(journal.is_file());
        assert_eq!(
            fs::read_to_string(ctx.content_root.join("world.md")).unwrap(),
            "---\ntitle: y\n---\nsomeone else's content\n",
            "冲突状态下不应该改写疑似冲突的目标文件"
        );
    }

    #[test]
    fn recover_aborts_and_keeps_journal_when_target_is_not_a_plain_file() {
        let project_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        // hello.md 从未创建过（模拟"文章已经移动过"的表象）；world.md 被占用成了
        // 目录而不是普通文件。
        fs::create_dir_all(ctx.content_root.join("world.md")).unwrap();

        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &[]).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(journal.is_file());
    }

    #[test]
    fn recover_rejects_symlinked_new_asset_directory() {
        let project_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(
            &ctx,
            "hello.md",
            "---\ntitle: x\n---\n![](hello/cover.png)\n",
        );
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        fs::write(ctx.content_root.join("hello/cover.png"), b"secret").unwrap();

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        // "world" 资产目录被替换成了指向项目外的符号链接。
        let outside = outside_dir.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(&outside, ctx.content_root.join("world")).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(
            journal.is_file(),
            "符号链接资产目录必须中止恢复、保留 journal"
        );
        assert!(
            !outside.join("cover.png").exists(),
            "不应该把图片移出项目边界"
        );
        assert!(
            ctx.content_root.join("hello/cover.png").is_file(),
            "source 图片不应该被移动"
        );
    }

    #[test]
    fn recover_rejects_symlinked_old_asset_directory() {
        let project_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(&ctx, "hello.md", "---\ntitle: x\n---\nbody\n");

        let asset_moves = vec![(
            ctx.content_root.join("hello/cover.png"),
            ctx.content_root.join("world/cover.png"),
        )];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        // "hello" 资产目录被替换成了指向项目外的符号链接，外面藏着一个同名文件。
        let outside = outside_dir.path().canonicalize().unwrap();
        fs::write(outside.join("cover.png"), b"attacker file").unwrap();
        std::os::unix::fs::symlink(&outside, ctx.content_root.join("hello")).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(journal.is_file());
        assert!(
            outside.join("cover.png").is_file(),
            "项目外的文件不应该被移动"
        );
        assert!(!ctx.content_root.join("world/cover.png").exists());
    }

    #[test]
    fn recover_post_only_ignores_unrelated_asset_directory() {
        let project_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let ctx = ctx_in(&root);
        make_post(
            &ctx,
            "hello.md",
            "---\ntitle: x\n---\nbody without any image\n",
        );

        // "hello" 目录（跟 hello.md 同名 stem）被换成了一个跟这次重命名完全
        // 无关的符号链接——这次操作根本没有图片意图（journal 里 asset_names
        // 是空的），不应该因为这个不相关的目录被挡住。
        let outside = outside_dir.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(&outside, ctx.content_root.join("hello")).unwrap();

        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &[]).unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert_eq!(
            recovered,
            vec![RecoveredRename {
                old_id: "hello.md".into(),
                new_id: "world.md".into(),
            }],
            "没有资产意图时，不相关的同名符号链接目录不应该阻塞纯文章恢复"
        );
        assert!(!journal.exists());
        assert!(ctx.content_root.join("world.md").is_file());
    }

    #[test]
    fn recover_rejects_duplicate_asset_names_before_moving_anything() {
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
        fs::write(ctx.content_root.join("hello/cover.png"), b"first").unwrap();

        // 篡改/损坏的 journal 可能出现两个不同来源目录派生出同一个文件名的
        // 情况（这里用两个不同的 source 目录、相同文件名模拟）。
        let asset_moves = vec![
            (
                ctx.content_root.join("hello/cover.png"),
                ctx.content_root.join("world/cover.png"),
            ),
            (
                ctx.content_root.join("elsewhere/cover.png"),
                ctx.content_root.join("world/cover.png"),
            ),
        ];
        let journal =
            write_rename_journal(app_data.path(), &ctx, "hello.md", "world.md", &asset_moves)
                .unwrap();

        let recovered = recover_pending_renames(app_data.path(), &ctx);
        assert!(recovered.is_empty());
        assert!(
            journal.is_file(),
            "重复文件名必须在移动任何文件之前就整体拒绝"
        );
        assert!(
            ctx.content_root.join("hello/cover.png").is_file(),
            "校验阶段就该失败，不应该移动任何文件"
        );
        assert!(!ctx.content_root.join("world/cover.png").exists());
    }
}
