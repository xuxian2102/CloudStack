//! CloudStack 的应用状态机：一次用户操作该怎样协调、状态怎样转换。
//!
//! 不依赖任何 UI 框架（GTK/glib/gio/adw/webkit）——只依赖 `cloudstack-core`
//! 的领域类型。GTK 层负责读取控件、调用这里的纯函数、执行异步任务、把结果
//! 渲染回界面。

pub mod controls;
pub mod drafts;
pub mod git;
pub mod git_refresh;
pub mod preview;
pub mod publish;
pub mod recent;
pub mod save;
pub mod settings;
pub mod workspace;

pub use controls::{capabilities_for, WorkspaceCapabilities, WorkspaceCapabilitiesInput};
pub use git_refresh::should_apply_git_refresh;
pub use preview::{
    PreviewAction, PreviewCompletion, PreviewCoordinator, PreviewRequest, PreviewTicket,
};
pub use save::{apply_successful_save, classify_save_completion, SaveCompletionOutcome};
pub use settings::{SettingsWriter, SettingsWriterAction, VersionedSettings};
