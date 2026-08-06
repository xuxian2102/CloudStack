//! 整体控件权限模型：给定当前工作区状态，一次性算出每个控件该不该可用。
//! 跟 GTK 的 `EditorState` 具体形状解耦——只依赖这里定义的输入快照，方便
//! 集中测试 busy/document/dirty 对控件可用性的影响，不需要真的搭一个 GTK
//! 窗口。GTK 层只负责把 `WorkspaceCapabilities` 里的布尔值写回控件。

use cloudstack_core::model::RepositorySnapshot;

use crate::git::{effective_action, EffectiveGitAction};

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceCapabilitiesInput<'a> {
    pub has_project: bool,
    pub has_document: bool,
    pub unsaved_document_count: usize,
    pub busy: bool,
    pub dirty: bool,
    pub git_snapshot: Option<&'a RepositorySnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCapabilities {
    /// 打开项目入口（顶栏按钮 + 欢迎页），有未保存文章或正忙时都要挡住。
    pub open_enabled: bool,
    pub home_enabled: bool,
    pub new_post_enabled: bool,
    pub rename_enabled: bool,
    pub delete_enabled: bool,
    pub save_enabled: bool,
    pub editor_editable: bool,
    pub editor_cursor_visible: bool,
    pub frontmatter_panel_enabled: bool,
    pub properties_enabled: bool,
    pub git_project_available: bool,
    pub git_dirty: bool,
    pub git_primary_action: EffectiveGitAction,
    pub post_list_enabled: bool,
    /// 只在没有文章展示时才强制收起 frontmatter 侧栏；文章重新出现时不会
    /// 由这里负责重新展开（那是 toggle-properties 动作自己的事）。
    pub hide_frontmatter_sidebar: bool,
}

pub fn capabilities_for(input: WorkspaceCapabilitiesInput<'_>) -> WorkspaceCapabilities {
    // 有未保存文章挂在内存里时，跳项目/新建文章这类会离开当前上下文的
    // 操作要挡住；已经在编辑的文档本身（保存、frontmatter、post_list）
    // 只看 busy，不受这条限制。
    let stable = !input.busy && input.unsaved_document_count == 0;
    WorkspaceCapabilities {
        open_enabled: stable,
        home_enabled: input.has_project && stable,
        new_post_enabled: input.has_project && stable,
        rename_enabled: input.has_document && stable,
        delete_enabled: input.has_document && stable,
        save_enabled: input.has_document && input.dirty && !input.busy,
        editor_editable: input.has_document && !input.busy,
        editor_cursor_visible: input.has_document,
        frontmatter_panel_enabled: input.has_document && !input.busy,
        properties_enabled: input.has_document && !input.busy,
        git_project_available: input.has_project,
        git_dirty: input.dirty,
        git_primary_action: effective_action(
            input.git_snapshot,
            input.busy,
            input.unsaved_document_count,
        ),
        post_list_enabled: input.has_project && !input.busy,
        hide_frontmatter_sidebar: !input.has_document,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        has_project: bool,
        has_document: bool,
        unsaved_document_count: usize,
        busy: bool,
        dirty: bool,
    ) -> WorkspaceCapabilitiesInput<'static> {
        WorkspaceCapabilitiesInput {
            has_project,
            has_document,
            unsaved_document_count,
            busy,
            dirty,
            git_snapshot: None,
        }
    }

    #[test]
    fn controls_for_default_state_only_allows_opening() {
        let model = capabilities_for(input(false, false, 0, false, false));
        assert!(model.open_enabled);
        assert!(!model.home_enabled);
        assert!(!model.new_post_enabled);
        assert!(!model.rename_enabled);
        assert!(!model.delete_enabled);
        assert!(!model.save_enabled);
        assert!(!model.editor_editable);
        assert!(!model.editor_cursor_visible);
        assert!(!model.frontmatter_panel_enabled);
        assert!(!model.properties_enabled);
        assert!(!model.git_project_available);
        assert_eq!(model.git_primary_action, EffectiveGitAction::None);
        assert!(!model.post_list_enabled);
        assert!(model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_project_without_document_enables_project_scoped_controls_only() {
        let model = capabilities_for(input(true, false, 0, false, false));
        assert!(model.home_enabled);
        assert!(model.new_post_enabled);
        assert!(!model.rename_enabled, "没有打开的文章不该允许重命名");
        assert!(!model.delete_enabled, "没有打开的文章不该允许删除");
        assert!(!model.save_enabled);
        assert!(!model.editor_editable);
        assert!(model.git_project_available);
        assert_eq!(model.git_primary_action, EffectiveGitAction::None);
        assert!(model.post_list_enabled);
        assert!(model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_dirty_document_enables_save() {
        let model = capabilities_for(input(true, true, 0, false, true));
        assert!(model.save_enabled);
        assert!(model.editor_editable);
        assert!(model.editor_cursor_visible);
        assert!(model.frontmatter_panel_enabled);
        assert!(model.properties_enabled);
        assert!(model.rename_enabled);
        assert!(model.delete_enabled);
        assert!(model.git_dirty);
        assert!(!model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_clean_document_disables_save_only() {
        let model = capabilities_for(input(true, true, 0, false, false));
        assert!(!model.save_enabled);
        assert!(model.editor_editable, "干净的文档仍然可以继续编辑");
    }

    #[test]
    fn controls_for_busy_disables_actionable_controls_but_not_read_only_ones() {
        let model = capabilities_for(input(true, true, 0, true, true));
        assert!(!model.open_enabled);
        assert!(!model.home_enabled);
        assert!(!model.new_post_enabled);
        assert!(!model.rename_enabled);
        assert!(!model.delete_enabled);
        assert!(!model.save_enabled);
        assert!(!model.editor_editable);
        assert!(!model.frontmatter_panel_enabled);
        assert!(!model.properties_enabled);
        assert_eq!(model.git_primary_action, EffectiveGitAction::None);
        assert!(!model.post_list_enabled);
        // busy 只挡"会触发新操作"的控件；纯展示性的判断不受它影响。
        assert!(
            model.editor_cursor_visible,
            "光标可见性只看有没有文档，跟 busy 无关"
        );
        assert!(
            model.git_project_available,
            "git 面板是否可用只看有没有项目，跟 busy 无关"
        );
        assert!(!model.hide_frontmatter_sidebar);
    }

    #[test]
    fn controls_for_unsaved_documents_blocks_navigation_but_not_the_open_document() {
        let model = capabilities_for(input(true, true, 1, false, true));
        // 有其它未保存的文章挂着：会离开当前上下文的操作要挡住……
        assert!(!model.open_enabled);
        assert!(!model.home_enabled);
        assert!(!model.new_post_enabled);
        assert!(!model.rename_enabled);
        assert!(!model.delete_enabled);
        // ……但当前正在编辑的这篇文章本身不受影响，不是 busy。
        assert!(model.save_enabled);
        assert!(model.editor_editable);
        assert!(model.frontmatter_panel_enabled);
        assert!(model.post_list_enabled);
        assert_eq!(
            model.git_primary_action,
            EffectiveGitAction::SaveBeforeGit { unsaved_count: 1 }
        );
    }

    #[test]
    fn controls_for_clean_current_document_keeps_other_unsaved_state_separate() {
        let model = capabilities_for(input(true, true, 1, false, false));
        assert!(
            !model.save_enabled,
            "当前文章干净时不应显示当前文章保存按钮"
        );
        assert!(!model.git_dirty, "Git 面板的未保存标记只反映当前文章");
        assert!(model.editor_editable);
        assert!(model.post_list_enabled);
        assert_eq!(
            model.git_primary_action,
            EffectiveGitAction::SaveBeforeGit { unsaved_count: 1 }
        );
    }
}
