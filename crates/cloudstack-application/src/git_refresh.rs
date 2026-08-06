//! Git 状态刷新完成时的纯判定逻辑，不依赖 GTK/glib/gio/adw/webkit。
//! 后台线程池（`gio::spawn_blocking`）不保证完成顺序，同一项目连续触发两次
//! 刷新时，先发出的请求可能后完成——只校验 project root 不够，还需要
//! generation 保证"后完成的旧请求不会覆盖已经完成的新请求"。

use std::path::Path;

pub fn should_apply_git_refresh(
    current_root: Option<&Path>,
    expected_root: &Path,
    current_generation: u64,
    expected_generation: u64,
) -> bool {
    current_root == Some(expected_root) && current_generation == expected_generation
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("/tmp/project")
    }

    #[test]
    fn applies_when_root_and_generation_match() {
        assert!(should_apply_git_refresh(Some(&root()), &root(), 3, 3));
    }

    #[test]
    fn rejects_when_generation_advanced() {
        assert!(!should_apply_git_refresh(Some(&root()), &root(), 4, 3));
    }

    #[test]
    fn rejects_when_root_changed() {
        let other = PathBuf::from("/tmp/other");
        assert!(!should_apply_git_refresh(Some(&other), &root(), 3, 3));
    }

    #[test]
    fn rejects_when_project_closed() {
        assert!(!should_apply_git_refresh(None, &root(), 3, 3));
    }
}
