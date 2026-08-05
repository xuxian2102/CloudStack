use crate::error::ErrorPayload;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostSummary {
    pub id: String,
    pub relative_path: String,
    pub modified_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostDocument {
    pub id: String,
    pub relative_path: String,
    /// frontmatter 原始 YAML 文本（不含 `---` 分隔线）；None 表示文件没有 frontmatter 块
    pub raw_frontmatter: Option<String>,
    pub body: String,
    /// 整个文件字节的 SHA-256，保存时回传用于检测外部修改
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftDocument {
    pub post_id: String,
    pub raw_frontmatter: Option<String>,
    pub body: String,
    pub base_revision: String,
    pub saved_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedImage {
    /// 可以直接写入 Markdown 图片目标的、相对文章文件的路径。
    pub markdown_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    pub version: u32,
    #[serde(default = "default_content_dir")]
    pub content_dir: String,
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub frontmatter: FrontmatterConfig,
    #[serde(default)]
    pub assets: AssetsConfig,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            content_dir: default_content_dir(),
            extensions: default_extensions(),
            frontmatter: FrontmatterConfig::default(),
            assets: AssetsConfig::default(),
        }
    }
}

fn default_content_dir() -> String {
    "src/content/blog".into()
}

fn default_extensions() -> Vec<String> {
    vec![".md".into()]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrontmatterConfig {
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetsConfig {
    #[serde(default)]
    pub mode: AssetMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetMode {
    /// 图片放在文章同目录下、与文章 stem 同名的子目录中。
    #[default]
    Colocated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

/// 只存在 Rust 侧的项目上下文；root/content_root 均已 canonicalize
#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub content_root: PathBuf,
    pub config: ProjectConfig,
}

/// 给前端展示用的项目信息（root 仅用于显示，前端永远不回传路径）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub root: String,
    pub generation: u64,
    pub config: ProjectConfig,
}

impl ProjectContext {
    pub fn info(&self, generation: u64) -> ProjectInfo {
        ProjectInfo {
            root: self.root.display().to_string(),
            generation,
            config: self.config.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Unmerged,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// 相对 project root 的路径
    pub path: String,
    /// 仅重命名时存在
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    /// 变更是否已在 git 索引里（相对 HEAD）
    pub staged: bool,
    /// 是否落在 content_dir 内——只有这些会被 publish 暂存
    pub managed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    pub staged: bool,
    pub staged_files: Vec<String>,
    pub committed: bool,
    pub commit_hash: Option<String>,
    pub pushed: bool,
    /// "stage" | "commit" | "push"，None 表示全部成功（或未尝试推送）
    pub error_stage: Option<String>,
    pub error: Option<ErrorPayload>,
}

#[cfg(test)]
mod tests {
    use super::PublishResult;
    use crate::error::ErrorPayload;

    #[test]
    fn publish_result_uses_structured_error_payload() {
        let result = serde_json::to_value(PublishResult {
            staged: false,
            staged_files: vec![],
            committed: false,
            commit_hash: None,
            pushed: false,
            error_stage: Some("stage".into()),
            error: Some(ErrorPayload::git_nothing_to_commit("没有可提交的改动")),
        })
        .unwrap();

        assert_eq!(result["errorStage"], "stage");
        assert_eq!(result["error"]["code"], "git_nothing_to_commit");
        assert!(result.get("message").is_none());
    }
}
