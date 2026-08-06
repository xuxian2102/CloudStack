//! CloudStack 的领域核心。
//!
//! 本 crate 不依赖 GTK、Tauri 或任何窗口系统。UI 只持有 [`ProjectContext`]，
//! 并通过这里的服务完成路径校验、原子保存、图片事务、草稿与 Git 操作。

pub mod error;
pub mod model;
pub mod path_guard;

pub mod services {
    pub mod assets;
    pub mod drafts;
    pub mod frontmatter;
    pub mod git;
    pub mod markdown;
    pub mod posts;
    pub mod project;
    pub mod recent;
    pub mod settings;
}

pub use error::AppError;
pub use model::{PostDocument, PostSummary, ProjectConfig, ProjectContext};
