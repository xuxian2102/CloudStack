use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

pub const MAX_RECENT_PROJECTS: usize = 10;
const MAX_RECENT_FILE_BYTES: usize = 64 * 1024;
const CURRENT_VERSION: u32 = 1;

/// 串行化 touch/remove/set_pinned 的读-改-写，避免 `tasks::run` 共享线程池下的
/// 并发覆盖。load() 不需要它：write_atomic 用 rename 落盘，读到的要么是完整
/// 旧文件要么是完整新文件，不存在撕裂读。
static RECENT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    pub root: PathBuf,
    pub last_opened_ms: u64,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentProjectsFile {
    version: u32,
    projects: Vec<RecentProject>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn recent_projects_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("recent-projects.json")
}

/// 把损坏或超限的文件挪到一边，从空列表重新开始，不让一次坏数据永久卡死
/// 之后所有 touch()（因为 touch 内部也要先 load）。
fn quarantine(path: &Path, reason: &str) {
    let target = path.with_extension(format!("json.corrupt-{}", now_ms()));
    match fs::rename(path, &target) {
        Ok(()) => log::warn!(
            "最近项目列表已损坏（{reason}），已备份到 {}",
            target.display()
        ),
        Err(error) => log::warn!("最近项目列表已损坏（{reason}），备份失败：{error}"),
    }
}

fn read_file(app_data_dir: &Path) -> Vec<RecentProject> {
    let path = recent_projects_path(app_data_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            log::warn!("读取最近项目列表失败：{error}");
            return Vec::new();
        }
    };
    if bytes.len() > MAX_RECENT_FILE_BYTES {
        quarantine(&path, "文件异常过大");
        return Vec::new();
    }
    match serde_json::from_slice::<RecentProjectsFile>(&bytes) {
        Ok(parsed) if parsed.version == CURRENT_VERSION => parsed.projects,
        Ok(_) => {
            quarantine(&path, "版本不受支持");
            Vec::new()
        }
        Err(error) => {
            quarantine(&path, &error.to_string());
            Vec::new()
        }
    }
}

/// 已固定的排在前面（各自保持 MRU 顺序），保证磁盘上的顺序就是 UI 需要展示的
/// 分组顺序，欢迎页不用再排序。只截断未固定的部分——固定是用户手动点出来的、
/// 天然受人的耐心限制的小集合，不像"最近打开"那样每次打开项目都会自动增长，
/// 截断固定列表只会在触顶时把新固定的条目直接从磁盘上抹掉，没有必要冒这个
/// 险（`MAX_RECENT_FILE_BYTES` 已经从字节数上防住了异常巨大的文件）。
fn normalize(projects: Vec<RecentProject>) -> Vec<RecentProject> {
    let mut pinned = Vec::new();
    let mut unpinned = Vec::new();
    for project in projects {
        if project.pinned {
            pinned.push(project);
        } else {
            unpinned.push(project);
        }
    }
    unpinned.truncate(MAX_RECENT_PROJECTS);
    pinned.into_iter().chain(unpinned).collect()
}

fn write_atomic(
    app_data_dir: &Path,
    projects: Vec<RecentProject>,
) -> Result<Vec<RecentProject>, AppError> {
    let file = RecentProjectsFile {
        version: CURRENT_VERSION,
        projects: normalize(projects),
    };
    let mut bytes = serde_json::to_vec(&file)
        .map_err(|error| AppError::Io(format!("最近项目列表序列化失败：{error}")))?;
    bytes.push(b'\n');

    fs::create_dir_all(app_data_dir)?;
    let path = recent_projects_path(app_data_dir);
    let mut temporary = tempfile::NamedTempFile::new_in(app_data_dir)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| AppError::Io(error.to_string()))?;
    fs::File::open(app_data_dir)?.sync_all()?;
    Ok(file.projects)
}

/// 供欢迎页展示；缺失文件返回空列表，不是错误。
pub fn load(app_data_dir: &Path) -> Result<Vec<RecentProject>, AppError> {
    Ok(read_file(app_data_dir))
}

/// 打开项目成功后调用：去重、置顶、按上限截断。重开一个已固定的项目不会把它
/// 变回未固定。
pub fn touch(app_data_dir: &Path, project_root: &Path) -> Result<Vec<RecentProject>, AppError> {
    let _guard = RECENT_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut projects = read_file(app_data_dir);
    let was_pinned = projects
        .iter()
        .any(|entry| entry.root == project_root && entry.pinned);
    projects.retain(|entry| entry.root != project_root);
    projects.insert(
        0,
        RecentProject {
            root: project_root.to_path_buf(),
            last_opened_ms: now_ms(),
            pinned: was_pinned,
        },
    );
    write_atomic(app_data_dir, projects)
}

/// 从最近列表中移除一项，不触碰磁盘上的项目文件；不管是否固定都能移除。
pub fn remove(app_data_dir: &Path, project_root: &Path) -> Result<Vec<RecentProject>, AppError> {
    let _guard = RECENT_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut projects = read_file(app_data_dir);
    projects.retain(|entry| entry.root != project_root);
    write_atomic(app_data_dir, projects)
}

/// 切换某个项目的固定状态；条目已不在列表里（比如被并发的 remove 抢先移除）
/// 时是空操作，不报错。
pub fn set_pinned(
    app_data_dir: &Path,
    project_root: &Path,
    pinned: bool,
) -> Result<Vec<RecentProject>, AppError> {
    let _guard = RECENT_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut projects = read_file(app_data_dir);
    for entry in &mut projects {
        if entry.root == project_root {
            entry.pinned = pinned;
        }
    }
    write_atomic(app_data_dir, projects)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        PathBuf::from(format!("/projects/{name}"))
    }

    #[test]
    fn missing_file_returns_empty_list() {
        let app_data = tempfile::tempdir().unwrap();
        assert_eq!(load(app_data.path()).unwrap(), Vec::new());
    }

    #[test]
    fn touch_dedupes_and_moves_to_front() {
        let app_data = tempfile::tempdir().unwrap();
        touch(app_data.path(), &root("a")).unwrap();
        touch(app_data.path(), &root("b")).unwrap();
        let projects = touch(app_data.path(), &root("a")).unwrap();
        assert_eq!(
            projects.iter().map(|p| &p.root).collect::<Vec<_>>(),
            vec![&root("a"), &root("b")]
        );
    }

    #[test]
    fn touch_truncates_at_max_recent_projects() {
        let app_data = tempfile::tempdir().unwrap();
        for index in 0..MAX_RECENT_PROJECTS + 3 {
            touch(app_data.path(), &root(&index.to_string())).unwrap();
        }
        let projects = load(app_data.path()).unwrap();
        assert_eq!(projects.len(), MAX_RECENT_PROJECTS);
        assert_eq!(
            projects[0].root,
            root(&(MAX_RECENT_PROJECTS + 2).to_string())
        );
    }

    #[test]
    fn remove_deletes_the_matching_entry_only() {
        let app_data = tempfile::tempdir().unwrap();
        touch(app_data.path(), &root("a")).unwrap();
        touch(app_data.path(), &root("b")).unwrap();
        let projects = remove(app_data.path(), &root("a")).unwrap();
        assert_eq!(
            projects.iter().map(|p| &p.root).collect::<Vec<_>>(),
            vec![&root("b")]
        );
    }

    #[test]
    fn corrupted_file_recovers_to_empty_list_and_is_quarantined() {
        let app_data = tempfile::tempdir().unwrap();
        fs::create_dir_all(app_data.path()).unwrap();
        fs::write(recent_projects_path(app_data.path()), b"not json").unwrap();

        assert_eq!(load(app_data.path()).unwrap(), Vec::new());

        let quarantined = fs::read_dir(app_data.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("recent-projects.json.corrupt-")
            });
        assert!(quarantined, "损坏文件应被改名隔离");
    }

    #[test]
    fn oversized_file_recovers_to_empty_list_and_is_quarantined() {
        let app_data = tempfile::tempdir().unwrap();
        fs::create_dir_all(app_data.path()).unwrap();
        let oversized = "x".repeat(MAX_RECENT_FILE_BYTES + 1);
        fs::write(recent_projects_path(app_data.path()), oversized).unwrap();

        assert_eq!(load(app_data.path()).unwrap(), Vec::new());
        assert!(!recent_projects_path(app_data.path()).exists());
    }

    #[test]
    fn concurrent_touches_do_not_lose_updates() {
        let app_data = tempfile::tempdir().unwrap();
        let app_data_path = app_data.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let app_data_path = app_data_path.clone();
                std::thread::spawn(move || {
                    touch(&app_data_path, &root(&index.to_string())).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let projects = load(app_data.path()).unwrap();
        assert_eq!(projects.len(), 8.min(MAX_RECENT_PROJECTS));
    }

    #[test]
    fn touch_preserves_pinned_flag_across_reopen() {
        let app_data = tempfile::tempdir().unwrap();
        touch(app_data.path(), &root("a")).unwrap();
        set_pinned(app_data.path(), &root("a"), true).unwrap();
        let projects = touch(app_data.path(), &root("a")).unwrap();
        assert!(projects[0].pinned);
    }

    #[test]
    fn set_pinned_toggles_flag_and_moves_entry_into_pinned_group() {
        let app_data = tempfile::tempdir().unwrap();
        touch(app_data.path(), &root("a")).unwrap();
        touch(app_data.path(), &root("b")).unwrap();
        let projects = set_pinned(app_data.path(), &root("a"), true).unwrap();
        assert_eq!(projects[0].root, root("a"));
        assert!(projects[0].pinned);
        assert!(!projects[1].pinned);

        let projects = set_pinned(app_data.path(), &root("a"), false).unwrap();
        assert!(!projects.iter().any(|p| p.pinned));
    }

    #[test]
    fn pinned_entries_survive_touch_truncation_beyond_max_recent_projects() {
        let app_data = tempfile::tempdir().unwrap();
        touch(app_data.path(), &root("pinned")).unwrap();
        set_pinned(app_data.path(), &root("pinned"), true).unwrap();

        for index in 0..MAX_RECENT_PROJECTS + 5 {
            touch(app_data.path(), &root(&format!("other-{index}"))).unwrap();
        }

        let projects = load(app_data.path()).unwrap();
        assert!(projects
            .iter()
            .any(|p| p.root == root("pinned") && p.pinned));
        assert_eq!(
            projects.iter().filter(|p| !p.pinned).count(),
            MAX_RECENT_PROJECTS
        );
    }

    #[test]
    fn pinned_projects_are_never_truncated() {
        let app_data = tempfile::tempdir().unwrap();
        let pin_count = MAX_RECENT_PROJECTS * 2 + 5;
        for index in 0..pin_count {
            let path = root(&format!("pinned-{index}"));
            touch(app_data.path(), &path).unwrap();
            set_pinned(app_data.path(), &path, true).unwrap();
        }
        let projects = load(app_data.path()).unwrap();
        assert_eq!(projects.len(), pin_count);
        assert!(projects.iter().all(|p| p.pinned));
    }
}
