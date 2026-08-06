use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Tag};
use sha2::{Digest, Sha256};

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

    /// 保存成功后调用：这篇文章这次保存正文里仍引用的图片停止追踪（文件保留，
    /// 交还文件系统管理）；不再引用、且内容还是当初粘贴那份的图片按规则删除；
    /// 不再引用但内容已经被外部程序改过的，跟 `cleanup_pending_asset` 一样只
    /// 解除追踪、不删文件——不能因为文章保存了就默认图片还是自己的。删除失败
    /// 保留记录，交给下一次保存/切换项目/关闭项目重试。
    pub fn reconcile_saved_post(
        &mut self,
        project_root: &Path,
        post_id: &str,
        saved_body: &str,
    ) -> Result<usize, AppError> {
        let mut remaining = Vec::with_capacity(self.entries.len());
        let mut cleaned = 0;
        let mut first_error = None;

        for entry in std::mem::take(&mut self.entries) {
            if entry.project_root != project_root || entry.post_id != post_id {
                remaining.push(entry);
                continue;
            }
            if markdown_references_image(saved_body, &entry.markdown_path) {
                // 已经进入这次保存的版本，交给文件系统管理，不再追踪。
                continue;
            }
            let current_path = match verify_pending_file_path(&entry) {
                Ok(Some(path)) => path,
                Ok(None) => {
                    cleaned += 1; // 文件已经不存在，没什么可清理的
                    continue;
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    remaining.push(entry);
                    continue;
                }
            };
            let outcome = match content_matches_pending_revision(&entry, &current_path) {
                Ok(true) => delete_pending_file(&entry, &current_path),
                Ok(false) => Ok(()), // 内容被外部改过，保留文件，只解除追踪
                Err(error) => Err(error),
            };
            match outcome {
                Ok(()) => cleaned += 1,
                Err(error) => {
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

/// 候选文件名的扩展名跟识别出的内容格式对不上时改写成规范扩展名；已经匹配
/// （包括 `jpg`/`jpeg`、`tif`/`tiff` 这类等价拼法）就保留用户原始写法。
fn reconcile_extension(name: &str, format: SupportedImageFormat) -> String {
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let matches = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| format.matches_extension(extension));
    if matches {
        name.to_owned()
    } else {
        format!("{stem}.{}", format.canonical_extension())
    }
}

/// CloudStack 保存和预览都认识的图片格式清单——导入、预览、GTK 资源协议的
/// content-type 都从这一份枚举派生，不再各自维护一份扩展名列表。
///
/// 有意不用 `infer::get(..).mime_type().starts_with("image/")` 这种前缀匹配：
/// `infer` 认识的 "image/*" 里还包含 PSD、CR2、HEIF、JXL 等 CloudStack 从来
/// 不能预览的格式，前缀匹配会让这些字节被当作"是图片"接受下来，存成一个自己
/// 永远打不开的文件。这里逐个列出 CloudStack 实际支持渲染的格式。
///
/// 不支持 SVG：SVG 是 XML 文档而不是二进制图片，`infer` 没有固定魔数可以嗅探，
/// 只能靠字符串搜索 `<svg` 这种不可靠的手段“认出”它，验证强度和其余格式的签名
/// 校验不在一个量级。以后如果要重新支持，推荐落地时用受控渲染器直接栅格化成
/// PNG/WebP 存下来，而不是长期保留一个活体 SVG 解析/渲染的攻击面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupportedImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Avif,
    Bmp,
    Tiff,
    Ico,
}

impl SupportedImageFormat {
    /// 只看文件签名，不解码图片内容。
    fn sniff(bytes: &[u8]) -> Option<Self> {
        match infer::get(bytes)?.mime_type() {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::WebP),
            "image/avif" => Some(Self::Avif),
            "image/bmp" => Some(Self::Bmp),
            "image/tiff" => Some(Self::Tiff),
            "image/vnd.microsoft.icon" => Some(Self::Ico),
            _ => None,
        }
    }

    fn canonical_extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::WebP => "webp",
            Self::Avif => "avif",
            Self::Bmp => "bmp",
            Self::Tiff => "tif",
            Self::Ico => "ico",
        }
    }

    /// 同一格式常见的多种拼法（`jpg`/`jpeg`、`tif`/`tiff`）都算匹配，避免把用户
    /// 原本就写对的扩展名强行改写成 `canonical_extension` 的那一种拼法。
    fn matches_extension(self, extension: &str) -> bool {
        let extension = extension.to_ascii_lowercase();
        match self {
            Self::Jpeg => extension == "jpg" || extension == "jpeg",
            Self::Tiff => extension == "tif" || extension == "tiff",
            other => extension == other.canonical_extension(),
        }
    }

    fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
            Self::Avif => "image/avif",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Ico => "image/x-icon",
        }
    }
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
    // 内容必须能被识别成受支持的图片格式才继续；跟大小/空检查放在一起，在创建
    // 任何目录或文件之前就拒绝，不留下半个空的资产目录。
    let format = SupportedImageFormat::sniff(bytes)
        .ok_or_else(|| AppError::Io("无法识别图片格式，拒绝保存".into()))?;
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
        // 有真实文件名（拖拽）：扩展名跟识别出的内容对不上时改名而不是报错（拖拽
        // 来源有时会给错或缺失扩展名），不同内容撞同名是正常情况，交给
        // unique_filename 加后缀。
        Some(name) => {
            let reconciled = reconcile_extension(&name, format);
            (unique_filename(&asset_dir, &reconciled), true)
        }
        // 没有文件名（剪贴板粘贴）：内容寻址命名，同样的字节天然映射到同一个文件名，
        // 已存在就直接复用（不重复占地方），不能走 unique_filename 那套"名字冲突就加后缀"的逻辑。
        None => {
            let desired = format!(
                "{}.{}",
                &revision_of(bytes)[..8],
                format.canonical_extension()
            );
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

/// 读到的字节数超过 `maximum` 时返回 `Ok(None)`，不把超限文件整个读进内存——
/// 调用方决定超限时报什么错误；`fs::metadata` 查大小、再 `fs::read` 整个文件
/// 这两步之间文件可能被外部替换/扩大，用 `Read::take` 从一开始就限制读取量
/// 能关掉这个 TOCTOU 窗口。
fn read_bounded_file(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, AppError> {
    let file = fs::File::open(path)?;
    let mut limited = file.take(maximum + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Ok(None);
    }
    Ok(Some(bytes))
}

pub struct ImageAsset {
    pub bytes: Vec<u8>,
    /// 由实际读到的字节内容识别得出，不是从 URL/文件名后缀猜测——文件名后缀
    /// 和真实内容不一致时（理论上不该发生，但防御性地按内容为准）也能返回
    /// 正确的 HTTP Content-Type。
    pub content_type: &'static str,
}

/// 读取当前文章引用的本地图片。只接受相对文章目录的 URL，并在 percent decode 与
/// canonicalize 后再次确认仍位于 content_root；不为前端开放通用文件读取能力。
/// 是否能预览完全由内容嗅探决定，不再有单独的扩展名白名单——包括不再接受 SVG。
pub fn read_image_asset(
    ctx: &ProjectContext,
    post_id: &str,
    markdown_path: &str,
) -> Result<ImageAsset, AppError> {
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

    let bytes = read_bounded_file(&canonical, MAX_PREVIEW_IMAGE_BYTES)?
        .ok_or_else(|| AppError::Io(format!("图片超过实时预览的 25 MiB 限制：{markdown_path}")))?;
    let format = SupportedImageFormat::sniff(&bytes)
        .ok_or_else(|| AppError::Io("该文件不是受支持的图片格式，无法预览".into()))?;
    Ok(ImageAsset {
        bytes,
        content_type: format.mime_type(),
    })
}

fn remove_dir_if_empty(dir: &Path) {
    if dir.is_dir() && fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(dir);
    }
}

/// 校验 entry.file_path 没被替换成别的东西；文件已经不存在时返回 `Ok(None)`
/// （调用方应当当作"没什么可清理的"，不是错误），路径发生可疑变化时返回 `Err`。
fn verify_pending_file_path(entry: &PendingAsset) -> Result<Option<PathBuf>, AppError> {
    if !entry.file_path.exists() {
        return Ok(None);
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
    Ok(Some(current_path))
}

/// 实际删除文件 + 清理空目录，调用方已经确认过路径安全、且已经决定要删除。
fn delete_pending_file(entry: &PendingAsset, current_path: &Path) -> Result<(), AppError> {
    fs::remove_file(current_path)?;
    if entry.asset_dir.is_dir() && fs::read_dir(&entry.asset_dir)?.next().is_none() {
        fs::remove_dir(&entry.asset_dir)?;
    }
    Ok(())
}

/// 有界流式计算文件内容 SHA-256，不整体读入内存；超过 `maximum` 直接返回
/// `Ok(None)`——调用方应当把它当作"内容已经被外部改变"处理，不能为了清理一个
/// pending 图片就去读一个可能被换成几 GB 大文件。
fn bounded_revision(path: &Path, maximum: u64) -> Result<Option<String>, AppError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    let mut total: u64 = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > maximum {
            return Ok(None);
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

/// `entry.revision` 是粘贴/拖拽那一刻的图片内容哈希，跟文章自己的 revision
/// 无关——文章保存不会让它失效。这里读盘比较当前内容是否还是同一份；不一致
/// （含读取途中发现文件已经超限的情况）说明文件已经被外部程序改过，不再归
/// 这次粘贴事务所有。
fn content_matches_pending_revision(
    entry: &PendingAsset,
    current_path: &Path,
) -> Result<bool, AppError> {
    let revision = bounded_revision(current_path, MAX_SAVED_IMAGE_BYTES as u64)?;
    Ok(revision.as_deref() == Some(entry.revision.as_str()))
}

fn cleanup_pending_asset(entry: &PendingAsset) -> Result<(), AppError> {
    let Some(current_path) = verify_pending_file_path(entry)? else {
        return Ok(());
    };

    if !content_matches_pending_revision(entry, &current_path)? {
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

    delete_pending_file(entry, &current_path)
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
    fn supported_image_format_sniffs_known_formats_by_signature() {
        assert_eq!(
            SupportedImageFormat::sniff(PNG_MAGIC),
            Some(SupportedImageFormat::Png)
        );
        assert_eq!(
            SupportedImageFormat::sniff(&[0xFF, 0xD8, 0xFF, 0x00]),
            Some(SupportedImageFormat::Jpeg)
        );
        assert_eq!(
            SupportedImageFormat::sniff(b"GIF89a..."),
            Some(SupportedImageFormat::Gif)
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(
            SupportedImageFormat::sniff(&webp),
            Some(SupportedImageFormat::WebP)
        );
        assert_eq!(SupportedImageFormat::sniff(b"not an image"), None);
        // SVG 是 XML 文本，没有固定魔数可嗅探——CloudStack 就不支持它。
        assert_eq!(
            SupportedImageFormat::sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
            None
        );
    }

    #[test]
    fn supported_image_format_extension_matching_accepts_equivalent_spellings() {
        assert!(SupportedImageFormat::Jpeg.matches_extension("jpg"));
        assert!(SupportedImageFormat::Jpeg.matches_extension("JPEG"));
        assert!(!SupportedImageFormat::Jpeg.matches_extension("png"));
        assert!(SupportedImageFormat::Tiff.matches_extension("tif"));
        assert!(SupportedImageFormat::Tiff.matches_extension("tiff"));
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

        // 内容必须仍然能被嗅探成受支持的图片格式，否则会先被"无法识别"拒绝而
        // 不是走到"同名冲突加后缀"这条路径；用同一签名但不同负载模拟"不同的
        // PNG 内容"。
        let mut different_png = PNG_MAGIC.to_vec();
        different_png.extend_from_slice(b"different payload");

        let rel1 = save_image(&ctx, "hello.md", Some("cover.png"), PNG_MAGIC)
            .unwrap()
            .image
            .markdown_path;
        let rel2 = save_image(&ctx, "hello.md", Some("cover.png"), &different_png)
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
    fn save_image_rejects_unrecognized_content_even_with_an_image_looking_name() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        assert!(save_image(&ctx, "hello.md", Some("cover.png"), b"not an image").is_err());
        assert!(save_image(&ctx, "hello.md", None, b"not an image").is_err());
        // 拒绝时不留下半个资产目录
        assert!(!ctx.content_root.join("hello").exists());
    }

    #[test]
    fn save_image_corrects_extension_to_match_detected_content() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");

        let jpeg_bytes: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        let rel = save_image(&ctx, "hello.md", Some("cover.png"), jpeg_bytes)
            .unwrap()
            .image
            .markdown_path;
        assert_eq!(rel, "hello/cover.jpg");
        assert!(ctx.content_root.join("hello/cover.jpg").is_file());

        let rel = save_image(&ctx, "hello.md", Some("untitled"), PNG_MAGIC)
            .unwrap()
            .image
            .markdown_path;
        assert_eq!(rel, "hello/untitled.png");
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

        let image = read_image_asset(
            &ctx,
            "nested/post.md",
            "./post/cover%20photo.png?width=800#hero",
        )
        .unwrap();
        assert_eq!(image.bytes, PNG_MAGIC);
        assert_eq!(image.content_type, "image/png");
    }

    #[test]
    fn preview_content_type_comes_from_sniffed_bytes_not_the_file_extension() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        // 文件名后缀是 .png，但实际内容是 JPEG——content_type 应该以真实内容为准。
        let jpeg_bytes: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        fs::write(ctx.content_root.join("hello/cover.png"), jpeg_bytes).unwrap();

        let image = read_image_asset(&ctx, "hello.md", "hello/cover.png").unwrap();
        assert_eq!(image.content_type, "image/jpeg");
    }

    #[test]
    fn preview_rejects_svg_and_other_unrecognized_content() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        fs::write(
            ctx.content_root.join("hello/icon.svg"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
        )
        .unwrap();

        assert!(read_image_asset(&ctx, "hello.md", "hello/icon.svg").is_err());
    }

    #[test]
    fn preview_read_is_bounded_and_does_not_load_oversized_files() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        fs::create_dir_all(ctx.content_root.join("hello")).unwrap();
        let mut oversized = PNG_MAGIC.to_vec();
        oversized.resize(usize::try_from(MAX_PREVIEW_IMAGE_BYTES).unwrap() + 1, 0);
        fs::write(ctx.content_root.join("hello/huge.png"), &oversized).unwrap();

        assert!(read_image_asset(&ctx, "hello.md", "hello/huge.png").is_err());
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

    #[test]
    fn reconcile_saved_post_keeps_file_but_stops_tracking_when_still_referenced() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let saved = save_image(&ctx, "hello.md", Some("a.png"), PNG_MAGIC).unwrap();
        let path = ctx.content_root.join(&saved.image.markdown_path);
        let mut pending = PendingAssetManager::default();
        pending.track(saved.pending.unwrap());

        let saved_body = format!("![]({})", saved.image.markdown_path);
        assert_eq!(
            pending
                .reconcile_saved_post(&ctx.root, "hello.md", &saved_body)
                .unwrap(),
            0
        );
        assert!(path.is_file());
        assert!(!pending.has_pending(&ctx.root, "hello.md"));
    }

    #[test]
    fn reconcile_saved_post_deletes_file_when_no_longer_referenced() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let kept = save_image(&ctx, "hello.md", Some("a.png"), PNG_MAGIC).unwrap();
        let dropped = save_image(&ctx, "hello.md", Some("b.png"), PNG_MAGIC).unwrap();
        let kept_path = ctx.content_root.join(&kept.image.markdown_path);
        let dropped_path = ctx.content_root.join(&dropped.image.markdown_path);
        let mut pending = PendingAssetManager::default();
        pending.track(kept.pending.unwrap());
        pending.track(dropped.pending.unwrap());

        // 保存后的正文只还引用 a.png，b.png 的引用已经被删掉了。
        let saved_body = format!("![]({})", kept.image.markdown_path);
        assert_eq!(
            pending
                .reconcile_saved_post(&ctx.root, "hello.md", &saved_body)
                .unwrap(),
            1
        );
        assert!(kept_path.is_file());
        assert!(!dropped_path.exists());
        assert!(!pending.has_pending(&ctx.root, "hello.md"));
    }

    #[test]
    fn reconcile_saved_post_retains_entry_when_asset_cannot_be_read() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let dropped = save_image(&ctx, "hello.md", Some("b.png"), PNG_MAGIC).unwrap();
        let asset_dir = ctx.content_root.join("hello");
        let file_path = ctx.content_root.join(&dropped.image.markdown_path);
        let mut pending = PendingAssetManager::default();
        pending.track(dropped.pending.unwrap());

        // 用同名目录替换图片文件。读取目录会稳定返回错误，不依赖测试进程是否
        // 以 root 运行，也不会因为删除成功后清理了父目录而影响测试收尾。
        fs::remove_file(&file_path).unwrap();
        fs::create_dir(&file_path).unwrap();

        let result = pending.reconcile_saved_post(&ctx.root, "hello.md", "");

        assert!(result.is_err());
        assert!(pending.has_pending(&ctx.root, "hello.md"));
        fs::remove_dir(&file_path).unwrap();
        assert!(asset_dir.is_dir());
    }

    #[test]
    fn reconcile_saved_post_ignores_entries_for_other_posts() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        make_post(&ctx, "other.md");
        let this_post = save_image(&ctx, "hello.md", Some("a.png"), PNG_MAGIC).unwrap();
        let other_post = save_image(&ctx, "other.md", Some("b.png"), PNG_MAGIC).unwrap();
        let mut pending = PendingAssetManager::default();
        pending.track(this_post.pending.unwrap());
        pending.track(other_post.pending.unwrap());

        // 保存的正文完全不引用任何图片；只有 hello.md 名下的条目应该被处理。
        assert_eq!(
            pending
                .reconcile_saved_post(&ctx.root, "hello.md", "no images here")
                .unwrap(),
            1
        );
        assert!(!pending.has_pending(&ctx.root, "hello.md"));
        assert!(pending.has_pending(&ctx.root, "other.md"));
    }

    #[test]
    fn reconcile_saved_post_ignores_entries_for_other_projects() {
        let (_dir_a, ctx_a) = ctx();
        let (_dir_b, ctx_b) = ctx();
        make_post(&ctx_a, "hello.md");
        make_post(&ctx_b, "hello.md");
        let entry_a = save_image(&ctx_a, "hello.md", Some("a.png"), PNG_MAGIC).unwrap();
        let entry_b = save_image(&ctx_b, "hello.md", Some("b.png"), PNG_MAGIC).unwrap();
        let mut pending = PendingAssetManager::default();
        pending.track(entry_a.pending.unwrap());
        pending.track(entry_b.pending.unwrap());

        // 两个项目都有同名文章 hello.md；只应该处理 ctx_a 名下的条目，
        // 证明过滤条件真的是 project_root，不只是 post_id 恰好没撞上。
        assert_eq!(
            pending
                .reconcile_saved_post(&ctx_a.root, "hello.md", "no images here")
                .unwrap(),
            1
        );
        assert!(!pending.has_pending(&ctx_a.root, "hello.md"));
        assert!(pending.has_pending(&ctx_b.root, "hello.md"));
    }

    #[test]
    fn reconcile_saved_post_releases_without_deleting_when_content_was_modified_externally() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let dropped = save_image(&ctx, "hello.md", Some("b.png"), PNG_MAGIC).unwrap();
        let path = ctx.content_root.join(&dropped.image.markdown_path);
        let mut pending = PendingAssetManager::default();
        pending.track(dropped.pending.unwrap());

        // 模拟外部程序在保存前把这张图片的内容换掉了。
        fs::write(&path, b"modified by another program").unwrap();

        // 保存后的正文不再引用这张图，但内容已经不是当初粘贴的那份，不能删。
        assert_eq!(
            pending
                .reconcile_saved_post(&ctx.root, "hello.md", "")
                .unwrap(),
            1
        );
        assert!(path.is_file(), "内容被外部改过的文件不应该被删除");
        assert!(!pending.has_pending(&ctx.root, "hello.md"));
    }

    #[test]
    fn reconcile_saved_post_does_not_hash_an_externally_oversized_pending_file() {
        let (_dir, ctx) = ctx();
        make_post(&ctx, "hello.md");
        let dropped = save_image(&ctx, "hello.md", Some("b.png"), PNG_MAGIC).unwrap();
        let path = ctx.content_root.join(&dropped.image.markdown_path);
        let mut pending = PendingAssetManager::default();
        pending.track(dropped.pending.unwrap());

        // 模拟外部程序把这张图片换成了一个超过保存上限的巨大文件：不能为了清理
        // 一次 pending 图片就把它整个读进内存来算哈希。
        let oversized = vec![0u8; MAX_SAVED_IMAGE_BYTES + 1];
        fs::write(&path, &oversized).unwrap();

        assert_eq!(
            pending
                .reconcile_saved_post(&ctx.root, "hello.md", "")
                .unwrap(),
            1
        );
        assert!(path.is_file(), "超限文件不应该被删除");
        assert!(!pending.has_pending(&ctx.root, "hello.md"));
    }
}
