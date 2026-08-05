use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Tag};

use crate::error::AppError;
use crate::model::{AssetMode, ProjectContext, SavedImage};
use crate::path_guard::resolve_post_path;
use crate::services::markdown;
use crate::services::posts::{revision_of, split_markdown};

const MAX_PREVIEW_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_SAVED_IMAGE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug)]
pub struct SaveImageOutcome {
    pub image: SavedImage,
    /// 只有这次调用真正创建了文件时才登记；命中内容寻址缓存时不拥有该文件。
    pub pending: Option<PendingAsset>,
}

#[derive(Debug, Clone)]
pub struct PendingAsset {
    project_root: PathBuf,
    content_root: PathBuf,
    post_id: String,
    post_path: PathBuf,
    asset_dir: PathBuf,
    file_path: PathBuf,
    markdown_path: String,
    revision: String,
}

/// 图片先落盘、Markdown 后保存，两步之间需要一个很小的事务层。这里仅跟踪本次应用
/// 会话中新建的图片；文章成功保存后确认，放弃编辑/切换项目/退出时则做保守清理。
#[derive(Debug, Default)]
pub struct PendingAssetManager {
    entries: Vec<PendingAsset>,
}

impl PendingAssetManager {
    pub fn track(&mut self, asset: PendingAsset) {
        if !self
            .entries
            .iter()
            .any(|entry| entry.file_path == asset.file_path)
        {
            self.entries.push(asset);
        }
    }

    pub fn confirm_post(&mut self, project_root: &Path, post_id: &str) {
        self.entries
            .retain(|entry| entry.project_root != project_root || entry.post_id != post_id);
    }

    #[cfg(test)]
    pub fn has_pending(&self, project_root: &Path, post_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.project_root == project_root && entry.post_id == post_id)
    }

    pub fn has_pending_project(&self, project_root: &Path) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.project_root == project_root)
    }

    pub fn forget_post(&mut self, project_root: &Path, post_id: &str) {
        self.confirm_post(project_root, post_id);
    }

    pub fn discard_post(&mut self, project_root: &Path, post_id: &str) -> Result<usize, AppError> {
        self.cleanup_where(|entry| entry.project_root == project_root && entry.post_id == post_id)
    }

    pub fn discard_project(&mut self, project_root: &Path) -> Result<usize, AppError> {
        self.cleanup_where(|entry| entry.project_root == project_root)
    }

    pub fn discard_all(&mut self) -> Result<usize, AppError> {
        self.cleanup_where(|_| true)
    }

    fn cleanup_where(
        &mut self,
        predicate: impl Fn(&PendingAsset) -> bool,
    ) -> Result<usize, AppError> {
        let mut remaining = Vec::with_capacity(self.entries.len());
        let mut cleaned = 0;
        let mut first_error = None;

        for entry in std::mem::take(&mut self.entries) {
            if !predicate(&entry) {
                remaining.push(entry);
                continue;
            }
            match cleanup_pending_asset(&entry) {
                Ok(()) => cleaned += 1,
                Err(error) => {
                    // 暂时性 IO 错误保留记录，下一次切换/退出还能重试。
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    remaining.push(entry);
                }
            }
        }
        self.entries = remaining;
        match first_error {
            Some(error) => Err(error),
            None => Ok(cleaned),
        }
    }
}

/// 只取 basename，拒绝空/`.`/`..`——天然防路径穿越，不需要额外正则
fn sanitize_filename(name: &str) -> Option<String> {
    let base = Path::new(name).file_name()?.to_str()?;
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    Some(base.to_string())
}

/// `dir/desired` 已存在时在扩展名前插 `-1`/`-2`/... 直到找到未占用的名字
fn unique_filename(dir: &Path, desired: &str) -> String {
    if !dir.join(desired).exists() {
        return desired.to_string();
    }
    let path = Path::new(desired);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(desired);
    let ext = path.extension().and_then(|s| s.to_str());
    for i in 1u32.. {
        let candidate = match ext {
            Some(ext) => format!("{stem}-{i}.{ext}"),
            None => format!("{stem}-{i}"),
        };
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!("dir 里不可能有无限多个同名候选文件")
}

/// 剪贴板粘贴没有文件名，只能从字节内容猜扩展名。infer 只看文件签名，不解码图片，
/// 同时覆盖 PNG/JPEG/GIF/WebP 之外的 BMP/AVIF/TIFF/ICO 等常见格式。
fn sniff_image_extension(bytes: &[u8]) -> Option<&'static str> {
    infer::get(bytes)
        .filter(|kind| kind.mime_type().starts_with("image/"))
        .map(|kind| kind.extension())
}

/// 资产目录跟随真实文件系统 stem，必须保留大小写和标点。它与 Astro 为 URL 生成的
/// Content Entry slug 是两个概念，不能复用 preview 的 slugify 逻辑。
fn post_stem_path(post_id: &str) -> &str {
    let last_slash = post_id.rfind('/').map(|index| index + 1).unwrap_or(0);
    match post_id[last_slash..].rfind('.') {
        Some(relative_index) => &post_id[..last_slash + relative_index],
        None => post_id,
    }
}

pub(crate) fn asset_dir_for_post(ctx: &ProjectContext, post_id: &str) -> Result<PathBuf, AppError> {
    // 先走统一的 PostId 路径守卫；资产目录由同一条已校验的相对路径派生。
    let _ = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    match ctx.config.assets.mode {
        AssetMode::Colocated => Ok(ctx.content_root.join(post_stem_path(post_id))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AssetDirectory {
    Missing,
    /// 普通项目内目录；不能据此推断整目录归编辑器所有。
    Directory,
    /// 同 stem 路径被文件、符号链接或边界外目录占用，不能视为文章资产。
    Unowned,
}

pub(crate) fn inspect_asset_directory(
    ctx: &ProjectContext,
    post_id: &str,
) -> Result<AssetDirectory, AppError> {
    let asset_dir = asset_dir_for_post(ctx, post_id)?;
    if !asset_dir.exists() {
        return Ok(AssetDirectory::Missing);
    }
    let metadata = fs::symlink_metadata(&asset_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(AssetDirectory::Unowned);
    }
    let canonical = asset_dir.canonicalize()?;
    if !canonical.starts_with(&ctx.content_root) {
        return Ok(AssetDirectory::Unowned);
    }
    Ok(AssetDirectory::Directory)
}

/// Markdown 图片路径相对文章所在目录，而资产目录路径相对 content_root。
/// 例如 `nested/post.md` 的资产落在 `nested/post/image.png`，正文应写
/// `post/image.png`，不能写 `nested/post/image.png`（否则会解析成 nested/nested/...）。
fn markdown_asset_path(post_id: &str, filename: &str) -> Result<String, AppError> {
    let asset_dir_name = Path::new(post_stem_path(post_id))
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::InvalidPostId(post_id.to_owned()))?;
    Ok(format!("{asset_dir_name}/{filename}"))
}

/// 保存一张图片到文章的同名资产子目录，返回相对文章文件的 Markdown 路径。
pub fn save_image(
    ctx: &ProjectContext,
    post_id: &str,
    suggested_name: Option<&str>,
    bytes: &[u8],
) -> Result<SaveImageOutcome, AppError> {
    if bytes.is_empty() {
        return Err(AppError::Io("图片文件为空，拒绝保存".into()));
    }
    if bytes.len() > MAX_SAVED_IMAGE_BYTES {
        return Err(AppError::Io("单张图片超过 25 MiB，拒绝保存".into()));
    }
    let post_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    if !post_path.is_file() {
        return Err(AppError::NotFound(post_id.to_owned()));
    }

    let asset_dir = asset_dir_for_post(ctx, post_id)?;
    fs::create_dir_all(&asset_dir)?;
    if matches!(
        inspect_asset_directory(ctx, post_id)?,
        AssetDirectory::Unowned
    ) {
        return Err(AppError::Io(format!(
            "无法保存图片：{} 不是项目内的普通目录，请先调整文章或目录名称",
            asset_dir.display()
        )));
    }
    // 双重校验：跟 path_guard 一贯的风格一致，即便 slug 来自已校验过的 post_id 也再核对一次
    let canon = asset_dir.canonicalize()?;
    if !canon.starts_with(&ctx.content_root) {
        return Err(AppError::InvalidPostId(post_id.to_owned()));
    }

    let (final_name, should_write) = match suggested_name.and_then(sanitize_filename) {
        // 有真实文件名（拖拽）：不同内容撞同名是正常情况，交给 unique_filename 加后缀
        Some(name) => (unique_filename(&asset_dir, &name), true),
        // 没有文件名（剪贴板粘贴）：内容寻址命名，同样的字节天然映射到同一个文件名，
        // 已存在就直接复用（不重复占地方），不能走 unique_filename 那套"名字冲突就加后缀"的逻辑
        None => {
            let ext = sniff_image_extension(bytes).unwrap_or("png");
            let desired = format!("{}.{ext}", &revision_of(bytes)[..8]);
            let desired_path = asset_dir.join(&desired);
            if desired_path.is_file() && fs::read(&desired_path)? == bytes {
                (desired, false)
            } else if desired_path.exists() {
                // 8 位短哈希理论上可能碰撞，也可能被同名目录占用；绝不能覆盖。
                (unique_filename(&asset_dir, &desired), true)
            } else {
                (desired, true)
            }
        }
    };

    let markdown_path = markdown_asset_path(post_id, &final_name)?;
    let file_path = asset_dir.join(&final_name);
    if should_write {
        if let Err(error) = write_new_file_atomically(&file_path, bytes) {
            let _ = fs::remove_file(&file_path);
            remove_dir_if_empty(&asset_dir);
            return Err(error);
        }
    }

    let pending = if should_write {
        let canonical_file = match file_path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_file(&file_path);
                remove_dir_if_empty(&asset_dir);
                return Err(error.into());
            }
        };
        if !canonical_file.starts_with(&ctx.content_root)
            || canonical_file.parent() != Some(canon.as_path())
        {
            let _ = fs::remove_file(&file_path);
            remove_dir_if_empty(&asset_dir);
            return Err(AppError::InvalidPostId(post_id.to_owned()));
        }
        Some(PendingAsset {
            project_root: ctx.root.clone(),
            content_root: ctx.content_root.clone(),
            post_id: post_id.to_owned(),
            post_path,
            asset_dir: canon,
            file_path: canonical_file,
            markdown_path: markdown_path.clone(),
            revision: revision_of(bytes),
        })
    } else {
        None
    };
    Ok(SaveImageOutcome {
        image: SavedImage { markdown_path },
        pending,
    })
}

fn write_new_file_atomically(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("图片路径没有父目录".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            AppError::AlreadyExists(path.display().to_string())
        } else {
            AppError::Io(error.error.to_string())
        }
    })?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn is_previewable_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "avif"
                    | "bmp"
                    | "tif"
                    | "tiff"
                    | "ico"
                    | "svg"
            )
        })
}

/// 读取当前文章引用的本地图片。只接受相对文章目录的 URL，并在 percent decode 与
/// canonicalize 后再次确认仍位于 content_root；不为前端开放通用文件读取能力。
pub fn read_image_asset(
    ctx: &ProjectContext,
    post_id: &str,
    markdown_path: &str,
) -> Result<Vec<u8>, AppError> {
    let post_path = resolve_post_path(&ctx.content_root, &ctx.config.extensions, post_id)?;
    if !post_path.is_file() {
        return Err(AppError::NotFound(post_id.to_owned()));
    }

    let target = markdown_path
        .split(['?', '#'])
        .next()
        .unwrap_or(markdown_path)
        .trim();
    let target = target
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(target);
    let decoded = percent_decode_str(target)
        .decode_utf8()
        .map_err(|_| AppError::Io("图片路径不是有效的 UTF-8 URL".into()))?;
    let relative = Path::new(decoded.as_ref());
    if decoded.is_empty()
        || decoded.contains('\0')
        || decoded.contains('\\')
        || decoded.contains("://")
        || decoded.starts_with("//")
        || relative.is_absolute()
    {
        return Err(AppError::Io("图片预览只允许文章目录内的相对路径".into()));
    }

    let post_dir = post_path
        .parent()
        .ok_or_else(|| AppError::InvalidPostId(post_id.to_owned()))?;
    let candidate = post_dir.join(relative);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| AppError::Io(format!("找不到图片：{markdown_path}")))?;
    if !canonical.starts_with(&ctx.content_root) || !canonical.is_file() {
        return Err(AppError::Io("图片路径超出项目内容目录".into()));
    }
    if !is_previewable_image_path(&canonical) {
        return Err(AppError::Io("该文件类型不能作为图片预览".into()));
    }

    let size = fs::metadata(&canonical)?.len();
    if size > MAX_PREVIEW_IMAGE_BYTES {
        return Err(AppError::Io(format!(
            "图片超过实时预览的 25 MiB 限制：{markdown_path}"
        )));
    }
    Ok(fs::read(canonical)?)
}

fn remove_dir_if_empty(dir: &Path) {
    if dir.is_dir() && fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(dir);
    }
}

fn cleanup_pending_asset(entry: &PendingAsset) -> Result<(), AppError> {
    if !entry.file_path.exists() {
        return Ok(());
    }

    let current_path = entry.file_path.canonicalize()?;
    if current_path != entry.file_path
        || !current_path.starts_with(&entry.content_root)
        || current_path.parent() != Some(entry.asset_dir.as_path())
    {
        return Err(AppError::Io(format!(
            "待清理图片路径已变化，拒绝删除：{}",
            entry.file_path.display()
        )));
    }

    let bytes = fs::read(&current_path)?;
    if revision_of(&bytes) != entry.revision {
        // 文件被外部修改后就不再归本次粘贴事务所有，保留并解除跟踪。
        return Ok(());
    }

    if entry.post_path.is_file() {
        let post = fs::read_to_string(&entry.post_path)?;
        if markdown_references_image(split_markdown(&post).1, &entry.markdown_path) {
            // 可能由其他编辑器/崩溃恢复流程保存过；正文已有引用时宁可保留。
            return Ok(());
        }
    }

    fs::remove_file(&current_path)?;
    if entry.asset_dir.is_dir() && fs::read_dir(&entry.asset_dir)?.next().is_none() {
        fs::remove_dir(&entry.asset_dir)?;
    }
    Ok(())
}

/// 一次批量剪贴板导入中途失败时，只回滚这次已经创建的文件；清理失败的条目返回给
/// 调用方继续纳入会话级 PendingAssetManager，避免误删此前已插入编辑器的图片。
pub fn rollback_pending_assets(entries: Vec<PendingAsset>) -> Vec<PendingAsset> {
    let mut failed = Vec::new();
    for entry in entries.into_iter().rev() {
        if let Err(error) = cleanup_pending_asset(&entry) {
            log::warn!("回滚剪贴板图片失败：{error}");
            failed.push(entry);
        }
    }
    failed
}

fn markdown_references_image(markdown: &str, target: &str) -> bool {
    markdown::events_with_offsets(markdown).any(|(event, _)| match event {
        Event::Start(Tag::Image { dest_url, .. }) => {
            let destination = dest_url
                .split(['?', '#'])
                .next()
                .unwrap_or(dest_url.as_ref());
            let destination = destination.strip_prefix("./").unwrap_or(destination);
            destination == target || percent_decode_str(destination).decode_utf8_lossy() == target
        }
        _ => false,
    })
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

    const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn sanitize_filename_strips_to_basename_and_rejects_empty() {
        assert_eq!(sanitize_filename("cover.png"), Some("cover.png".into()));
        // 只取 basename——"../evil.png" 被归约成安全的 "evil.png"，不是拒绝整个请求；
        // 反正只会拼到 asset_dir 下面当直接子文件，取到什么 basename 都逃不出 asset_dir
        assert_eq!(sanitize_filename("../evil.png"), Some("evil.png".into()));
        assert_eq!(sanitize_filename("a/b.png"), Some("b.png".into()));
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("."), None);
        assert_eq!(sanitize_filename(".."), None);
    }

    #[test]
    fn unique_filename_increments_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_filename(dir.path(), "a.png"), "a.png");

        std::fs::write(dir.path().join("a.png"), "x").unwrap();
        assert_eq!(unique_filename(dir.path(), "a.png"), "a-1.png");

        std::fs::write(dir.path().join("a-1.png"), "x").unwrap();
        assert_eq!(unique_filename(dir.path(), "a.png"), "a-2.png");
    }

    #[test]
    fn sniff_image_extension_detects_known_formats() {
        assert_eq!(sniff_image_extension(PNG_MAGIC), Some("png"));
        assert_eq!(
            sniff_image_extension(&[0xFF, 0xD8, 0xFF, 0x00]),
            Some("jpg")
        );
        assert_eq!(sniff_image_extension(b"GIF89a..."), Some("gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image_extension(&webp), Some("webp"));
        assert_eq!(sniff_image_extension(b"not an image"), None);
    }

    fn make_post(ctx: &ProjectContext, id: &str) {
        std::fs::write(ctx.content_root.join(id), "---\ntitle: x\n---\nbody").unwrap();
    }

    #[test]
    fn save_image_with_suggested_name_lands_in_stem_dir() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        let rel = save_image(&ctx, "hello.md", Some("cover.png"), PNG_MAGIC)
            .unwrap()
            .image
            .markdown_path;
        assert_eq!(rel, "hello/cover.png");
        assert!(ctx.content_root.join("hello/cover.png").is_file());
    }

    #[test]
    fn nested_post_gets_a_path_relative_to_its_own_directory() {
        let (_dir, ctx) = ctx();
        std::fs::create_dir_all(ctx.content_root.join("nested")).unwrap();
        make_post(&ctx, "nested/hello.md");

        let rel = save_image(&ctx, "nested/hello.md", Some("cover.png"), PNG_MAGIC)
            .unwrap()
            .image
            .markdown_path;
        assert_eq!(rel, "hello/cover.png");
        assert!(ctx.content_root.join("nested/hello/cover.png").is_file());
    }

    #[test]
    fn asset_directory_preserves_post_filename_case() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "Photo_test.md");

        let rel = save_image(&ctx, "Photo_test.md", Some("cover.png"), PNG_MAGIC)
            .unwrap()
            .image
            .markdown_path;
        assert_eq!(rel, "Photo_test/cover.png");
        assert!(ctx.content_root.join("Photo_test/cover.png").is_file());
        assert!(!ctx.content_root.join("photo_test/cover.png").exists());
    }

    #[test]
    fn save_image_without_name_uses_content_hash_and_dedupes_identical_bytes() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        let first = save_image(&ctx, "hello.md", None, PNG_MAGIC).unwrap();
        assert!(first.pending.is_some());
        let rel1 = first.image.markdown_path;
        assert!(rel1.starts_with("hello/"));
        assert!(rel1.ends_with(".png"));

        // 同样的字节再存一次应该落到同一个文件名（内容寻址），不会重复占地方
        let second = save_image(&ctx, "hello.md", None, PNG_MAGIC).unwrap();
        assert!(second.pending.is_none());
        let rel2 = second.image.markdown_path;
        assert_eq!(rel1, rel2);
    }

    #[test]
    fn save_image_suggested_name_conflict_gets_suffixed() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        let rel1 = save_image(&ctx, "hello.md", Some("cover.png"), PNG_MAGIC)
            .unwrap()
            .image
            .markdown_path;
        let rel2 = save_image(&ctx, "hello.md", Some("cover.png"), b"different bytes")
            .unwrap()
            .image
            .markdown_path;
        assert_eq!(rel1, "hello/cover.png");
        assert_eq!(rel2, "hello/cover-1.png");
        // 两个文件都真实存在，第一个没被覆盖
        assert_eq!(
            std::fs::read(ctx.content_root.join("hello/cover.png")).unwrap(),
            PNG_MAGIC
        );
    }

    #[test]
    fn save_image_rejects_missing_or_invalid_post_id() {
        let (_dir, ctx) = ctx();
        assert!(matches!(
            save_image(&ctx, "does-not-exist.md", None, PNG_MAGIC),
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            save_image(&ctx, "../escape.md", None, PNG_MAGIC),
            Err(AppError::InvalidPostId(_))
        ));
    }

    #[test]
    fn save_image_rejects_empty_and_oversized_payloads_before_creating_assets() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        assert!(save_image(&ctx, "hello.md", Some("empty.png"), &[]).is_err());
        let oversized = vec![0u8; MAX_SAVED_IMAGE_BYTES + 1];
        assert!(save_image(&ctx, "hello.md", Some("huge.png"), &oversized).is_err());
        assert!(!ctx.content_root.join("hello").exists());
    }

    #[test]
    fn saved_image_uses_non_executable_file_permissions() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        save_image(&ctx, "hello.md", Some("cover.png"), PNG_MAGIC).unwrap();
        let mode = fs::metadata(ctx.content_root.join("hello/cover.png"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn reads_relative_and_percent_encoded_image_for_preview() {
        let (_dir, ctx) = ctx();
        std::fs::create_dir_all(ctx.content_root.join("nested/post")).unwrap();
        make_post(&ctx, "nested/post.md");
        fs::write(
            ctx.content_root.join("nested/post/cover photo.png"),
            PNG_MAGIC,
        )
        .unwrap();

        let bytes = read_image_asset(
            &ctx,
            "nested/post.md",
            "./post/cover%20photo.png?width=800#hero",
        )
        .unwrap();
        assert_eq!(bytes, PNG_MAGIC);
    }

    #[test]
    fn image_preview_rejects_remote_absolute_and_non_image_paths() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        fs::write(ctx.content_root.join("notes.txt"), b"not an image").unwrap();

        for target in ["https://example.com/a.png", "/a.png", "notes.txt"] {
            assert!(
                read_image_asset(&ctx, "hello.md", target).is_err(),
                "应当拒绝：{target}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn image_preview_rejects_traversal_and_symlink_escape() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.png"), PNG_MAGIC).unwrap();
        std::os::unix::fs::symlink(outside.path(), ctx.content_root.join("linked")).unwrap();

        assert!(read_image_asset(&ctx, "hello.md", "../secret.png").is_err());
        assert!(read_image_asset(&ctx, "hello.md", "linked/secret.png").is_err());
    }

    #[test]
    fn discard_pending_removes_unreferenced_file_and_empty_directory() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let saved = save_image(&ctx, "hello.md", Some("cover.png"), PNG_MAGIC).unwrap();
        let mut pending = PendingAssetManager::default();
        pending.track(saved.pending.unwrap());

        assert_eq!(pending.discard_post(&ctx.root, "hello.md").unwrap(), 1);
        assert!(!ctx.content_root.join("hello/cover.png").exists());
        assert!(!ctx.content_root.join("hello").exists());
    }

    #[test]
    fn discard_pending_preserves_file_referenced_by_saved_markdown() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let saved = save_image(&ctx, "hello.md", Some("cover photo.png"), PNG_MAGIC).unwrap();
        let mut pending = PendingAssetManager::default();
        pending.track(saved.pending.unwrap());
        fs::write(
            ctx.content_root.join("hello.md"),
            "---\ntitle: x\n---\n![](./hello/cover%20photo.png?width=1200#hero)\n",
        )
        .unwrap();

        pending.discard_post(&ctx.root, "hello.md").unwrap();
        assert!(ctx.content_root.join("hello/cover photo.png").is_file());
        assert!(!pending.has_pending(&ctx.root, "hello.md"));
    }

    #[test]
    fn image_reference_scanner_uses_the_shared_markdown_dialect() {
        assert!(markdown_references_image(
            "| image |\n| --- |\n| ![](hello/cover.png) |",
            "hello/cover.png"
        ));
        assert!(!markdown_references_image(
            "$![](hello/cover.png)$",
            "hello/cover.png"
        ));
    }

    #[test]
    fn save_allows_colocated_image_alongside_nested_content_hierarchy() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        make_post(&ctx, "hello/nested.md");

        let saved = save_image(&ctx, "hello.md", Some("cover.png"), PNG_MAGIC).unwrap();
        assert_eq!(saved.image.markdown_path, "hello/cover.png");
        assert!(ctx.content_root.join("hello/cover.png").exists());
        assert!(ctx.content_root.join("hello/nested.md").is_file());
    }

    #[test]
    fn confirming_post_releases_ownership_without_deleting_image() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let saved = save_image(&ctx, "hello.md", None, PNG_MAGIC).unwrap();
        let path = ctx.content_root.join(&saved.image.markdown_path);
        let mut pending = PendingAssetManager::default();
        pending.track(saved.pending.unwrap());

        pending.confirm_post(&ctx.root, "hello.md");
        assert_eq!(pending.discard_post(&ctx.root, "hello.md").unwrap(), 0);
        assert!(path.is_file());
    }
}
