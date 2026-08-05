use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path};

use crate::error::AppError;
use crate::model::{ProjectConfig, ProjectContext, CONFIG_VERSION};

pub const CONFIG_FILE: &str = ".cloudstack.json";
pub const LEGACY_CONFIG_FILE: &str = ".blog-editor.json";

pub fn open_project(root: &Path) -> Result<ProjectContext, AppError> {
    let root = root
        .canonicalize()
        .map_err(|e| AppError::InvalidProject(format!("{}：{e}", root.display())))?;
    if !root.is_dir() {
        return Err(AppError::InvalidProject(format!(
            "不是目录：{}",
            root.display()
        )));
    }

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
    match (current.is_file(), legacy.is_file()) {
        (true, false) => Ok(current),
        (false, true) => Ok(legacy),
        (true, true) => Err(AppError::Config(format!(
            "项目同时包含 {CONFIG_FILE} 和 {LEGACY_CONFIG_FILE}，请只保留其中一个"
        ))),
        (false, false) => Err(AppError::Config(format!(
            "项目根目录缺少 {CONFIG_FILE}（也未找到兼容配置 {LEGACY_CONFIG_FILE}）"
        ))),
    }
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
    let content_rel = Path::new(&config.content_dir);
    if content_rel.is_absolute()
        || !content_rel
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(AppError::Config(format!(
            "contentDir 非法：{}",
            config.content_dir
        )));
    }
    let content_root = root
        .join(content_rel)
        .canonicalize()
        .map_err(|_| AppError::Config(format!("contentDir 不存在：{}", config.content_dir)))?;
    if !content_root.is_dir() || !content_root.starts_with(root) {
        return Err(AppError::Config(format!(
            "contentDir 非法：{}",
            config.content_dir
        )));
    }

    validate_extensions(&config.extensions)?;
    validate_frontmatter_fields(&config)?;

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
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 99 }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 1, "contentDir": "/etc" }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 1, "contentDir": "../outside" }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));

        let dir = setup(r#"{ "version": 1, "contentDir": "does/not/exist" }"#);
        assert!(matches!(open_project(dir.path()), Err(AppError::Config(_))));
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
