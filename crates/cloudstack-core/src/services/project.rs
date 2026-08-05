use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path};
use std::{fs::Permissions, os::unix::fs::PermissionsExt as _};

use crate::error::AppError;
use crate::model::{FieldSpec, ProjectConfig, ProjectContext, CONFIG_VERSION};

pub const CONFIG_FILE: &str = ".cloudstack.json";
pub const LEGACY_CONFIG_FILE: &str = ".blog-editor.json";
const CONTENT_DIR_SUGGESTIONS: [&str; 4] = ["src/content/blog", "content", "posts", "notes"];

pub fn open_project(root: &Path) -> Result<ProjectContext, AppError> {
    let root = canonical_project_root(root)?;

    let config_path = select_config_path(&root)?;
    let config_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(CONFIG_FILE);
    let raw = fs::read_to_string(&config_path)?;
    let config: ProjectConfig = serde_json::from_str(&raw)
        .map_err(|e| AppError::Config(format!("{config_name} 解析失败：{e}")))?;
    validate_project_config_at(&root, config, config_path)
}

fn select_config_path(root: &Path) -> Result<std::path::PathBuf, AppError> {
    let current = root.join(CONFIG_FILE);
    let legacy = root.join(LEGACY_CONFIG_FILE);
    match (
        is_regular_config_file(&current)?,
        is_regular_config_file(&legacy)?,
    ) {
        (true, false) => Ok(current),
        (false, true) => Ok(legacy),
        (true, true) => Err(AppError::Config(format!(
            "项目同时包含 {CONFIG_FILE} 和 {LEGACY_CONFIG_FILE}，请只保留其中一个"
        ))),
        (false, false) => Err(AppError::MissingProjectConfig),
    }
}

fn is_regular_config_file(path: &Path) -> Result<bool, AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(AppError::Config(format!(
            "配置路径必须是普通文件：{}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::Io(error.to_string())),
    }
}

/// 为首次打开的普通文件夹选择一个不会改变已有目录结构的文章目录。
/// 已有常见目录优先；全都不存在时仅返回建议值，真正创建仍需用户确认。
pub fn suggest_content_dir(root: &Path) -> Result<String, AppError> {
    let root = canonical_project_root(root)?;
    for candidate in CONTENT_DIR_SUGGESTIONS {
        let path = root.join(candidate);
        if path.is_dir()
            && path
                .canonicalize()
                .is_ok_and(|canonical| canonical.starts_with(&root))
        {
            return Ok(candidate.to_string());
        }
    }
    Ok("notes".to_string())
}

/// 在用户确认后创建最小配置和文章目录。不会覆盖新旧任一配置文件。
pub fn initialize_project(
    root: &Path,
    content_dir: &str,
    with_blog_frontmatter: bool,
) -> Result<ProjectContext, AppError> {
    let root = canonical_project_root(root)?;
    match select_config_path(&root) {
        Err(AppError::MissingProjectConfig) => {}
        Ok(path) => {
            return Err(AppError::Config(format!(
                "项目已经存在配置：{}",
                path.display()
            )));
        }
        Err(error) => return Err(error),
    }

    ensure_content_directory(&root, content_dir)?;

    let mut config = ProjectConfig {
        content_dir: content_dir.to_owned(),
        ..ProjectConfig::default()
    };
    if with_blog_frontmatter {
        config.frontmatter.fields = vec![
            FieldSpec {
                name: "title".into(),
                field_type: "string".into(),
                required: true,
                default: None,
            },
            FieldSpec {
                name: "pubDate".into(),
                field_type: "date".into(),
                required: true,
                default: None,
            },
            FieldSpec {
                name: "draft".into(),
                field_type: "boolean".into(),
                required: false,
                default: Some(serde_json::json!(false)),
            },
            FieldSpec {
                name: "tags".into(),
                field_type: "tags".into(),
                required: false,
                default: Some(serde_json::json!([])),
            },
        ];
    }

    // 先用最终内容目录验证完整配置，再以 noclobber 原子写入，防止竞态覆盖。
    let _ = validate_project_config_at(&root, config.clone(), root.join(CONFIG_FILE))?;
    let mut bytes = serde_json::to_vec_pretty(&config)
        .map_err(|error| AppError::Config(format!("配置序列化失败：{error}")))?;
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(0o644))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(root.join(CONFIG_FILE))
        .map_err(|error| AppError::Io(format!("创建 {CONFIG_FILE} 失败：{error}")))?;
    fs::File::open(&root)?.sync_all()?;
    open_project(&root)
}

/// 修复被移动或删除的文章目录；保留配置中的未知字段，只更新 contentDir。
pub fn repair_content_directory(
    root: &Path,
    content_dir: &str,
) -> Result<ProjectContext, AppError> {
    let root = canonical_project_root(root)?;
    let config_path = select_config_path(&root)?;
    let raw = fs::read_to_string(&config_path)?;
    let mut config: ProjectConfig = serde_json::from_str(&raw)
        .map_err(|error| AppError::Config(format!("配置解析失败：{error}")))?;
    let content_root = ensure_content_directory(&root, content_dir)?;
    let context = ProjectContext {
        root,
        content_root,
        config_path,
        config: config.clone(),
    };
    config.content_dir = content_dir.to_owned();
    write_project_config(&context, config)
}

fn ensure_content_directory(
    root: &Path,
    content_dir: &str,
) -> Result<std::path::PathBuf, AppError> {
    let content_path = checked_content_path(root, content_dir)?;
    if content_path.exists() {
        let canonical = content_path.canonicalize()?;
        if !canonical.is_dir() || !canonical.starts_with(root) {
            return Err(AppError::Config(format!("文章目录无效：{content_dir}")));
        }
        return Ok(canonical);
    }

    let mut ancestor = content_path.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| AppError::Config(format!("无法确认文章目录：{content_dir}")))?;
    }
    let canonical_ancestor = ancestor.canonicalize()?;
    if !canonical_ancestor.starts_with(root) {
        return Err(AppError::Config(format!(
            "文章目录不能经过项目外的符号链接：{content_dir}"
        )));
    }
    fs::create_dir_all(&content_path)?;
    let canonical = content_path.canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(AppError::Config(format!("文章目录无效：{content_dir}")));
    }
    Ok(canonical)
}

fn canonical_project_root(root: &Path) -> Result<std::path::PathBuf, AppError> {
    let root = root
        .canonicalize()
        .map_err(|error| AppError::InvalidProject(format!("{}：{error}", root.display())))?;
    if !root.is_dir() {
        return Err(AppError::InvalidProject(format!(
            "不是目录：{}",
            root.display()
        )));
    }
    Ok(root)
}

fn checked_content_path(root: &Path, content_dir: &str) -> Result<std::path::PathBuf, AppError> {
    let content_rel = Path::new(content_dir);
    if content_dir.is_empty()
        || content_rel.is_absolute()
        || !content_rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(AppError::Config(format!("contentDir 非法：{content_dir}")));
    }
    Ok(root.join(content_rel))
}

/// 打开项目与设置保存必须共用同一套校验，避免 UI 能写出下一次启动却打不开的配置。
pub fn validate_project_config(
    root: &Path,
    config: ProjectConfig,
) -> Result<ProjectContext, AppError> {
    validate_project_config_at(root, config, select_config_path(root)?)
}

fn validate_project_config_at(
    root: &Path,
    config: ProjectConfig,
    config_path: std::path::PathBuf,
) -> Result<ProjectContext, AppError> {
    if config_path != root.join(CONFIG_FILE) && config_path != root.join(LEGACY_CONFIG_FILE) {
        return Err(AppError::Config("项目配置路径不受信任".into()));
    }
    if config.version != CONFIG_VERSION {
        return Err(AppError::Config(format!(
            "不支持的配置版本 {}（当前支持 {CONFIG_VERSION}）",
            config.version
        )));
    }

    // contentDir 必须是项目内的相对路径，规则与 PostId 同样严格
    let content_root = checked_content_path(root, &config.content_dir)?
        .canonicalize()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AppError::MissingContentDirectory(config.content_dir.clone())
            } else {
                AppError::Config(format!(
                    "无法读取 contentDir {}：{error}",
                    config.content_dir
                ))
            }
        })?;
    if !content_root.is_dir() || !content_root.starts_with(root) {
        return Err(AppError::Config(format!(
            "contentDir 非法：{}",
            config.content_dir
        )));
    }

    validate_extensions(&config.extensions)?;
    validate_frontmatter_fields(&config)?;
    validate_git_preferences(&config)?;

    Ok(ProjectContext {
        root: root.to_path_buf(),
        content_root,
        config_path,
        config,
    })
}

/// 保留配置里本版本不认识的键，只覆盖当前版本负责的结构化字段；这样插件或未来版本
/// 写入的扩展配置不会因为用户打开一次设置面板就被静默删除。
pub fn write_project_config(
    context: &ProjectContext,
    config: ProjectConfig,
) -> Result<ProjectContext, AppError> {
    let root = &context.root;
    let config_path = &context.config_path;
    let _ = validate_project_config_at(root, config.clone(), config_path.clone())?;
    let raw = fs::read_to_string(config_path)?;
    let config_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(CONFIG_FILE);
    let mut merged: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| AppError::Config(format!("{config_name} 解析失败：{error}")))?;
    let updated = serde_json::to_value(&config)
        .map_err(|error| AppError::Config(format!("配置序列化失败：{error}")))?;
    merge_json(&mut merged, updated);
    if let Some(object) = merged.as_object_mut() {
        // 旧 Astro 进程预览已经被原生静态渲染替代。只在用户显式保存设置时
        // 执行迁移；单纯打开旧项目不会修改磁盘。
        object.remove("preview");
    }

    let mut bytes = serde_json::to_vec_pretty(&merged)
        .map_err(|error| AppError::Config(format!("配置序列化失败：{error}")))?;
    bytes.push(b'\n');
    let permissions = fs::metadata(config_path)?.permissions();
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.as_file().set_permissions(permissions)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(config_path)
        .map_err(|error| AppError::Io(error.to_string()))?;
    fs::File::open(root)?.sync_all()?;

    // 从最终落盘内容重新加载，保证返回给 AppState 的上下文和磁盘完全一致。
    open_project(root)
}

fn validate_extensions(extensions: &[String]) -> Result<(), AppError> {
    if extensions.is_empty() {
        return Err(AppError::Config("extensions 至少需要一个扩展名".into()));
    }
    let mut seen = HashSet::new();
    for extension in extensions {
        let Some(suffix) = extension.strip_prefix('.') else {
            return Err(AppError::Config(format!(
                "扩展名必须以 . 开头：{extension}"
            )));
        };
        if suffix.is_empty()
            || !suffix.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(AppError::Config(format!("扩展名非法：{extension}")));
        }
        if !seen.insert(extension.to_ascii_lowercase()) {
            return Err(AppError::Config(format!("扩展名重复：{extension}")));
        }
    }
    Ok(())
}

fn validate_frontmatter_fields(config: &ProjectConfig) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    for field in &config.frontmatter.fields {
        if field.name.is_empty()
            || field.name.trim() != field.name
            || field.name.chars().any(char::is_control)
        {
            return Err(AppError::Config(
                "Frontmatter 字段名不能为空或包含控制字符".into(),
            ));
        }
        if !seen.insert(field.name.as_str()) {
            return Err(AppError::Config(format!(
                "Frontmatter 字段名重复：{}",
                field.name
            )));
        }
        if field.field_type.is_empty() || field.field_type.chars().any(char::is_control) {
            return Err(AppError::Config(format!(
                "Frontmatter 字段类型非法：{}",
                field.name
            )));
        }
        let Some(default) = field.default.as_ref() else {
            continue;
        };
        let valid_default = match field.field_type.as_str() {
            "boolean" => default.is_boolean(),
            "tags" => default
                .as_array()
                .is_some_and(|values| values.iter().all(serde_json::Value::is_string)),
            "date" | "string" => default.is_string(),
            _ => true,
        };
        if !valid_default {
            return Err(AppError::Config(format!(
                "Frontmatter 字段 {} 的默认值与类型 {} 不匹配",
                field.name, field.field_type
            )));
        }
    }
    Ok(())
}

fn validate_git_preferences(config: &ProjectConfig) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    for article in &config.git.excluded_articles {
        let path = Path::new(article);
        let extension = path.extension().and_then(|value| value.to_str());
        let valid_extension = extension.is_some_and(|extension| {
            config
                .extensions
                .iter()
                .any(|allowed| allowed.strip_prefix('.') == Some(extension))
        });
        if article.is_empty()
            || path.is_absolute()
            || !path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || !valid_extension
            || !seen.insert(article)
        {
            return Err(AppError::Config(format!(
                "Git 排除文章标识非法或重复：{article}"
            )));
        }
    }
    Ok(())
}

fn merge_json(existing: &mut serde_json::Value, updated: serde_json::Value) {
    match (existing, updated) {
        (serde_json::Value::Object(existing), serde_json::Value::Object(updated)) => {
            for (key, value) in updated {
                match existing.get_mut(&key) {
                    Some(current) => merge_json(current, value),
                    None => {
                        existing.insert(key, value);
                    }
                }
            }
        }
        (existing, updated) => *existing = updated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssetMode, FieldSpec};
    use serde_json::json;

    fn setup(config_json: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/content/blog")).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), config_json).unwrap();
        dir
    }

    fn setup_legacy(config_json: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/content/blog")).unwrap();
        std::fs::write(dir.path().join(LEGACY_CONFIG_FILE), config_json).unwrap();
        dir
    }

    #[test]
    fn loads_valid_config_with_defaults() {
        let dir = setup(r#"{ "version": 1 }"#);
        let ctx = open_project(dir.path()).unwrap();
        assert_eq!(ctx.config.content_dir, "src/content/blog");
        assert_eq!(ctx.config.extensions, vec![".md".to_string()]);
        assert_eq!(ctx.config.assets.mode, AssetMode::Colocated);
        assert!(ctx.content_root.ends_with("src/content/blog"));
        assert!(ctx.config_path.ends_with(CONFIG_FILE));
    }

    #[test]
    fn loads_legacy_config_without_renaming_it() {
        let dir = setup_legacy(r#"{ "version": 1 }"#);
        let ctx = open_project(dir.path()).unwrap();
        assert!(ctx.config_path.ends_with(LEGACY_CONFIG_FILE));
        assert!(!dir.path().join(CONFIG_FILE).exists());
    }

    #[test]
    fn rejects_projects_with_both_config_names() {
        let dir = setup(r#"{ "version": 1 }"#);
        std::fs::write(dir.path().join(LEGACY_CONFIG_FILE), r#"{ "version": 1 }"#).unwrap();
        let error = open_project(dir.path()).unwrap_err().to_string();
        assert!(error.contains(CONFIG_FILE));
        assert!(error.contains(LEGACY_CONFIG_FILE));
    }

    #[test]
    fn parses_frontmatter_fields() {
        let dir = setup(
            r#"{
              "version": 1,
              "frontmatter": { "fields": [
                { "name": "title", "type": "string", "required": true },
                { "name": "draft", "type": "boolean", "default": false }
              ]}
            }"#,
        );
        let ctx = open_project(dir.path()).unwrap();
        assert_eq!(ctx.config.frontmatter.fields.len(), 2);
        assert_eq!(ctx.config.frontmatter.fields[0].field_type, "string");
        assert!(ctx.config.frontmatter.fields[0].required);
    }

    #[test]
    fn rejects_missing_config_wrong_version_and_bad_content_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            open_project(dir.path()),
            Err(AppError::MissingProjectConfig)
        ));

        let dir = setup(r#"{ "version": 99 }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 1, "contentDir": "/etc" }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 1, "contentDir": "../outside" }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 1, "contentDir": "does/not/exist" }"#);
        assert!(matches!(
            open_project(dir.path()),
            Err(AppError::MissingContentDirectory(path)) if path == "does/not/exist"
        ));
    }

    #[test]
    fn suggests_an_existing_common_content_directory_or_notes() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(suggest_content_dir(empty.path()).unwrap(), "notes");

        let astro = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(astro.path().join("src/content/blog")).unwrap();
        std::fs::create_dir_all(astro.path().join("notes")).unwrap();
        assert_eq!(
            suggest_content_dir(astro.path()).unwrap(),
            "src/content/blog"
        );
    }

    #[test]
    fn initializes_a_minimal_project_without_overwriting_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let context = initialize_project(dir.path(), "notes", false).unwrap();
        assert!(context.content_root.ends_with("notes"));
        assert!(context.config.frontmatter.fields.is_empty());
        assert!(dir.path().join(CONFIG_FILE).is_file());

        let before = fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(initialize_project(dir.path(), "other", false).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap(),
            before
        );
        assert!(!dir.path().join("other").exists());
    }

    #[test]
    fn blog_template_adds_the_optional_frontmatter_fields() {
        let dir = tempfile::tempdir().unwrap();
        let context = initialize_project(dir.path(), "content", true).unwrap();
        let names = context
            .config
            .frontmatter
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["title", "pubDate", "draft", "tags"]);
    }

    #[cfg(unix)]
    #[test]
    fn initialization_rejects_a_content_directory_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("notes")).unwrap();

        assert!(initialize_project(dir.path(), "notes", false).is_err());
        assert!(!dir.path().join(CONFIG_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn opening_and_initialization_reject_config_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), r#"{ "version": 1 }"#).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(CONFIG_FILE)).unwrap();

        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));
        assert!(initialize_project(dir.path(), "notes", false).is_err());
        assert!(!dir.path().join("notes").exists());
    }

    #[test]
    fn repairs_a_missing_content_directory_and_preserves_unknown_config() {
        let dir = setup(
            r#"{
              "version": 1,
              "customTool": { "keep": true }
            }"#,
        );
        fs::remove_dir_all(dir.path().join("src/content/blog")).unwrap();
        assert!(matches!(
            open_project(dir.path()),
            Err(AppError::MissingContentDirectory(path)) if path == "src/content/blog"
        ));

        let context = repair_content_directory(dir.path(), "notes").unwrap();
        assert!(context.content_root.ends_with("notes"));
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap())
                .unwrap();
        assert_eq!(value["contentDir"], "notes");
        assert_eq!(value["customTool"]["keep"], true);
    }

    #[test]
    fn rejects_invalid_editable_settings() {
        let dir = setup(r#"{ "version": 1 }"#);
        let valid = open_project(dir.path()).unwrap().config;

        let mut config = valid.clone();
        config.extensions = vec!["md".into()];
        assert!(matches!(
            validate_project_config(dir.path(), config),
            Err(AppError::Config(_))
        ));

        let mut config = valid.clone();
        config.extensions = vec![".md".into(), ".MD".into()];
        assert!(matches!(
            validate_project_config(dir.path(), config),
            Err(AppError::Config(_))
        ));

        let mut config = valid.clone();
        config.frontmatter.fields = vec![
            FieldSpec {
                name: "title".into(),
                field_type: "string".into(),
                required: true,
                default: None,
            },
            FieldSpec {
                name: "title".into(),
                field_type: "boolean".into(),
                required: false,
                default: Some(json!("not a boolean")),
            },
        ];
        assert!(matches!(
            validate_project_config(dir.path(), config),
            Err(AppError::Config(_))
        ));

        let mut config = valid;
        config.frontmatter.fields = vec![FieldSpec {
            name: "draft".into(),
            field_type: "boolean".into(),
            required: false,
            default: Some(json!("not a boolean")),
        }];
        assert!(matches!(
            validate_project_config(dir.path(), config),
            Err(AppError::Config(_))
        ));
    }

    #[test]
    fn writes_atomically_and_preserves_unknown_config_keys() {
        let dir = setup(
            r#"{
              "version": 1,
              "customTool": { "keep": true },
              "preview": {
                "port": 4321,
                "customPreview": 7
              }
            }"#,
        );
        let path = dir.path().join(CONFIG_FILE);
        let before_open = fs::read_to_string(&path).unwrap();
        let context = open_project(dir.path()).unwrap();
        let mut config = context.config.clone();
        assert_eq!(fs::read_to_string(&path).unwrap(), before_open);
        config.extensions = vec![".md".into(), ".markdown".into()];

        let updated = write_project_config(&context, config).unwrap();
        assert_eq!(updated.config.extensions, vec![".md", ".markdown"]);

        let raw = fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(raw.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["customTool"]["keep"], json!(true));
        assert!(value.get("preview").is_none());
    }

    #[test]
    fn writes_legacy_config_back_to_its_original_path() {
        let dir = setup_legacy(r#"{ "version": 1, "customTool": true }"#);
        let context = open_project(dir.path()).unwrap();
        let mut config = context.config.clone();
        config.extensions.push(".markdown".into());

        let updated = write_project_config(&context, config).unwrap();

        assert!(updated.config_path.ends_with(LEGACY_CONFIG_FILE));
        assert!(!dir.path().join(CONFIG_FILE).exists());
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(LEGACY_CONFIG_FILE)).unwrap())
                .unwrap();
        assert_eq!(value["customTool"], json!(true));
    }

    #[test]
    fn invalid_update_never_changes_the_config_file() {
        let dir = setup(r#"{ "version": 1, "customTool": "keep" }"#);
        let path = dir.path().join(CONFIG_FILE);
        let before = fs::read_to_string(&path).unwrap();
        let context = open_project(dir.path()).unwrap();
        let mut config = context.config.clone();
        config.extensions = vec!["md".into()];

        assert!(matches!(
            write_project_config(&context, config),
            Err(AppError::Config(_))
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), before);
    }

    #[test]
    fn write_rejects_a_context_with_an_untrusted_config_path() {
        let dir = setup(r#"{ "version": 1 }"#);
        let mut context = open_project(dir.path()).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        context.config_path = outside.path().to_path_buf();

        assert!(matches!(
            write_project_config(&context, context.config.clone()),
            Err(AppError::Config(_))
        ));
    }
}
