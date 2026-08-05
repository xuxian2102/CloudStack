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
    #[serde(default)]
    pub git: GitPreferences,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            content_dir: default_content_dir(),
            extensions: default_extensions(),
            frontmatter: FrontmatterConfig::default(),
            assets: AssetsConfig::default(),
            git: GitPreferences::default(),
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPreferences {
    /// 文章相对 contentDir 的 ID；配置文件是本地状态，本清单不会被 CloudStack 提交。
    #[serde(default)]
    pub excluded_articles: Vec<String>,
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
    /// 当前项目实际使用的配置文件；可能是新名称，也可能是兼容保留的旧名称。
    pub config_path: PathBuf,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryTopology {
    NotInitialized,
    ParentRepository,
    NoCommit,
    NoRemote,
    NoUpstream,
    Tracking,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncRelation {
    Unknown,
    Synced,
    Ahead,
    Behind,
    Diverged,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeState {
    pub managed_changes: usize,
    pub unmanaged_changes: usize,
    pub staged_changes: usize,
    pub has_conflicts: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitEnvironment {
    pub git_available: bool,
    pub gh_available: bool,
    pub gh_authenticated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemote {
    pub name: String,
    /// 已脱敏；URL userinfo 永远不会进入公开状态模型。
    pub url: Option<String>,
}

impl WorktreeState {
    pub fn is_clean(self) -> bool {
        self.managed_changes == 0 && self.unmanaged_changes == 0 && !self.has_conflicts
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySnapshot {
    pub environment: GitEnvironment,
    pub identity: Option<GitIdentity>,
    pub topology: RepositoryTopology,
    pub sync: SyncRelation,
    pub worktree: WorktreeState,
    pub remotes: Vec<GitRemote>,
    pub config_tracked: bool,
    pub status: GitStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandTrace {
    /// 已脱敏、可直接展示的命令行。
    pub command: String,
    /// stdout/stderr 在进入此结构前已经完成脱敏。
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationReport {
    pub traces: Vec<CommandTrace>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitOperationResult {
    pub report: OperationReport,
    pub error: Option<String>,
}

impl GitOperationResult {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryVisibility {
    Private,
    Public,
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
    pub report: OperationReport,
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
            report: Default::default(),
        })
        .unwrap();

        assert_eq!(result["errorStage"], "stage");
        assert_eq!(result["error"]["code"], "git_nothing_to_commit");
        assert!(result.get("message").is_none());
    }
}
