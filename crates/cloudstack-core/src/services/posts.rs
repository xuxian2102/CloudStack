use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write as _;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Tag};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::model::{PostDocument, PostSummary, ProjectContext};
use crate::path_guard::resolve_post_path;
use crate::services::assets::asset_dir_for_post;
use crate::services::markdown;
use crate::services::operations;
use crate::text::{decode_text, encode_text, LineEnding, TextFileFormat};

pub fn revision_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 拆分 frontmatter 与正文。返回的 frontmatter 不含 `---` 分隔线并保留自身换行；
/// None 表示没有 frontmatter 块（含分隔线未闭合的情况——宁可整篇当正文也不丢内容）。
/// 对 LF 文件满足 join_markdown(split_markdown(x)) == x（BOM 会被剥离，见测试）。
pub fn split_markdown(text: &str) -> (Option<&str>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---")
        .and_then(|t| t.strip_prefix("\r\n").or_else(|| t.strip_prefix('\n')))
    else {
        return (None, text);
    };

    let mut idx = 0;
    while idx < rest.len() {
        let line_end = rest[idx..]
            .find('\n')
            .map(|p| idx + p + 1)
            .unwrap_or(rest.len());
        let line = rest[idx..line_end].trim_end_matches(['\n', '\r']);
        if line == "---" {
            return (Some(&rest[..idx]), &rest[line_end..]);
        }
        idx = line_end;
    }
    (None, text)
}

pub fn join_markdown(raw_frontmatter: Option<&str>, body: &str) -> String {
    match raw_frontmatter {
        None => body.to_owned(),
        Some(fm) => {
            let mut s = String::with_capacity(fm.len() + body.len() + 10);
            s.push_str("---\n");
            s.push_str(fm);
            if !fm.is_empty() && !fm.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("---\n");
            s.push_str(body);
            s
        }
    }
}

/// 按 frontmatter.fields 里 type=="tags" 的字段名分组建索引（一个项目可能配置不止一个
/// 标签类字段，比如 tags 和 categories，各自的候选值不应该混在一起）。
/// 单篇文章 YAML 损坏时跳过继续，不因为一篇文章拖垮整个索引。
pub fn list_tags(ctx: &ProjectContext) -> Result<HashMap<String, Vec<String>>, AppError> {
    let tag_fields: Vec<&str> = ctx
        .config
        .frontmatter
        .fields
        .iter()
        .filter(|f| f.field_type == "tags")
        .map(|f| f.name.as_str())
        .collect();
    if tag_fields.is_empty() {
        return Ok(HashMap::new());
    }

    let mut sets: HashMap<&str, std::collections::BTreeSet<String>> = tag_fields
        .iter()
        .map(|&f| (f, Default::default()))
        .collect();

    for post in list_posts(ctx)? {
        let Ok(text) = fs::read_to_string(ctx.content_root.join(&post.id)) else {
            continue;
        };
        let Some(fm) = split_markdown(&text).0 else {
            continue;
        };
        let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(fm) else {
            continue;
        };
        for &field in &tag_fields {
            if let Some(serde_yaml::Value::Sequence(seq)) = value.get(field) {
                for item in seq {
                    if let Some(s) = item.as_str() {
                        sets.get_mut(field).unwrap().insert(s.to_string());
                    }
                }
            }
        }
    }

    Ok(sets
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.into_iter().collect()))
        .collect())
}

pub fn list_posts(ctx: &ProjectContext) -> Result<Vec<PostSummary>, AppError> {
    let mut out = Vec::new();
    walk(
        &ctx.content_root,
        &ctx.content_root,
        &ctx.config.extensions,
        &mut out,
    )?;
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn walk(
    root: &Path,
    dir: &Path,
    extensions: &[String],
    out: &mut Vec<PostSummary>,
) -> Result<(), AppError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        // 符号链接可能指向 content root 之外，路径守卫会拒绝打开，列表里干脆不显示
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            walk(root, &path, extensions, out)?;
        } else if has_allowed_extension(&path, extensions) {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| AppError::Io("路径前缀异常".into()))?;
            let id = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let modified_ms = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
            out.push(PostSummary {
                relative_path: id.clone(),
                id,
                modified_ms,
            });
        }
    }
    Ok(())
}

fn has_allowed_extension(path: &Path, extensions: &[String]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    extensions
        .iter()
        .any(|allowed| allowed.strip_prefix('.').unwrap_or(allowed) == ext)
}

pub fn read_post(ctx: &ProjectContext, id: &str) -> Result<PostDocument, AppError> {
    let path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, id)?;
    let bytes = read_existing(&path, id)?;
    // revision 必须先于任何解码/归一化算出来，这样外部只改了 EOL 风格的
    // 修改也能被判定为冲突，而不是被 decode_text 归一化之后悄悄放过。
    let revision = revision_of(&bytes);
    let decoded = decode_text(&bytes)?;
    let (fm, body) = split_markdown(&decoded.text);
    Ok(PostDocument {
        id: id.to_owned(),
        relative_path: id.to_owned(),
        raw_frontmatter: fm.map(str::to_owned),
        body: body.to_owned(),
        revision,
        format: decoded.format,
    })
}

pub struct PostWriteResult {
    pub revision: String,
    pub format: TextFileFormat,
}

pub fn write_post(
    ctx: &ProjectContext,
    id: &str,
    raw_frontmatter: Option<&str>,
    body: &str,
    format: TextFileFormat,
    expected_revision: &str,
) -> Result<PostWriteResult, AppError> {
    let path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, id)?;
    let current = read_existing(&path, id)?;
    if revision_of(&current) != expected_revision {
        return Err(AppError::ExternalModificationConflict);
    }
    let mut content = join_markdown(raw_frontmatter, body);
    // frontmatter-only 且正文为空时，join_markdown 总会在闭合的 "---" 后面
    // 强制补一个换行；这种情况下 body 本身是空字符串，不携带任何信息能区分
    // 磁盘原文到底有没有这个换行，只能靠读盘时观察到的 has_final_newline
    // 兜底纠正。其余情况下末尾有没有换行完全由 body 自身内容决定，见
    // `crate::text` 模块文档。
    if raw_frontmatter.is_some()
        && body.is_empty()
        && !format.has_final_newline
        && content.ends_with('\n')
    {
        content.pop();
    }
    let bytes = encode_text(&content, format.line_ending)?;
    atomic_write_checked(&path, &bytes, Some(expected_revision))?;
    // 保存后的 format 反映实际落盘的字节，而不是调用方传入的旧 format——
    // 比如 Mixed 换行的文件保存一次之后就会变成 Lf。
    let written_format = decode_text(&bytes)?.format;
    Ok(PostWriteResult {
        revision: revision_of(&bytes),
        format: written_format,
    })
}

pub fn create_post(
    ctx: &ProjectContext,
    id: &str,
    raw_frontmatter: Option<&str>,
    body: &str,
) -> Result<PostDocument, AppError> {
    let path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = join_markdown(raw_frontmatter, body);
    let bytes = encode_text(&content, LineEnding::Lf)?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("文章路径没有父目录".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(&path).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            AppError::AlreadyExists(id.to_owned())
        } else {
            AppError::Io(error.error.to_string())
        }
    })?;
    sync_directory(parent)?;
    read_post(ctx, id)
}

pub fn validate_rename(
    ctx: &ProjectContext,
    old_id: &str,
    new_id: &str,
    expected_revision: &str,
) -> Result<(), AppError> {
    let old_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, old_id)?;
    let new_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, new_id)?;
    let original = read_existing(&old_path, old_id)?;
    if revision_of(&original) != expected_revision {
        return Err(AppError::ExternalModificationConflict);
    }
    if new_path.exists() {
        return Err(AppError::AlreadyExists(new_id.to_owned()));
    }

    let text = String::from_utf8(original)
        .map_err(|_| AppError::Io(format!("文件不是 UTF-8 编码：{old_id}")))?;
    let (_, body) = split_markdown(&text);
    let _ = plan_colocated_asset_moves(ctx, old_id, new_id, body)?;
    Ok(())
}

/// 除了 `expected_revision` 之外，重命名在开始移动任何文件之前，会先把完整操作
/// 意图写进 `app_data_dir` 下的 crash-safe journal（见
/// `crate::services::operations`）。这不是给正常的 in-process 失败用的——那种
/// 情况仍然沿用下面的 best-effort 回滚；journal 是给"整个进程在移动文件的过程
/// 中被杀掉"这种情况兜底：下次打开同一个项目时，`operations::recover_pending_renames`
/// 会把任何遗留的半完成操作"继续做完"，不会留下图片已经搬走、文章还没搬这种
/// 永久不一致的中间态。
pub fn rename_post(
    ctx: &ProjectContext,
    old_id: &str,
    new_id: &str,
    expected_revision: &str,
    app_data_dir: &Path,
) -> Result<PostDocument, AppError> {
    validate_rename(ctx, old_id, new_id, expected_revision)?;
    let old_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, old_id)?;
    let new_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, new_id)?;
    let original = read_existing(&old_path, old_id)?;
    if revision_of(&original) != expected_revision {
        return Err(AppError::ExternalModificationConflict);
    }
    if new_path.exists() {
        return Err(AppError::AlreadyExists(new_id.to_owned()));
    }

    let original_text = String::from_utf8(original)
        .map_err(|_| AppError::Io(format!("文件不是 UTF-8 编码：{old_id}")))?;
    let (_, body) = split_markdown(&original_text);
    let rewritten_body = rewrite_colocated_image_paths(body, old_id, new_id)?;
    let rewritten = if rewritten_body == body {
        None
    } else {
        let body_offset = body.as_ptr() as usize - original_text.as_ptr() as usize;
        let mut text = original_text.clone();
        text.replace_range(body_offset.., &rewritten_body);
        Some(text)
    };

    // 只移动正文实际引用、且 canonicalize 后确实是同名目录直接子文件的图片。
    // 同名目录里的 JSON、嵌套文章或其他用户文件从不因为目录名相同而被整体搬走。
    let old_asset_dir = asset_dir_for_post(ctx, old_id)?;
    let new_asset_dir = asset_dir_for_post(ctx, new_id)?;
    let asset_moves = plan_colocated_asset_moves(ctx, old_id, new_id, body)?;

    // journal 落盘失败就整体放弃——此时还没有任何文件被移动，直接返回错误让
    // 调用方重试，不留下任何需要恢复的痕迹。
    let journal_path =
        operations::write_rename_journal(app_data_dir, ctx, old_id, new_id, &asset_moves)?;

    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !asset_moves.is_empty() {
        if let Some(parent) = new_asset_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(&new_asset_dir)?;
    }
    let mut moved_assets: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (source, target) in &asset_moves {
        if let Err(error) = fs::rename(source, target) {
            finish_after_rollback(&journal_path, rollback_asset_moves(&moved_assets));
            remove_dir_if_empty(&new_asset_dir);
            return Err(error.into());
        }
        moved_assets.push((source.clone(), target.clone()));
    }
    if let Err(error) = fs::rename(&old_path, &new_path) {
        finish_after_rollback(&journal_path, rollback_asset_moves(&moved_assets));
        remove_dir_if_empty(&new_asset_dir);
        return Err(error.into());
    }
    if let Some(text) = rewritten {
        if let Err(error) = atomic_write(&new_path, text.as_bytes()) {
            let mut rollback_succeeded = fs::rename(&new_path, &old_path).is_ok();
            rollback_succeeded &= rollback_asset_moves(&moved_assets);
            finish_after_rollback(&journal_path, rollback_succeeded);
            remove_dir_if_empty(&new_asset_dir);
            return Err(error);
        }
    }
    remove_dir_if_empty(&old_asset_dir);
    operations::remove_rename_journal(&journal_path);

    read_post(ctx, new_id)
}

/// 回滚成功就说明磁盘状态已经确定落回旧状态，journal 可以删掉；回滚本身失败
/// （极罕见的二次故障）就必须保留 journal——下次打开项目时，
/// `operations::recover_pending_renames` 会把这个半完成的操作继续做完，而不是
/// 让它永远卡在不一致的中间态。
fn finish_after_rollback(journal_path: &Path, rollback_succeeded: bool) {
    if rollback_succeeded {
        operations::remove_rename_journal(journal_path);
    } else {
        log::error!(
            "重命名回滚未完全成功，保留操作日志待下次打开项目时恢复：{}",
            journal_path.display()
        );
    }
}

/// 尽力把已经移动的图片挪回原位；返回是否全部回滚成功。
#[must_use]
fn rollback_asset_moves(moved: &[(PathBuf, PathBuf)]) -> bool {
    let mut succeeded = true;
    for (source, target) in moved.iter().rev() {
        if let Err(error) = fs::rename(target, source) {
            log::error!(
                "回滚图片移动失败（{} → {}）：{error}",
                target.display(),
                source.display()
            );
            succeeded = false;
        }
    }
    succeeded
}

pub(crate) fn remove_dir_if_empty(path: &Path) {
    if path.is_dir() && fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(path);
    }
}

/// `rename_post` 崩溃恢复用：`post_path` 当前内容里如果还有指向 `old_id` 同名
/// 目录的图片路径就原地原子重写成指向 `new_id`；已经是新路径（或者本来就不需要
/// 重写）时什么也不做。之所以可以在任意时刻对着 `post_path` 当前内容重新跑一遍，
/// 是因为 `rewrite_colocated_image_paths` 只找 `old_dir/` 前缀——内容已经被重写
/// 过之后就再也找不到这个前缀，天然是幂等的。
pub(crate) fn reapply_colocated_image_rewrite(
    post_path: &Path,
    old_id: &str,
    new_id: &str,
) -> Result<(), AppError> {
    let bytes = read_existing(post_path, new_id)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| AppError::Io(format!("文件不是 UTF-8 编码：{new_id}")))?;
    let (_, body) = split_markdown(&text);
    let rewritten_body = rewrite_colocated_image_paths(body, old_id, new_id)?;
    if rewritten_body == body {
        return Ok(());
    }
    let body_offset = body.as_ptr() as usize - text.as_ptr() as usize;
    let mut rewritten_text = text.clone();
    rewritten_text.replace_range(body_offset.., &rewritten_body);
    atomic_write(post_path, rewritten_text.as_bytes())
}

fn plan_colocated_asset_moves(
    ctx: &ProjectContext,
    old_id: &str,
    new_id: &str,
    body: &str,
) -> Result<Vec<(PathBuf, PathBuf)>, AppError> {
    let old_dir = asset_dir_for_post(ctx, old_id)?;
    let new_dir = asset_dir_for_post(ctx, new_id)?;
    if old_dir == new_dir {
        return Ok(Vec::new());
    }

    let files = referenced_colocated_image_files(ctx, old_id, body)?;
    if files.is_empty() {
        return Ok(Vec::new());
    }
    validate_asset_directory_target(ctx, &new_dir)?;
    let mut moves = Vec::with_capacity(files.len());
    for source in files {
        let name = source
            .file_name()
            .ok_or_else(|| AppError::Io("图片路径没有文件名".into()))?;
        let target = new_dir.join(name);
        if target.exists() {
            return Err(AppError::AlreadyExists(format!(
                "目标图片已存在：{}",
                target.display()
            )));
        }
        moves.push((source, target));
    }
    Ok(moves)
}

fn validate_asset_directory_target(ctx: &ProjectContext, path: &Path) -> Result<(), AppError> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Io(format!(
            "目标图片目录被其他路径占用：{}",
            path.display()
        )));
    }
    if !path.canonicalize()?.starts_with(&ctx.content_root) {
        return Err(AppError::Io(format!(
            "目标图片目录超出内容目录：{}",
            path.display()
        )));
    }
    Ok(())
}

/// 从 Markdown AST 中提取文章同名目录下的直接图片文件。只有边界、类型和真实路径都
/// 能确认的文件才进入移动/删除计划；无法确认的条目一律保留。
fn referenced_colocated_image_files(
    ctx: &ProjectContext,
    post_id: &str,
    body: &str,
) -> Result<Vec<PathBuf>, AppError> {
    let asset_dir = asset_dir_for_post(ctx, post_id)?;
    if !asset_dir.exists() {
        return Ok(Vec::new());
    }
    let metadata = fs::symlink_metadata(&asset_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(Vec::new());
    }
    let canonical_asset_dir = asset_dir.canonicalize()?;
    if !canonical_asset_dir.starts_with(&ctx.content_root) {
        return Ok(Vec::new());
    }
    let post_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    let post_parent = post_path
        .parent()
        .ok_or_else(|| AppError::Io("文章路径没有父目录".into()))?;
    let mut files = BTreeSet::new();

    for (event, _) in markdown::events_with_offsets(body) {
        let Event::Start(Tag::Image { dest_url, .. }) = event else {
            continue;
        };
        let destination = dest_url
            .split(['?', '#'])
            .next()
            .unwrap_or(dest_url.as_ref());
        let destination = destination
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
            .unwrap_or(destination);
        let destination = destination.strip_prefix("./").unwrap_or(destination);
        let Ok(decoded) = percent_decode_str(destination).decode_utf8() else {
            continue;
        };
        let relative = Path::new(decoded.as_ref());
        if relative.is_absolute()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            continue;
        }
        let candidate = post_parent.join(relative);
        let Ok(metadata) = fs::symlink_metadata(&candidate) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let canonical = candidate.canonicalize()?;
        if canonical.starts_with(&ctx.content_root)
            && canonical.parent() == Some(canonical_asset_dir.as_path())
        {
            files.insert(canonical);
        }
    }
    Ok(files.into_iter().collect())
}

/// 只改 Markdown 解析器确认过的图片目标；代码块、普通文本和链接不参与替换。
fn rewrite_colocated_image_paths(
    body: &str,
    old_id: &str,
    new_id: &str,
) -> Result<String, AppError> {
    let old_dir = Path::new(old_id)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppError::InvalidPostId(old_id.to_owned()))?;
    let new_dir = Path::new(new_id)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppError::InvalidPostId(new_id.to_owned()))?;
    if old_dir == new_dir {
        return Ok(body.to_owned());
    }

    let raw_old = format!("{old_dir}/");
    let encoded_old = format!("{}/", encode_uri_segment(old_dir));
    let encoded_new = format!("{}/", encode_uri_segment(new_dir));
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();

    for (event, range) in markdown::events_with_offsets(body) {
        let Event::Start(Tag::Image { .. }) = event else {
            continue;
        };
        let source = &body[range.clone()];
        let Some(destination_range) = inline_image_destination_range(source) else {
            // 引用式图片的真实 URL 位于定义处，不属于这段源码；这里保守跳过。
            continue;
        };
        let destination = &source[destination_range.clone()];
        let leading_len = usize::from(destination.starts_with("./")) * 2;
        let core = &destination[leading_len..];
        let decoded = percent_decode_str(core).decode_utf8_lossy();

        let prefix_len = if core.starts_with(&encoded_old) {
            Some(encoded_old.len())
        } else if core.starts_with(&raw_old) {
            Some(raw_old.len())
        } else if decoded.starts_with(&raw_old) {
            core.find('/').map(|slash| slash + 1)
        } else {
            None
        };
        if let Some(prefix_len) = prefix_len {
            let start = range.start + destination_range.start + leading_len;
            edits.push((start..(start + prefix_len), encoded_new.clone()));
        }
    }

    let mut rewritten = body.to_owned();
    edits.sort_by_key(|(range, _)| range.start);
    for (range, replacement) in edits.into_iter().rev() {
        rewritten.replace_range(range, &replacement);
    }
    Ok(rewritten)
}

/// 从一段 `![alt](destination "title")` 源码中定位 destination 的精确字节范围。
/// 不把 title 或 alt 里恰好相同的文本误当成 URL；引用式图片会返回 None。
fn inline_image_destination_range(source: &str) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    if !bytes.starts_with(b"![") {
        return None;
    }

    let mut index = 2;
    let mut bracket_depth = 1usize;
    let open_paren = loop {
        let byte = *bytes.get(index)?;
        if byte == b'\\' {
            index += 1;
            index += source.get(index..)?.chars().next()?.len_utf8();
            continue;
        }
        if byte == b'[' {
            bracket_depth += 1;
        } else if byte == b']' {
            bracket_depth -= 1;
            if bracket_depth == 0 {
                if bytes.get(index + 1) == Some(&b'(') {
                    break index + 1;
                }
                return None;
            }
        }
        index += source.get(index..)?.chars().next()?.len_utf8();
    };

    index = open_paren + 1;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'<') {
        let start = index + 1;
        index = start;
        while let Some(&byte) = bytes.get(index) {
            if byte == b'\\' {
                index += 1;
                index += source.get(index..)?.chars().next()?.len_utf8();
            } else if byte == b'>' {
                return Some(start..index);
            } else {
                index += source.get(index..)?.chars().next()?.len_utf8();
            }
        }
        return None;
    }

    let start = index;
    let mut paren_depth = 0usize;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'\\' {
            index += 1;
            index += source.get(index..)?.chars().next()?.len_utf8();
        } else if byte == b'(' {
            paren_depth += 1;
            index += 1;
        } else if byte == b')' {
            if paren_depth == 0 {
                return Some(start..index);
            }
            paren_depth -= 1;
            index += 1;
        } else if byte.is_ascii_whitespace() && paren_depth == 0 {
            return Some(start..index);
        } else {
            index += source.get(index..)?.chars().next()?.len_utf8();
        }
    }
    None
}

fn encode_uri_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub fn delete_post(
    ctx: &ProjectContext,
    id: &str,
    expected_revision: &str,
) -> Result<(), AppError> {
    delete_post_with(ctx, id, expected_revision, |paths| {
        trash::delete_all(paths)
            .map_err(|error| AppError::Io(format!("移入系统废纸篓失败：{error}")))
    })
}

fn delete_post_with(
    ctx: &ProjectContext,
    id: &str,
    expected_revision: &str,
    delete: impl FnOnce(&[PathBuf]) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let post_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, id)?;
    let bytes = read_existing(&post_path, id)?;
    if revision_of(&bytes) != expected_revision {
        return Err(AppError::ExternalModificationConflict);
    }

    let text =
        String::from_utf8(bytes).map_err(|_| AppError::Io(format!("文件不是 UTF-8 编码：{id}")))?;
    let (_, body) = split_markdown(&text);
    let asset_dir = asset_dir_for_post(ctx, id)?;
    let mut targets = referenced_colocated_image_files(ctx, id, body)?;
    targets.push(post_path);
    delete(&targets)?;
    remove_dir_if_empty(&asset_dir);
    Ok(())
}

fn read_existing(path: &Path, id: &str) -> Result<Vec<u8>, AppError> {
    fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => AppError::NotFound(id.to_owned()),
        _ => AppError::Io(e.to_string()),
    })
}

/// 同目录临时文件 + rename，避免写到一半崩溃产生半截文件
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    atomic_write_checked(path, bytes, None)
}

fn atomic_write_checked(
    path: &Path,
    bytes: &[u8],
    expected_revision: Option<&str>,
) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("目标路径没有父目录".into()))?;
    let permissions = fs::metadata(path)?.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.as_file().set_permissions(permissions)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    // 把外部修改检测尽量贴近最终 rename，缩小首次检查与落盘之间的竞态窗口。
    if let Some(expected) = expected_revision {
        if revision_of(&fs::read(path)?) != expected {
            return Err(AppError::ExternalModificationConflict);
        }
    }
    tmp.persist(path).map_err(|e| AppError::Io(e.to_string()))?;
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), AppError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectConfig;

    fn ctx() -> (tempfile::TempDir, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let content_root = dir.path().canonicalize().unwrap();
        let ctx = ProjectContext {
            root: content_root.clone(),
            config_path: content_root.join(".cloudstack.json"),
            content_root,
            config: ProjectConfig::default(),
        };
        (dir, ctx)
    }

    fn rename_current(
        ctx: &ProjectContext,
        old_id: &str,
        new_id: &str,
    ) -> Result<PostDocument, AppError> {
        let revision = read_post(ctx, old_id)?.revision;
        let app_data = tempfile::tempdir().unwrap();
        rename_post(ctx, old_id, new_id, &revision, app_data.path())
    }

    fn rename_current_in(
        ctx: &ProjectContext,
        old_id: &str,
        new_id: &str,
        app_data_dir: &Path,
    ) -> Result<PostDocument, AppError> {
        let revision = read_post(ctx, old_id)?.revision;
        rename_post(ctx, old_id, new_id, &revision, app_data_dir)
    }

    #[test]
    fn split_lf_file() {
        let (fm, body) = split_markdown("---\ntitle: a\n---\n\n# Hi\n");
        assert_eq!(fm, Some("title: a\n"));
        assert_eq!(body, "\n# Hi\n");
    }

    #[test]
    fn split_crlf_file() {
        let (fm, body) = split_markdown("---\r\ntitle: a\r\n---\r\n\r\nhi");
        assert_eq!(fm, Some("title: a\r\n"));
        assert_eq!(body, "\r\nhi");
    }

    #[test]
    fn split_bom_is_stripped() {
        let (fm, body) = split_markdown("\u{feff}---\ntitle: a\n---\nbody");
        assert_eq!(fm, Some("title: a\n"));
        assert_eq!(body, "body");
    }

    #[test]
    fn split_no_frontmatter_and_unterminated() {
        assert_eq!(split_markdown("# 只有正文\n"), (None, "# 只有正文\n"));
        // 未闭合的 frontmatter：整篇当正文，不丢内容
        let text = "---\ntitle: a\n没有闭合";
        assert_eq!(split_markdown(text), (None, text));
        // 正文里的 --- 不受影响
        let (fm, body) = split_markdown("---\ntitle: a\n---\nx\n---\ny\n");
        assert_eq!(fm, Some("title: a\n"));
        assert_eq!(body, "x\n---\ny\n");
    }

    #[test]
    fn split_join_roundtrip_is_lossless_for_lf() {
        for text in [
            "---\ntitle: a # 注释\nweird: 'q'\n---\n\n# Hi\n\n正文\n",
            "---\ntitle: a\n---\n",
            "no frontmatter at all\n",
            "---\n---\nempty fm\n",
        ] {
            let (fm, body) = split_markdown(text);
            assert_eq!(join_markdown(fm, body), text, "roundtrip 失败：{text:?}");
        }
    }

    #[test]
    fn read_write_roundtrip_and_revision() {
        let (_dir, ctx) = ctx();
        let original = "---\n# 置顶注释\ntitle: \"hello\"\ncustom_field: keep-me\n---\n\n正文\n";
        std::fs::write(ctx.content_root.join("a.md"), original).unwrap();

        let doc = read_post(&ctx, "a.md").unwrap();
        assert_eq!(
            doc.raw_frontmatter.as_deref(),
            Some("# 置顶注释\ntitle: \"hello\"\ncustom_field: keep-me\n")
        );

        // 原样写回 → 文件字节不变
        let result = write_post(
            &ctx,
            "a.md",
            doc.raw_frontmatter.as_deref(),
            &doc.body,
            doc.format,
            &doc.revision,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            original
        );
        assert_eq!(result.revision, doc.revision);
        assert_eq!(result.format, doc.format);
    }

    #[test]
    fn write_preserves_frontmatter_only_file_without_final_newline() {
        let (_dir, ctx) = ctx();
        // frontmatter-only、正文为空、"---" 之后没有任何换行——body 本身是
        // 空字符串，唯一能区分"磁盘原本就没有这个换行"的信号是读盘时记录
        // 的 has_final_newline。
        let original = "---\ntitle: a\n---";
        std::fs::write(ctx.content_root.join("a.md"), original).unwrap();

        let doc = read_post(&ctx, "a.md").unwrap();
        assert_eq!(doc.body, "");
        assert!(!doc.format.has_final_newline);

        let result = write_post(
            &ctx,
            "a.md",
            doc.raw_frontmatter.as_deref(),
            &doc.body,
            doc.format,
            &doc.revision,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            original
        );
        assert_eq!(result.revision, doc.revision);
    }

    #[test]
    fn write_detects_external_modification() {
        let (_dir, ctx) = ctx();
        std::fs::write(ctx.content_root.join("a.md"), "---\nt: 1\n---\nx").unwrap();
        let doc = read_post(&ctx, "a.md").unwrap();

        // 模拟外部编辑器改了文件
        std::fs::write(ctx.content_root.join("a.md"), "---\nt: 2\n---\ny").unwrap();

        let result = write_post(&ctx, "a.md", None, "覆盖", doc.format, &doc.revision);
        assert!(matches!(
            result,
            Err(AppError::ExternalModificationConflict)
        ));
        // 冲突时绝不落盘
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            "---\nt: 2\n---\ny"
        );
    }

    #[test]
    fn write_preserves_crlf_and_no_final_newline_after_edit() {
        let (_dir, ctx) = ctx();
        std::fs::write(
            ctx.content_root.join("a.md"),
            b"---\r\ntitle: a\r\n---\r\nbody",
        )
        .unwrap();

        let doc = read_post(&ctx, "a.md").unwrap();
        assert_eq!(doc.format.line_ending, LineEnding::CrLf);
        assert!(!doc.format.has_final_newline);
        assert_eq!(doc.body, "body");

        // 用户编辑正文加了一行，但仍然没有在 EOF 按 Enter。
        let edited_body = "body\nmore";
        let result = write_post(
            &ctx,
            "a.md",
            doc.raw_frontmatter.as_deref(),
            edited_body,
            doc.format,
            &doc.revision,
        )
        .unwrap();

        assert_eq!(
            std::fs::read(ctx.content_root.join("a.md")).unwrap(),
            b"---\r\ntitle: a\r\n---\r\nbody\r\nmore".to_vec(),
            "CRLF 风格和缺失的末尾换行都必须原样保留"
        );
        assert_eq!(result.format.line_ending, LineEnding::CrLf);
        assert!(!result.format.has_final_newline);
    }

    #[test]
    fn write_reflects_user_added_or_removed_final_newline() {
        let (_dir, ctx) = ctx();
        std::fs::write(ctx.content_root.join("a.md"), "body").unwrap();
        let doc = read_post(&ctx, "a.md").unwrap();
        assert!(!doc.format.has_final_newline);

        // 用户在 EOF 按了 Enter。
        let added = write_post(&ctx, "a.md", None, "body\n", doc.format, &doc.revision).unwrap();
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            "body\n"
        );
        assert!(added.format.has_final_newline);

        // 又用 Backspace 删掉。
        let removed =
            write_post(&ctx, "a.md", None, "body", added.format, &added.revision).unwrap();
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            "body"
        );
        assert!(!removed.format.has_final_newline);
    }

    #[test]
    fn write_normalizes_mixed_line_endings_to_lf_and_reports_it() {
        let (_dir, ctx) = ctx();
        std::fs::write(ctx.content_root.join("a.md"), b"line1\r\nline2\nline3\r\n").unwrap();
        let doc = read_post(&ctx, "a.md").unwrap();
        assert_eq!(doc.format.line_ending, LineEnding::Mixed);

        let result = write_post(
            &ctx,
            "a.md",
            doc.raw_frontmatter.as_deref(),
            &doc.body,
            doc.format,
            &doc.revision,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            "line1\nline2\nline3\n"
        );
        assert_eq!(
            result.format.line_ending,
            LineEnding::Lf,
            "保存后返回的 format 必须反映实际落盘的字节，不是调用方传入的旧 Mixed"
        );
    }

    #[test]
    fn revision_changes_when_only_line_ending_style_changes() {
        let (_dir, ctx) = ctx();
        std::fs::write(ctx.content_root.join("a.md"), "line1\nline2\n").unwrap();
        let lf_revision = read_post(&ctx, "a.md").unwrap().revision;

        std::fs::write(ctx.content_root.join("a.md"), "line1\r\nline2\r\n").unwrap();
        let crlf_revision = read_post(&ctx, "a.md").unwrap().revision;

        assert_ne!(
            lf_revision, crlf_revision,
            "revision 必须基于原始字节计算，纯 EOL 风格变化也要能被检测为外部修改"
        );
    }

    #[test]
    fn rename_with_image_rewrite_preserves_crlf_and_no_final_newline() {
        let (_dir, ctx) = ctx();
        // 无 frontmatter、CRLF 换行、EOF 没有换行，正文引用了同名目录下的图片。
        std::fs::write(
            ctx.content_root.join("old.md"),
            b"![img](old/p.png)\r\nnext",
        )
        .unwrap();
        fs::create_dir_all(ctx.content_root.join("old")).unwrap();
        fs::write(ctx.content_root.join("old/p.png"), "img").unwrap();

        let doc = rename_current(&ctx, "old.md", "new.md").unwrap();

        // rename 只应该改写图片路径，不应该顺带把 CRLF 规范化成 LF 或补上末尾换行。
        assert_eq!(
            std::fs::read(ctx.content_root.join("new.md")).unwrap(),
            b"![img](new/p.png)\r\nnext".to_vec(),
            "rename 的图片路径重写不能顺带做 EOL 归一化"
        );
        assert!(ctx.content_root.join("new/p.png").is_file());

        // 重新读回来的 PostDocument 也要如实反映磁盘上仍然是 CRLF、没有末尾换行。
        assert_eq!(doc.format.line_ending, LineEnding::CrLf);
        assert!(!doc.format.has_final_newline);
        assert_eq!(doc.body, "![img](new/p.png)\nnext");
    }

    #[test]
    fn rename_post_moves_asset_dir_when_present() {
        let (_dir, ctx) = ctx();
        create_post(&ctx, "hello.md", None, "![](hello/cover.png)\n").unwrap();
        std::fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        std::fs::write(ctx.content_root.join("hello/cover.png"), "img").unwrap();
        std::fs::write(ctx.content_root.join("hello/unrelated.json"), "keep").unwrap();

        rename_current(&ctx, "hello.md", "renamed.md").unwrap();

        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("renamed/cover.png")).unwrap(),
            "img"
        );
        // 同名目录不能被当成整篇文章所有；未被正文引用的用户文件留在原处。
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("hello/unrelated.json")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn rename_post_without_asset_dir_still_succeeds() {
        let (_dir, ctx) = ctx();
        create_post(&ctx, "hello.md", None, "body").unwrap();
        // 从没插过图片，没有 hello/ 目录——重命名应该照常成功，不能因为目录不存在而报错
        let summary = rename_current(&ctx, "hello.md", "renamed.md").unwrap();
        assert_eq!(summary.id, "renamed.md");
    }

    #[test]
    fn rename_post_rejects_when_target_asset_dir_already_taken() {
        let (_dir, ctx) = ctx();
        create_post(&ctx, "a.md", None, "![](a/img.png)\n").unwrap();
        std::fs::create_dir_all(ctx.content_root.join("a")).unwrap();
        std::fs::write(ctx.content_root.join("a/img.png"), "1").unwrap();

        // 目标目录可以包含其他文件，但同名目标图片绝不能覆盖。
        std::fs::create_dir_all(ctx.content_root.join("b")).unwrap();
        std::fs::write(ctx.content_root.join("b/img.png"), "existing").unwrap();

        let err = rename_current(&ctx, "a.md", "b.md").unwrap_err();
        assert!(matches!(err, AppError::AlreadyExists(_)));
        // 冲突时 .md 本体不应该已经被搬走（先检查资产目录，再动 .md）
        assert!(ctx.content_root.join("a.md").is_file());
    }

    #[test]
    fn create_rejects_existing_and_rename_rejects_overwrite() {
        let (_dir, ctx) = ctx();
        create_post(&ctx, "a.md", Some("title: a\n"), "\nbody\n").unwrap();
        assert!(matches!(
            create_post(&ctx, "a.md", None, ""),
            Err(AppError::AlreadyExists(_))
        ));

        create_post(&ctx, "b.md", None, "b").unwrap();
        assert!(matches!(
            rename_current(&ctx, "a.md", "b.md"),
            Err(AppError::AlreadyExists(_))
        ));

        // 正常重命名（含新建子目录）
        let summary = rename_current(&ctx, "a.md", "2026/a-renamed.md").unwrap();
        assert_eq!(summary.id, "2026/a-renamed.md");
        assert!(ctx.content_root.join("2026/a-renamed.md").is_file());
        assert!(!ctx.content_root.join("a.md").exists());
    }

    #[test]
    fn rename_rewrites_only_parsed_colocated_image_targets() {
        let (_dir, ctx) = ctx();
        create_post(
            &ctx,
            "hello world.md",
            None,
            "![hello%20world/alt](./hello%20world/cover.png \"hello%20world/title\")\n\n[普通链接](hello%20world/page)\n\n$![](hello%20world/math.png)$\n\n```md\n![](hello%20world/code.png)\n```\n",
        )
        .unwrap();
        fs::create_dir_all(ctx.content_root.join("hello world")).unwrap();
        fs::write(ctx.content_root.join("hello world/cover.png"), "img").unwrap();

        let doc = rename_current(&ctx, "hello world.md", "new name.md").unwrap();
        assert!(doc
            .body
            .contains("![hello%20world/alt](./new%20name/cover.png \"hello%20world/title\")"));
        assert!(doc.body.contains("[普通链接](hello%20world/page)"));
        assert!(doc.body.contains("$![](hello%20world/math.png)$"));
        assert!(doc.body.contains("![](hello%20world/code.png)"));
        assert!(ctx.content_root.join("new name/cover.png").is_file());
    }

    #[test]
    fn rename_moves_only_referenced_images_and_preserves_nested_articles() {
        let (_dir, ctx) = ctx();
        create_post(&ctx, "a.md", None, "![](a/cover.png)\n").unwrap();
        fs::create_dir_all(ctx.content_root.join("a")).unwrap();
        fs::write(ctx.content_root.join("a/cover.png"), "image").unwrap();
        create_post(&ctx, "a/nested.md", None, "nested").unwrap();

        rename_current(&ctx, "a.md", "renamed.md").unwrap();

        assert!(!ctx.content_root.join("a.md").exists());
        assert!(ctx.content_root.join("a/nested.md").is_file());
        assert!(ctx.content_root.join("renamed.md").is_file());
        assert!(ctx.content_root.join("renamed/cover.png").is_file());
    }

    #[test]
    fn rename_rejects_stale_revision_before_moving_anything() {
        let (_dir, ctx) = ctx();
        let doc = create_post(&ctx, "a.md", None, "body").unwrap();
        fs::write(ctx.content_root.join("a.md"), "external edit").unwrap();

        let app_data = tempfile::tempdir().unwrap();
        let error = rename_post(&ctx, "a.md", "b.md", &doc.revision, app_data.path()).unwrap_err();
        assert!(matches!(error, AppError::ExternalModificationConflict));
        assert!(ctx.content_root.join("a.md").is_file());
        assert!(!ctx.content_root.join("b.md").exists());
        assert!(
            fs::read_dir(app_data.path().join("operations"))
                .into_iter()
                .flatten()
                .next()
                .is_none(),
            "revision 冲突在写 journal 之前就该被拒绝，不应该留下任何操作日志"
        );
    }

    #[test]
    fn rename_removes_its_journal_on_success_and_leaves_none_behind() {
        let (_dir, ctx) = ctx();
        create_post(&ctx, "hello.md", None, "![](hello/cover.png)").unwrap();
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        fs::write(ctx.content_root.join("hello/cover.png"), "png").unwrap();
        let app_data = tempfile::tempdir().unwrap();

        rename_current_in(&ctx, "hello.md", "world.md", app_data.path()).unwrap();

        let operations_dir = app_data.path().join("operations");
        let remaining: Vec<_> = fs::read_dir(&operations_dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .collect();
        assert!(
            remaining.is_empty(),
            "重命名成功后不应该留下任何操作日志：{remaining:?}"
        );
    }

    #[test]
    fn delete_collects_post_and_asset_directory_after_revision_check() {
        let (_dir, ctx) = ctx();
        let doc = create_post(&ctx, "a.md", None, "![](a/cover.png)\n").unwrap();
        fs::create_dir_all(ctx.content_root.join("a")).unwrap();
        fs::write(ctx.content_root.join("a/cover.png"), "img").unwrap();
        fs::write(ctx.content_root.join("a/unrelated.json"), "keep").unwrap();

        let mut seen = Vec::new();
        delete_post_with(&ctx, "a.md", &doc.revision, |paths| {
            seen.extend_from_slice(paths);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            seen,
            vec![
                ctx.content_root.join("a/cover.png").canonicalize().unwrap(),
                ctx.content_root.join("a.md"),
            ]
        );
        assert!(!seen.contains(&ctx.content_root.join("a/unrelated.json")));
    }

    #[test]
    fn delete_rejects_stale_revision_without_calling_trash() {
        let (_dir, ctx) = ctx();
        let doc = create_post(&ctx, "a.md", None, "body").unwrap();
        fs::write(ctx.content_root.join("a.md"), "external edit").unwrap();
        let mut called = false;

        let error = delete_post_with(&ctx, "a.md", &doc.revision, |_| {
            called = true;
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(error, AppError::ExternalModificationConflict));
        assert!(!called);
        assert!(ctx.content_root.join("a.md").is_file());
    }

    #[test]
    fn delete_never_treats_nested_article_directory_as_owned_assets() {
        let (_dir, ctx) = ctx();
        let doc = create_post(&ctx, "a.md", None, "body").unwrap();
        fs::create_dir_all(ctx.content_root.join("a")).unwrap();
        create_post(&ctx, "a/nested.md", None, "nested").unwrap();

        let mut seen = Vec::new();
        delete_post_with(&ctx, "a.md", &doc.revision, |paths| {
            seen.extend_from_slice(paths);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![ctx.content_root.join("a.md")]);
    }

    #[test]
    fn fixture_smoke_readonly() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/test-blog");
        let ctx = crate::services::project::open_project(&fixture).unwrap();

        let ids: Vec<String> = list_posts(&ctx)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert!(ids.contains(&"hello-astro.md".to_string()));
        assert!(ids.contains(&"nested/2026-plans.md".to_string()));

        // 每篇文章拆分/重组都必须无损（含正文代码块里的 --- 行）
        for id in &ids {
            let doc = read_post(&ctx, id).unwrap();
            let raw = std::fs::read_to_string(ctx.content_root.join(id)).unwrap();
            assert_eq!(
                join_markdown(doc.raw_frontmatter.as_deref(), &doc.body),
                raw,
                "{id} 拆分/重组必须无损"
            );
        }

        let tricky = read_post(&ctx, "tricky-frontmatter.md").unwrap();
        let fm = tricky.raw_frontmatter.unwrap();
        assert!(fm.contains("# 这条注释必须在保存后原样保留"));
        assert!(fm.contains("legacy_field"));

        let plain = read_post(&ctx, "no-frontmatter.md").unwrap();
        assert!(plain.raw_frontmatter.is_none());
    }

    #[test]
    fn list_skips_hidden_and_non_matching() {
        let (_dir, ctx) = ctx();
        std::fs::write(ctx.content_root.join("a.md"), "a").unwrap();
        std::fs::create_dir_all(ctx.content_root.join("nested")).unwrap();
        std::fs::write(ctx.content_root.join("nested/b.md"), "b").unwrap();
        std::fs::write(ctx.content_root.join(".hidden.md"), "h").unwrap();
        std::fs::write(ctx.content_root.join("notes.txt"), "t").unwrap();

        let ids: Vec<String> = list_posts(&ctx)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec!["a.md".to_string(), "nested/b.md".to_string()]);
    }

    fn ctx_with_tag_fields(field_names: &[&str]) -> (tempfile::TempDir, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let content_root = dir.path().canonicalize().unwrap();
        let mut config = ProjectConfig::default();
        config.frontmatter.fields = field_names
            .iter()
            .map(|name| crate::model::FieldSpec {
                name: name.to_string(),
                field_type: "tags".into(),
                required: false,
                default: None,
            })
            .collect();
        let ctx = ProjectContext {
            root: content_root.clone(),
            config_path: content_root.join(".cloudstack.json"),
            content_root,
            config,
        };
        (dir, ctx)
    }

    #[test]
    fn list_tags_dedupes_and_sorts_across_posts() {
        let (_dir, ctx) = ctx_with_tag_fields(&["tags"]);
        std::fs::write(
            ctx.content_root.join("a.md"),
            "---\ntags: [astro, rust]\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            ctx.content_root.join("b.md"),
            "---\ntags: [rust, zig]\n---\nbody",
        )
        .unwrap();

        let tags = list_tags(&ctx).unwrap();
        assert_eq!(
            tags.get("tags").unwrap(),
            &vec!["astro".to_string(), "rust".to_string(), "zig".to_string()]
        );
    }

    #[test]
    fn list_tags_keeps_different_tag_fields_separate() {
        let (_dir, ctx) = ctx_with_tag_fields(&["tags", "categories"]);
        std::fs::write(
            ctx.content_root.join("a.md"),
            "---\ntags: [astro]\ncategories: [教程]\n---\nbody",
        )
        .unwrap();

        let tags = list_tags(&ctx).unwrap();
        assert_eq!(tags.get("tags").unwrap(), &vec!["astro".to_string()]);
        assert_eq!(tags.get("categories").unwrap(), &vec!["教程".to_string()]);
    }

    #[test]
    fn list_tags_skips_malformed_frontmatter_without_failing() {
        let (_dir, ctx) = ctx_with_tag_fields(&["tags"]);
        std::fs::write(
            ctx.content_root.join("broken.md"),
            "---\ntags: [unclosed\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            ctx.content_root.join("ok.md"),
            "---\ntags: [astro]\n---\nbody",
        )
        .unwrap();

        let tags = list_tags(&ctx).unwrap();
        assert_eq!(tags.get("tags").unwrap(), &vec!["astro".to_string()]);
    }

    #[test]
    fn list_tags_empty_when_no_tags_field_configured() {
        let (_dir, ctx) = ctx(); // 默认配置没有任何 frontmatter 字段
        std::fs::write(
            ctx.content_root.join("a.md"),
            "---\ntags: [astro]\n---\nbody",
        )
        .unwrap();
        assert!(list_tags(&ctx).unwrap().is_empty());
    }
}
