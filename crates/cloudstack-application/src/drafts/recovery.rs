use std::path::Path;

use cloudstack_core::model::{DraftDocument, PostDocument};

/// 冷启动/切换文章之外的场景下，是否还能把读到的自动恢复草稿展示给用户：
/// 五个条件都要满足——不忙、当前文档没有未保存的改动、仍是派发读取时的
/// 那个项目会话（root 没变）、仍是那篇文章、document epoch 没被切走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftRecoveryEligibilityInput<'a> {
    pub busy: bool,
    pub dirty: bool,

    pub expected_project_root: &'a Path,
    pub current_project_root: Option<&'a Path>,

    pub expected_post_id: &'a str,
    pub current_post_id: Option<&'a str>,

    pub expected_epoch: u64,
    pub current_epoch: u64,
}

pub fn can_offer_recovery(input: DraftRecoveryEligibilityInput<'_>) -> bool {
    !input.busy
        && !input.dirty
        && input.current_project_root == Some(input.expected_project_root)
        && input.current_post_id == Some(input.expected_post_id)
        && input.current_epoch == input.expected_epoch
}

/// 一次写入/删除失败该不该展示给用户：只看当前是不是还停在那篇文章上，不
/// 关心 busy/dirty/epoch（这些跟"要不要打扰用户看一条失败提示"无关，是
/// `can_offer_recovery` 才需要的更严格的条件）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentDraftTargetInput<'a> {
    pub expected_project_root: &'a Path,
    pub current_project_root: Option<&'a Path>,
    pub expected_post_id: &'a str,
    pub current_post_id: Option<&'a str>,
}

pub fn is_current_draft_target(input: CurrentDraftTargetInput<'_>) -> bool {
    input.current_project_root == Some(input.expected_project_root)
        && input.current_post_id == Some(input.expected_post_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftRecoveryDecision {
    /// 草稿内容跟磁盘上的文档完全一样（不管 base_revision 是否匹配），
    /// 已经没有价值，直接清掉，不用打扰用户。
    DeleteRedundant,
    Offer {
        disk_changed_since_draft: bool,
    },
}

pub fn classify_recovery(document: &PostDocument, draft: &DraftDocument) -> DraftRecoveryDecision {
    if draft.raw_frontmatter == document.raw_frontmatter && draft.body == document.body {
        DraftRecoveryDecision::DeleteRedundant
    } else {
        DraftRecoveryDecision::Offer {
            disk_changed_since_draft: draft.base_revision != document.revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn recovery_requires_idle_matching_project_post_and_epoch() {
        let root = PathBuf::from("/tmp/project");
        let base = DraftRecoveryEligibilityInput {
            busy: false,
            dirty: false,
            expected_project_root: &root,
            current_project_root: Some(&root),
            expected_post_id: "a.md",
            current_post_id: Some("a.md"),
            expected_epoch: 1,
            current_epoch: 1,
        };
        assert!(can_offer_recovery(base), "全部条件满足时应该允许提示恢复");

        assert!(
            !can_offer_recovery(DraftRecoveryEligibilityInput { busy: true, ..base }),
            "正在忙时不该提示"
        );
        assert!(
            !can_offer_recovery(DraftRecoveryEligibilityInput {
                dirty: true,
                ..base
            }),
            "当前文档已经有未保存改动时不该覆盖"
        );
        let other_root = PathBuf::from("/tmp/other-project");
        assert!(
            !can_offer_recovery(DraftRecoveryEligibilityInput {
                current_project_root: Some(&other_root),
                ..base
            }),
            "跨异步边界后项目已经切换，不该把恢复提示打到新项目上"
        );
        assert!(
            !can_offer_recovery(DraftRecoveryEligibilityInput {
                current_post_id: Some("b.md"),
                ..base
            }),
            "用户已经切到别的文章时不该提示"
        );
        assert!(
            !can_offer_recovery(DraftRecoveryEligibilityInput {
                current_epoch: 2,
                ..base
            }),
            "document epoch 已经推进（比如重新加载过）时不该提示"
        );
    }

    fn document(raw_frontmatter: Option<&str>, body: &str, revision: &str) -> PostDocument {
        PostDocument {
            id: "a.md".into(),
            relative_path: "a.md".into(),
            raw_frontmatter: raw_frontmatter.map(str::to_owned),
            body: body.into(),
            revision: revision.into(),
        }
    }

    fn draft(raw_frontmatter: Option<&str>, body: &str, base_revision: &str) -> DraftDocument {
        DraftDocument {
            post_id: "a.md".into(),
            raw_frontmatter: raw_frontmatter.map(str::to_owned),
            body: body.into(),
            base_revision: base_revision.into(),
            saved_at_ms: 0,
        }
    }

    #[test]
    fn identical_draft_is_deleted_without_prompt() {
        let document = document(Some("title: a"), "body", "rev-1");
        let draft = draft(Some("title: a"), "body", "rev-0");
        assert_eq!(
            classify_recovery(&document, &draft),
            DraftRecoveryDecision::DeleteRedundant,
            "内容跟磁盘一致时不该管 base_revision 是否匹配，直接清掉"
        );
    }

    #[test]
    fn matching_base_revision_offers_normal_recovery() {
        let document = document(Some("title: a"), "disk body", "rev-1");
        let draft = draft(Some("title: a"), "draft body", "rev-1");
        assert_eq!(
            classify_recovery(&document, &draft),
            DraftRecoveryDecision::Offer {
                disk_changed_since_draft: false
            }
        );
    }

    #[test]
    fn changed_base_revision_offers_disk_changed_warning() {
        let document = document(Some("title: a"), "disk body", "rev-2");
        let draft = draft(Some("title: a"), "draft body", "rev-1");
        assert_eq!(
            classify_recovery(&document, &draft),
            DraftRecoveryDecision::Offer {
                disk_changed_since_draft: true
            }
        );
    }

    #[test]
    fn current_draft_target_requires_both_root_and_post_id() {
        let root = PathBuf::from("/tmp/project");
        let other_root = PathBuf::from("/tmp/other-project");
        let base = CurrentDraftTargetInput {
            expected_project_root: &root,
            current_project_root: Some(&root),
            expected_post_id: "a.md",
            current_post_id: Some("a.md"),
        };
        assert!(is_current_draft_target(base));

        assert!(!is_current_draft_target(CurrentDraftTargetInput {
            current_project_root: Some(&other_root),
            ..base
        }));
        assert!(!is_current_draft_target(CurrentDraftTargetInput {
            current_post_id: Some("b.md"),
            ..base
        }));
        assert!(!is_current_draft_target(CurrentDraftTargetInput {
            current_project_root: None,
            ..base
        }));
    }
}
