use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{AppError, ErrorPayload};
use crate::model::{
    ChangeKind, CommandTrace, FileChange, GitEnvironment, GitIdentity, GitOperationResult,
    GitRemote, GitStatus, OperationReport, ProjectContext, PublishResult, RepositorySnapshot,
    RepositoryTopology, RepositoryVisibility, SyncRelation, WorktreeState,
};

const MAX_GIT_STREAM_BYTES: usize = 16 * 1024 * 1024;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(180);

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    trace: CommandTrace,
}

fn command_timeout(args: &[&str]) -> Duration {
    if args.first() == Some(&"push") {
        GIT_PUSH_TIMEOUT
    } else {
        GIT_COMMAND_TIMEOUT
    }
}

fn read_bounded(mut stream: impl std::io::Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_GIT_STREAM_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        if count > remaining {
            truncated = true;
        }
    }
    Ok((bytes, truncated))
}

fn terminate_process_group(child: &mut std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    // SAFETY: spawn 时为 git 创建了 pid == pgid 的独立进程组；负 pid 只作用于该组。
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        let _ = child.try_wait();
        // SAFETY: signal 0 不发送信号，只检查这个独立进程组是否仍存在。
        if unsafe { libc::kill(-pid, 0) } != 0 {
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: 同上；SIGTERM 后仍存活时强制结束，避免 hook/ssh 子进程遗留。
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
}

fn run_command(
    root: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, AppError> {
    let started = Instant::now();
    let mut child = Command::new(program)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("LC_ALL", "C")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| AppError::Git(format!("无法执行 {program}：{error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Git("无法读取 git 标准输出".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Git("无法读取 git 错误输出".into()))?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| AppError::Git(format!("等待 git 失败：{error}")))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            terminate_process_group(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(AppError::Git(format!(
                "{program} {} 执行超时（{} 秒）",
                args.first().copied().unwrap_or("command"),
                timeout.as_secs(),
            )));
        }
        thread::sleep(Duration::from_millis(20));
    };

    if let Ok(pid) = i32::try_from(child.id()) {
        // git 已退出时，清理极少数仍继承 stdout/stderr 的后台 hook/helper，避免读取线程
        // 因管道不关闭而永久等待。此进程组只属于本次 git 命令。
        // SAFETY: spawn 时用 process_group(0) 创建了独立进程组。
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }

    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| AppError::Git("git 标准输出读取线程异常结束".into()))?
        .map_err(|error| AppError::Git(format!("读取 git 标准输出失败：{error}")))?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| AppError::Git("git 错误输出读取线程异常结束".into()))?
        .map_err(|error| AppError::Git(format!("读取 git 错误输出失败：{error}")))?;
    if stdout_truncated || stderr_truncated {
        return Err(AppError::Git("git 输出超过 16 MiB，已中止处理".into()));
    }
    let trace = CommandTrace {
        command: display_command(program, args),
        stdout: redact_secrets(&String::from_utf8_lossy(&stdout)),
        stderr: redact_secrets(&String::from_utf8_lossy(&stderr)),
        exit_code: status.code(),
        success: status.success(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    Ok(CommandOutput {
        status,
        stdout,
        stderr,
        trace,
    })
}

fn run_git(root: &Path, args: &[&str]) -> Result<CommandOutput, AppError> {
    run_command(root, "git", args, command_timeout(args))
}

fn run_gh(root: &Path, args: &[&str]) -> Result<CommandOutput, AppError> {
    run_command(root, "gh", args, GIT_PUSH_TIMEOUT)
}

fn display_command(program: &str, args: &[&str]) -> String {
    let rendered = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    redact_secrets(&format!(
        "GIT_TERMINAL_PROMPT=0 SSH_ASKPASS_REQUIRE=never {program} {rendered}"
    ))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@%+=,-".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// 在任何命令输出进入可展示的 CommandTrace 之前清除 URL userinfo。
/// 例如 https://user:token@example.com/repo 会变为 https://***@example.com/repo。
fn redact_secrets(value: &str) -> String {
    let mut result = value.to_owned();
    let mut search_from = 0;
    while let Some(relative) = result[search_from..].find("://") {
        let scheme_end = search_from + relative + 3;
        let authority_end = result[scheme_end..]
            .find(|character: char| character == '/' || character.is_whitespace())
            .map_or(result.len(), |offset| scheme_end + offset);
        let Some(at_offset) = result[scheme_end..authority_end].rfind('@') else {
            search_from = authority_end;
            continue;
        };
        let at = scheme_end + at_offset;
        result.replace_range(scheme_end..at, "***");
        search_from = scheme_end + 4;
    }
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"] {
        let mut search_from = 0;
        while let Some(relative) = result[search_from..].find(prefix) {
            let start = search_from + relative;
            let token_end = result[start..]
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .count()
                + start;
            result.replace_range(start..token_end, "***");
            search_from = start + 3;
        }
    }
    result
}

/// 裁剪过的 stderr，用于把 git 的原始报错透传给用户
fn git_error_message(output: &CommandOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        format!("git 退出码 {:?}", output.status.code())
    } else {
        trimmed.to_string()
    }
}

fn map_git_error(output: &CommandOutput) -> AppError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("not a git repository") {
        return AppError::Git("项目目录不是 git 仓库".into());
    }
    AppError::Git(git_error_message(output))
}

pub fn status(ctx: &ProjectContext) -> Result<GitStatus, AppError> {
    match repository_root(&ctx.root)? {
        Some(root) if root == ctx.root => {}
        Some(_) => {
            return Err(AppError::Git(
                "项目目录自身不是 Git 仓库根目录；为避免误操作父仓库，已拒绝继续".into(),
            ));
        }
        None => return Err(AppError::Git("项目目录不是 git 仓库".into())),
    }
    // --untracked-files=all：默认 git 会把整个未跟踪目录折叠成一行（如 "src/"），
    // 这样匹配不到 content_dir 前缀，新建文章所在的全新子目录就会被漏判为 managed=false
    let output = run_git(
        &ctx.root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "-z",
        ],
    )?;
    if !output.status.success() {
        return Err(map_git_error(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_porcelain_v2(&text, &ctx.config.content_dir))
}

fn repository_root(root: &Path) -> Result<Option<PathBuf>, AppError> {
    let output = run_git(root, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return Ok(None);
        }
        return Err(map_git_error(&output));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let canonical = std::fs::canonicalize(&path)
        .map_err(|error| AppError::Io(format!("无法确认 Git 仓库根目录 {path}：{error}")))?;
    Ok(Some(canonical))
}

fn inspect_environment(root: &Path) -> GitEnvironment {
    let git_available = run_command(root, "git", &["--version"], GIT_COMMAND_TIMEOUT)
        .is_ok_and(|output| output.status.success());
    let gh_available = run_command(root, "gh", &["--version"], GIT_COMMAND_TIMEOUT)
        .is_ok_and(|output| output.status.success());
    let gh_authenticated = gh_available
        && run_command(
            root,
            "gh",
            &["auth", "status", "--active", "--hostname", "github.com"],
            GIT_COMMAND_TIMEOUT,
        )
        .is_ok_and(|output| output.status.success());
    GitEnvironment {
        git_available,
        gh_available,
        gh_authenticated,
    }
}

fn empty_snapshot(topology: RepositoryTopology, environment: GitEnvironment) -> RepositorySnapshot {
    RepositorySnapshot {
        environment,
        identity: None,
        topology,
        sync: SyncRelation::Unknown,
        worktree: WorktreeState::default(),
        remotes: Vec::new(),
        config_tracked: false,
        status: GitStatus {
            branch: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            changes: Vec::new(),
        },
    }
}

/// 从彼此正交的仓库拓扑、同步关系和工作区状态描述当前 Git 仓库。
/// 非 Git 项目是一个正常状态，而不是读取状态失败。
pub fn snapshot(ctx: &ProjectContext) -> Result<RepositorySnapshot, AppError> {
    let environment = inspect_environment(&ctx.root);
    if !environment.git_available {
        return Ok(empty_snapshot(
            RepositoryTopology::NotInitialized,
            environment,
        ));
    }
    match repository_root(&ctx.root)? {
        None => {
            return Ok(empty_snapshot(
                RepositoryTopology::NotInitialized,
                environment,
            ));
        }
        Some(root) if root != ctx.root => {
            return Ok(empty_snapshot(
                RepositoryTopology::ParentRepository,
                environment,
            ));
        }
        Some(_) => {}
    }

    let status = status(ctx)?;
    let identity = git_identity(&ctx.root)?;
    let has_head = run_git(&ctx.root, &["rev-parse", "--verify", "HEAD"])?
        .status
        .success();
    let config_tracked = run_git(
        &ctx.root,
        &["ls-files", "--error-unmatch", "--", active_config_name(ctx)],
    )?
    .status
    .success();
    let remotes = run_git(&ctx.root, &["remote"])?;
    if !remotes.status.success() {
        return Err(map_git_error(&remotes));
    }
    let mut remote_names = String::from_utf8_lossy(&remotes.stdout)
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    remote_names.sort();
    let has_remote = !remote_names.is_empty();
    let remotes = remote_names
        .iter()
        .map(|name| {
            let output = run_git(&ctx.root, &["remote", "get-url", name])?;
            let url = output
                .status
                .success()
                .then(|| redact_secrets(String::from_utf8_lossy(&output.stdout).trim()));
            Ok(GitRemote {
                name: name.clone(),
                url: url.filter(|url| !url.is_empty()),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let topology = if !has_head {
        RepositoryTopology::NoCommit
    } else if status.branch.is_none() {
        RepositoryTopology::Detached
    } else if !has_remote {
        RepositoryTopology::NoRemote
    } else if status.upstream.is_none() {
        RepositoryTopology::NoUpstream
    } else {
        RepositoryTopology::Tracking
    };
    let sync = if status.upstream.is_none() {
        SyncRelation::Unknown
    } else {
        match (status.ahead > 0, status.behind > 0) {
            (false, false) => SyncRelation::Synced,
            (true, false) => SyncRelation::Ahead,
            (false, true) => SyncRelation::Behind,
            (true, true) => SyncRelation::Diverged,
        }
    };
    let worktree = WorktreeState {
        managed_changes: status
            .changes
            .iter()
            .filter(|change| change.managed)
            .count(),
        unmanaged_changes: status
            .changes
            .iter()
            .filter(|change| !change.managed)
            .count(),
        staged_changes: status.changes.iter().filter(|change| change.staged).count(),
        has_conflicts: status
            .changes
            .iter()
            .any(|change| change.kind == ChangeKind::Unmerged),
    };
    Ok(RepositorySnapshot {
        environment,
        identity,
        topology,
        sync,
        worktree,
        remotes,
        config_tracked,
        status,
    })
}

fn git_identity(root: &Path) -> Result<Option<GitIdentity>, AppError> {
    let name = run_git(root, &["config", "--get", "user.name"])?;
    let email = run_git(root, &["config", "--get", "user.email"])?;
    if !name.status.success() || !email.status.success() {
        return Ok(None);
    }
    let name = String::from_utf8_lossy(&name.stdout).trim().to_owned();
    let email = String::from_utf8_lossy(&email.stdout).trim().to_owned();
    if name.is_empty() || email.is_empty() {
        Ok(None)
    } else {
        Ok(Some(GitIdentity { name, email }))
    }
}

fn active_config_name(ctx: &ProjectContext) -> &str {
    ctx.config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(crate::services::project::CONFIG_FILE)
}

fn is_managed(path: &str, content_dir: &str) -> bool {
    let content_dir = content_dir.trim_end_matches('/');
    path == content_dir || path.starts_with(&format!("{content_dir}/"))
}

/// 首次提交和日常发布共用的受管路径白名单。
/// 路径只来源于 Git status，不使用 `git add .` / `git add -A`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedScope {
    paths: Vec<String>,
}

impl ManagedScope {
    pub fn from_status(ctx: &ProjectContext, status: &GitStatus) -> Self {
        let mut scope = Self::all_from_status(ctx, status);
        scope.paths.retain(|path| {
            owning_article(ctx, status, path).is_none_or(|article| {
                !ctx.config
                    .git
                    .excluded_articles
                    .iter()
                    .any(|excluded| excluded == &article)
            })
        });
        scope
    }

    pub fn from_selected(
        ctx: &ProjectContext,
        status: &GitStatus,
        selected_paths: &[String],
    ) -> Result<Self, AppError> {
        let available = Self::all_from_status(ctx, status);
        let mut paths = selected_paths.to_vec();
        paths.sort();
        paths.dedup();
        if paths
            .iter()
            .any(|path| !available.paths.iter().any(|available| available == path))
        {
            return Err(AppError::Git(
                "提交选择包含当前受管改动之外的路径，已拒绝继续".into(),
            ));
        }
        Ok(Self { paths })
    }

    fn all_from_status(ctx: &ProjectContext, status: &GitStatus) -> Self {
        let mut paths = Vec::new();
        for change in &status.changes {
            if change.managed {
                paths.push(change.path.clone());
            }
            if let Some(old) = &change.old_path {
                if is_managed(old, &ctx.config.content_dir) {
                    paths.push(old.clone());
                }
            }
        }
        paths.sort();
        paths.dedup();
        Self { paths }
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

fn owning_article(ctx: &ProjectContext, status: &GitStatus, git_path: &str) -> Option<String> {
    let content_prefix = format!("{}/", ctx.config.content_dir.trim_end_matches('/'));
    let relative = git_path.strip_prefix(&content_prefix)?;
    if has_article_extension(ctx, relative) {
        return Some(relative.to_owned());
    }

    // 图片目录和嵌套文章目录可以重叠（hello.md 与 hello/nested.md）。从图片父目录
    // 向上寻找，最深层真实/待删除文章拥有该图片，避免外层排除项吞掉嵌套文章。
    let mut stem = Path::new(relative).parent()?;
    loop {
        for extension in &ctx.config.extensions {
            let candidate = format!("{}{}", stem.to_string_lossy(), extension);
            let appears_in_status = status.changes.iter().any(|change| {
                change.path == format!("{content_prefix}{candidate}")
                    || change.old_path.as_deref() == Some(&format!("{content_prefix}{candidate}"))
            });
            if appears_in_status || ctx.content_root.join(&candidate).is_file() {
                return Some(candidate);
            }
        }
        stem = stem.parent()?;
    }
}

fn has_article_extension(ctx: &ProjectContext, relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ctx.config
                .extensions
                .iter()
                .any(|allowed| allowed.strip_prefix('.') == Some(extension))
        })
}

fn classify(xy: &str) -> ChangeKind {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');
    let code = if x != '.' { x } else { y };
    match code {
        'A' => ChangeKind::Added,
        'D' => ChangeKind::Deleted,
        'R' | 'C' => ChangeKind::Renamed,
        _ => ChangeKind::Modified,
    }
}

fn build_change(xy: &str, path: String, old_path: Option<String>, content_dir: &str) -> FileChange {
    let staged = xy.chars().next().is_some_and(|x| x != '.');
    let kind = classify(xy);
    let managed = is_managed(&path, content_dir);
    FileChange {
        path,
        old_path,
        kind,
        staged,
        managed,
    }
}

/// `1 XY sub mH mI mW hH hI path`（"1 " 已被调用方剥离）
fn parse_ordinary_fields(rest: &str) -> Option<(&str, &str)> {
    let mut parts = rest.splitn(8, ' ');
    let xy = parts.next()?;
    for _ in 0..6 {
        parts.next()?;
    }
    let path = parts.next()?;
    Some((xy, path))
}

/// `2 XY sub mH mI mW hH hI X<score> path`（origPath 是下一个 NUL 分隔的 token）
fn parse_rename_fields(rest: &str) -> Option<(&str, &str)> {
    let mut parts = rest.splitn(9, ' ');
    let xy = parts.next()?;
    for _ in 0..7 {
        parts.next()?;
    }
    let path = parts.next()?;
    Some((xy, path))
}

/// 解析 `git status --porcelain=v2 --branch -z` 的输出。
/// 用 -z 而不是默认的换行分隔，是因为文章标题常含中文/特殊字符，
/// 非 -z 模式下 git 会对路径做 C 风格转义，徒增解析负担。
pub fn parse_porcelain_v2(text: &str, content_dir: &str) -> GitStatus {
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut changes = Vec::new();

    let mut tokens = text.split('\0').filter(|t| !t.is_empty());

    while let Some(tok) = tokens.next() {
        if let Some(rest) = tok.strip_prefix("# branch.head ") {
            if rest != "(detached)" {
                branch = Some(rest.to_string());
            }
        } else if let Some(rest) = tok.strip_prefix("# branch.upstream ") {
            upstream = Some(rest.to_string());
        } else if let Some(rest) = tok.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if tok.starts_with("# ") {
            // 其他 header（如 branch.oid），忽略
        } else if let Some(rest) = tok.strip_prefix("1 ") {
            if let Some((xy, path)) = parse_ordinary_fields(rest) {
                changes.push(build_change(xy, path.to_string(), None, content_dir));
            }
        } else if let Some(rest) = tok.strip_prefix("2 ") {
            if let Some((xy, path)) = parse_rename_fields(rest) {
                let old_path = tokens.next().unwrap_or("").to_string();
                changes.push(build_change(
                    xy,
                    path.to_string(),
                    Some(old_path),
                    content_dir,
                ));
            }
        } else if let Some(rest) = tok.strip_prefix("u ") {
            if let Some(path) = rest.split_whitespace().last() {
                changes.push(FileChange {
                    path: path.to_string(),
                    old_path: None,
                    kind: ChangeKind::Unmerged,
                    staged: false,
                    managed: is_managed(path, content_dir),
                });
            }
        } else if let Some(path) = tok.strip_prefix("? ") {
            changes.push(FileChange {
                path: path.to_string(),
                old_path: None,
                kind: ChangeKind::Untracked,
                staged: false,
                managed: is_managed(path, content_dir),
            });
        }
        // "!" ignored 条目：没有传 --ignored，不会出现
    }

    GitStatus {
        branch,
        upstream,
        ahead,
        behind,
        changes,
    }
}

fn current_commit_hash(root: &Path) -> Option<String> {
    let output = run_git(root, &["rev-parse", "HEAD"]).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn classify_push_error(stderr: &str) -> ErrorPayload {
    if stderr.contains("has no upstream branch") {
        ErrorPayload::git_push_no_upstream(
            "当前分支没有配置上游分支，请先在终端执行一次 git push -u <remote> <分支名>",
        )
    } else if stderr.contains("Authentication failed")
        || stderr.contains("could not read Username")
        || stderr.contains("Permission denied (publickey)")
    {
        ErrorPayload::git_push_authentication_failed(
            "推送失败（认证问题）：请检查 git 凭证或 SSH key 配置",
        )
    } else {
        let trimmed = stderr.trim();
        if trimmed.is_empty() {
            ErrorPayload::git_push_failed("推送失败")
        } else {
            ErrorPayload::git_push_failed_detail(format!("推送失败：{trimmed}"), trimmed.to_owned())
        }
    }
}

fn finish_operation(output: CommandOutput, fallback: &str) -> GitOperationResult {
    let error = if output.status.success() {
        None
    } else {
        let detail = if !output.trace.stderr.trim().is_empty() {
            output.trace.stderr.trim().to_owned()
        } else if !output.trace.stdout.trim().is_empty() {
            output.trace.stdout.trim().to_owned()
        } else {
            fallback.to_owned()
        };
        Some(detail)
    };
    GitOperationResult {
        report: OperationReport {
            traces: vec![output.trace],
        },
        error,
    }
}

fn ensure_exact_repository(ctx: &ProjectContext) -> Result<(), AppError> {
    match repository_root(&ctx.root)? {
        Some(root) if root == ctx.root => Ok(()),
        Some(_) => Err(AppError::Git(
            "项目位于父级 Git 仓库中；为避免误操作父仓库，必须先在项目根目录独立初始化".into(),
        )),
        None => Err(AppError::Git("项目尚未初始化 Git 仓库".into())),
    }
}

fn has_head(root: &Path) -> Result<bool, AppError> {
    Ok(run_git(root, &["rev-parse", "--verify", "HEAD"])?
        .status
        .success())
}

fn remote_exists(root: &Path, name: &str) -> Result<bool, AppError> {
    Ok(run_git(root, &["remote", "get-url", name])?
        .status
        .success())
}

fn validate_remote_url(url: &str) -> Result<(), AppError> {
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') || url.contains(['\n', '\r', '\0']) {
        return Err(AppError::Git("远端地址无效".into()));
    }
    let supported = url.starts_with("https://")
        || url.starts_with("ssh://")
        || (url.contains('@') && url.contains(':') && !url.contains(char::is_whitespace));
    if !supported {
        return Err(AppError::Git("远端地址必须使用 HTTPS 或 SSH".into()));
    }
    if let Some(authority) = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
    {
        if authority.contains('@') {
            return Err(AppError::Git(
                "HTTPS 远端地址不能内嵌用户名或令牌，请使用凭据助手".into(),
            ));
        }
    }
    if let Some(authority) = url
        .strip_prefix("ssh://")
        .and_then(|rest| rest.split('/').next())
    {
        if authority
            .split_once('@')
            .is_some_and(|(userinfo, _)| userinfo.contains(':'))
        {
            return Err(AppError::Git(
                "SSH 远端地址不能内嵌密码，请使用 SSH agent".into(),
            ));
        }
    }
    Ok(())
}

fn validate_github_repository_name(name: &str) -> Result<(), AppError> {
    let valid_part = |part: &str| {
        !part.is_empty()
            && !part.starts_with('-')
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    let parts = name.split('/').collect::<Vec<_>>();
    if !(parts.len() == 1 || parts.len() == 2) || !parts.iter().all(|part| valid_part(part)) {
        return Err(AppError::Git(
            "GitHub 仓库名应为 repo 或 owner/repo，只能包含字母、数字、点、横线和下划线".into(),
        ));
    }
    Ok(())
}

/// 在项目根目录创建独立仓库；不会在检测到父级仓库时误操作父仓库。
pub fn initialize(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    match repository_root(&ctx.root)? {
        Some(root) if root == ctx.root => {
            return Err(AppError::Git("项目已经是 Git 仓库".into()));
        }
        Some(_) => {
            return Err(AppError::Git(
                "项目位于父级 Git 仓库中，CloudStack 不会在其中自动嵌套初始化".into(),
            ));
        }
        None => {}
    }
    let output = run_git(&ctx.root, &["init", "-b", "main"])?;
    let mut result = finish_operation(output, "Git 初始化失败");
    if result.succeeded() {
        if let Err(error) = ensure_local_config_excluded(ctx) {
            result.error = Some(format!("Git 已初始化，但无法设置本地配置排除：{error}"));
        }
    }
    Ok(result)
}

/// 把 CloudStack 配置加入当前仓库的本地 exclude，不修改共享 `.gitignore`。
pub fn ensure_local_config_excluded(ctx: &ProjectContext) -> Result<(), AppError> {
    match repository_root(&ctx.root)? {
        Some(root) if root == ctx.root => {}
        Some(_) => return Ok(()),
        None => return Ok(()),
    }
    let output = run_git(&ctx.root, &["rev-parse", "--git-path", "info/exclude"])?;
    if !output.status.success() {
        return Err(map_git_error(&output));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        ctx.root.join(path)
    };
    let existing = match fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let mut missing = [
        crate::services::project::CONFIG_FILE,
        crate::services::project::LEGACY_CONFIG_FILE,
    ]
    .into_iter()
    .filter(|entry| !existing.lines().any(|line| line.trim() == *entry))
    .peekable();
    if missing.peek().is_none() {
        return Ok(());
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }
    for entry in missing {
        writeln!(file, "{entry}")?;
    }
    file.sync_all()?;
    Ok(())
}

pub fn stop_tracking_project_config(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    if git_identity(&ctx.root)?.is_none() {
        return Err(AppError::Git(
            "停止跟踪配置需要创建一个本地提交，请先配置仓库提交身份".into(),
        ));
    }
    let staged = run_git(&ctx.root, &["diff", "--cached", "--name-only"])?;
    if !staged.status.success() {
        return Err(map_git_error(&staged));
    }
    let staged_paths = String::from_utf8_lossy(&staged.stdout);
    if staged_paths.lines().any(|path| !path.trim().is_empty()) {
        return Err(AppError::Git(
            "索引中已有其他暂存改动；为避免把它们带入配置移除提交，请先在终端处理".into(),
        ));
    }
    let output = run_git(
        &ctx.root,
        &[
            "rm",
            "--cached",
            "--ignore-unmatch",
            "--",
            active_config_name(ctx),
        ],
    )?;
    let mut result = finish_operation(output, "停止跟踪 CloudStack 配置失败");
    if !result.succeeded() {
        return Ok(result);
    }
    let commit = run_git(&ctx.root, &["commit", "-m", "停止跟踪 CloudStack 本地配置"])?;
    let commit = finish_operation(commit, "提交配置移除失败");
    result.report.traces.extend(commit.report.traces);
    result.error = commit.error;
    if result.error.is_some() {
        let restore = run_git(&ctx.root, &["reset", "--", active_config_name(ctx)])?;
        let restore = finish_operation(restore, "恢复配置索引失败");
        result.report.traces.extend(restore.report.traces);
        if let Some(error) = restore.error {
            let commit_error = result.error.take().unwrap_or_default();
            result.error = Some(format!("{commit_error}\n恢复配置索引也失败：{error}"));
        }
        return Ok(result);
    }
    if result.succeeded() {
        if let Err(error) = ensure_local_config_excluded(ctx) {
            result.error = Some(format!("配置已停止跟踪，但设置本地排除失败：{error}"));
        }
    }
    Ok(result)
}

pub fn configure_identity(
    ctx: &ProjectContext,
    name: &str,
    email: &str,
) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    let name = name.trim();
    let email = email.trim();
    if name.is_empty()
        || name.contains(['\n', '\r', '\0'])
        || email.is_empty()
        || email.contains(char::is_whitespace)
        || email.contains('\0')
        || !email.contains('@')
    {
        return Err(AppError::Git("Git 用户名或邮箱格式无效".into()));
    }

    let first = run_git(&ctx.root, &["config", "--local", "user.name", name])?;
    let mut result = finish_operation(first, "设置 Git 用户名失败");
    if !result.succeeded() {
        return Ok(result);
    }
    let second = run_git(&ctx.root, &["config", "--local", "user.email", email])?;
    let second = finish_operation(second, "设置 Git 邮箱失败");
    result.report.traces.extend(second.report.traces);
    result.error = second.error;
    Ok(result)
}

pub fn add_origin(ctx: &ProjectContext, url: &str) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    validate_remote_url(url)?;
    if remote_exists(&ctx.root, "origin")? {
        return Err(AppError::Git("origin 远端已经存在".into()));
    }
    let output = run_git(&ctx.root, &["remote", "add", "origin", url.trim()])?;
    Ok(finish_operation(output, "添加 origin 失败"))
}

pub fn create_github_repository(
    ctx: &ProjectContext,
    name: &str,
    visibility: RepositoryVisibility,
) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    validate_github_repository_name(name.trim())?;
    if !has_head(&ctx.root)? {
        return Err(AppError::Git("创建远端前必须先完成首次提交".into()));
    }
    if remote_exists(&ctx.root, "origin")? {
        return Err(AppError::Git("origin 远端已经存在".into()));
    }
    let environment = inspect_environment(&ctx.root);
    if !environment.gh_available {
        return Err(AppError::Git("未安装 GitHub CLI（gh）".into()));
    }
    if !environment.gh_authenticated {
        return Err(AppError::Git(
            "GitHub CLI 尚未登录，请先在终端执行 gh auth login".into(),
        ));
    }
    let visibility = match visibility {
        RepositoryVisibility::Private => "--private",
        RepositoryVisibility::Public => "--public",
    };
    let output = run_gh(
        &ctx.root,
        &[
            "repo",
            "create",
            name.trim(),
            visibility,
            "--source=.",
            "--remote=origin",
            "--push",
        ],
    )?;
    Ok(finish_operation(output, "GitHub 仓库创建或首次推送失败"))
}

pub fn setup_github_credentials(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    let environment = inspect_environment(&ctx.root);
    if !environment.gh_available || !environment.gh_authenticated {
        return Err(AppError::Git(
            "GitHub CLI 未安装或尚未登录，无法配置 HTTPS 凭据助手".into(),
        ));
    }
    let output = run_gh(
        &ctx.root,
        &["auth", "setup-git", "--hostname", "github.com"],
    )?;
    Ok(finish_operation(output, "GitHub 凭据助手配置失败"))
}

pub fn push_upstream(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    if !has_head(&ctx.root)? {
        return Err(AppError::Git("仓库尚无提交，不能推送".into()));
    }
    if !remote_exists(&ctx.root, "origin")? {
        return Err(AppError::Git("没有配置 origin 远端".into()));
    }
    let current = status(ctx)?;
    if current.upstream.is_some() {
        return Err(AppError::Git("当前分支已经配置 upstream".into()));
    }
    let branch = current
        .branch
        .ok_or_else(|| AppError::Git("分离 HEAD 状态不能设置 upstream".into()))?;
    let output = run_git(&ctx.root, &["push", "--set-upstream", "origin", &branch])?;
    Ok(finish_operation(output, "首次推送失败"))
}

pub fn fetch(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    let remotes = run_git(&ctx.root, &["remote"])?;
    if !remotes.status.success() || String::from_utf8_lossy(&remotes.stdout).trim().is_empty() {
        return Err(AppError::Git("没有可获取的远端".into()));
    }
    let output = run_git(&ctx.root, &["fetch", "--prune"])?;
    Ok(finish_operation(output, "获取远端状态失败"))
}

pub fn push(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    let current = status(ctx)?;
    if current.upstream.is_none() {
        return Err(AppError::Git("当前分支没有 upstream".into()));
    }
    if current.behind > 0 {
        return Err(AppError::Git(
            "远端包含本地没有的提交；为避免隐式合并，已拒绝推送".into(),
        ));
    }
    let output = run_git(&ctx.root, &["push"])?;
    Ok(finish_operation(output, "推送失败"))
}

pub fn pull_fast_forward(ctx: &ProjectContext) -> Result<GitOperationResult, AppError> {
    ensure_exact_repository(ctx)?;
    let current = status(ctx)?;
    if current.upstream.is_none() {
        return Err(AppError::Git("当前分支没有 upstream".into()));
    }
    if !current.changes.is_empty() {
        return Err(AppError::Git(
            "工作区存在改动；CloudStack 不会自动 stash，请先在终端处理".into(),
        ));
    }
    if current.ahead > 0 || current.behind == 0 {
        return Err(AppError::Git(
            "当前状态不满足纯 fast-forward 更新条件".into(),
        ));
    }
    let output = run_git(&ctx.root, &["pull", "--ff-only"])?;
    Ok(finish_operation(output, "快进同步失败"))
}

/// 依次 stage → commit → (push)，任何一步失败都停在那一步，
/// 已经完成的部分如实保留在结果里（不因为后面失败而抹掉前面的成功）。
pub fn publish(ctx: &ProjectContext, message: &str, push: bool) -> Result<PublishResult, AppError> {
    publish_internal(ctx, message, push, None)
}

pub fn publish_selected(
    ctx: &ProjectContext,
    message: &str,
    push: bool,
    selected_paths: &[String],
) -> Result<PublishResult, AppError> {
    publish_internal(ctx, message, push, Some(selected_paths))
}

fn publish_internal(
    ctx: &ProjectContext,
    message: &str,
    push: bool,
    selected_paths: Option<&[String]>,
) -> Result<PublishResult, AppError> {
    let current = status(ctx)?;
    let mut report = OperationReport::default();

    if current.behind > 0 {
        return Err(AppError::Git(
            "远端包含本地没有的提交；CloudStack 不会通过提交制造分叉，请先在终端处理本地改动"
                .into(),
        ));
    }

    let mut unmerged_paths: Vec<&str> = current
        .changes
        .iter()
        .filter(|change| change.kind == ChangeKind::Unmerged)
        .map(|change| change.path.as_str())
        .collect();
    if !unmerged_paths.is_empty() {
        unmerged_paths.sort_unstable();
        let paths = unmerged_paths.join("、");
        return Ok(PublishResult {
            staged: false,
            staged_files: vec![],
            committed: false,
            commit_hash: None,
            pushed: false,
            error_stage: Some("stage".into()),
            error: Some(ErrorPayload::git_unresolved_conflicts(
                format!("存在未解决的 Git 冲突，发布已中止：{paths}"),
                paths,
            )),
            report,
        });
    }

    let managed_scope = match selected_paths {
        Some(paths) => ManagedScope::from_selected(ctx, &current, paths)?,
        None => ManagedScope::from_status(ctx, &current),
    };
    let managed_paths = managed_scope.paths().to_vec();

    if managed_scope.is_empty() {
        return Ok(PublishResult {
            staged: false,
            staged_files: vec![],
            committed: false,
            commit_hash: None,
            pushed: false,
            error_stage: Some("stage".into()),
            error: Some(ErrorPayload::git_nothing_to_commit("没有可提交的改动")),
            report,
        });
    }

    let mut add_args = vec!["add", "--"];
    add_args.extend(managed_paths.iter().map(String::as_str));
    let add_output = run_git(&ctx.root, &add_args)?;
    report.traces.push(add_output.trace.clone());
    if !add_output.status.success() {
        let detail = git_error_message(&add_output);
        return Ok(PublishResult {
            staged: false,
            staged_files: vec![],
            committed: false,
            commit_hash: None,
            pushed: false,
            error_stage: Some("stage".into()),
            error: Some(ErrorPayload::git_stage_failed(
                format!("暂存失败：{detail}"),
                detail,
            )),
            report,
        });
    }

    let mut commit_args = vec!["commit", "-m", message, "--"];
    commit_args.extend(managed_paths.iter().map(String::as_str));
    let commit_output = run_git(&ctx.root, &commit_args)?;
    report.traces.push(commit_output.trace.clone());
    if !commit_output.status.success() {
        let detail = git_error_message(&commit_output);
        return Ok(PublishResult {
            staged: true,
            staged_files: managed_paths,
            committed: false,
            commit_hash: None,
            pushed: false,
            error_stage: Some("commit".into()),
            error: Some(ErrorPayload::git_commit_failed(
                format!("提交失败：{detail}"),
                detail,
            )),
            report,
        });
    }

    let commit_hash = current_commit_hash(&ctx.root);

    if !push {
        return Ok(PublishResult {
            staged: true,
            staged_files: managed_paths,
            committed: true,
            commit_hash,
            pushed: false,
            error_stage: None,
            error: None,
            report,
        });
    }

    let push_output = run_git(&ctx.root, &["push"])?;
    report.traces.push(push_output.trace.clone());
    if !push_output.status.success() {
        let stderr = String::from_utf8_lossy(&push_output.stderr);
        return Ok(PublishResult {
            staged: true,
            staged_files: managed_paths,
            committed: true,
            commit_hash,
            pushed: false,
            error_stage: Some("push".into()),
            error: Some(classify_push_error(&stderr)),
            report,
        });
    }

    Ok(PublishResult {
        staged: true,
        staged_files: managed_paths,
        committed: true,
        commit_hash,
        pushed: true,
        error_stage: None,
        error: None,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectConfig;
    use std::path::PathBuf;

    fn porcelain(parts: &[&str]) -> String {
        let mut s = parts.join("\0");
        s.push('\0');
        s
    }

    #[test]
    fn parses_branch_header_and_modified_file() {
        let text = porcelain(&[
            "# branch.oid abc123",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 h1 h2 src/content/blog/a.md",
        ]);
        let status = parse_porcelain_v2(&text, "src/content/blog");
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert_eq!(status.changes.len(), 1);
        let c = &status.changes[0];
        assert_eq!(c.path, "src/content/blog/a.md");
        assert_eq!(c.kind, ChangeKind::Modified);
        assert!(!c.staged);
        assert!(c.managed);
    }

    #[test]
    fn distinguishes_staged_and_managed_scope() {
        let text = porcelain(&[
            "# branch.head main",
            "1 A. N... 000000 100644 100644 000000 h1 src/content/blog/new.md",
            "? src/content/blog/draft.md",
            "1 .M N... 100644 100644 100644 h1 h2 .cloudstack.json",
            "1 .M N... 100644 100644 100644 h1 h2 README.md",
        ]);
        let status = parse_porcelain_v2(&text, "src/content/blog");
        let by_path = |p: &str| status.changes.iter().find(|c| c.path == p).unwrap();

        let added = by_path("src/content/blog/new.md");
        assert_eq!(added.kind, ChangeKind::Added);
        assert!(added.staged);
        assert!(added.managed);

        let untracked = by_path("src/content/blog/draft.md");
        assert_eq!(untracked.kind, ChangeKind::Untracked);
        assert!(untracked.managed);

        assert!(!by_path(".cloudstack.json").managed);

        let readme = by_path("README.md");
        assert!(!readme.managed);
    }

    #[test]
    fn treats_all_cloudstack_config_files_as_local_only() {
        let text = porcelain(&[
            "? .cloudstack.json",
            "1 .M N... 100644 100644 100644 h1 h2 .blog-editor.json",
        ]);
        let status = parse_porcelain_v2(&text, "src/content/blog");
        let by_path = |path: &str| status.changes.iter().find(|c| c.path == path).unwrap();

        assert!(!by_path(".cloudstack.json").managed);
        assert!(!by_path(".blog-editor.json").managed);
    }

    #[test]
    fn parses_renamed_entry() {
        let text = porcelain(&[
            "# branch.head main",
            "2 R. N... 100644 100644 100644 h1 h2 R100 src/content/blog/new-name.md",
            "src/content/blog/old-name.md",
        ]);
        let status = parse_porcelain_v2(&text, "src/content/blog");
        assert_eq!(status.changes.len(), 1);
        let c = &status.changes[0];
        assert_eq!(c.path, "src/content/blog/new-name.md");
        assert_eq!(c.old_path.as_deref(), Some("src/content/blog/old-name.md"));
        assert_eq!(c.kind, ChangeKind::Renamed);
    }

    #[test]
    fn fresh_repo_without_upstream_defaults_gracefully() {
        let text = porcelain(&[
            "# branch.oid (initial)",
            "# branch.head main",
            "? src/content/blog/a.md",
        ]);
        let status = parse_porcelain_v2(&text, "src/content/blog");
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream, None);
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn detached_head_has_no_branch_name() {
        let text = porcelain(&["# branch.head (detached)"]);
        let status = parse_porcelain_v2(&text, "src/content/blog");
        assert_eq!(status.branch, None);
    }

    #[test]
    fn command_trace_redacts_url_credentials_before_storage() {
        let text =
            "fatal: https://alice:secret@example.com/org/repo.git rejected; token gho_abc123";
        let redacted = redact_secrets(text);
        assert_eq!(
            redacted,
            "fatal: https://***@example.com/org/repo.git rejected; token ***"
        );
        assert!(!redacted.contains("alice"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn displayed_commands_include_non_interactive_terminal_policy() {
        let command = display_command("git", &["commit", "-m", "更新 文章"]);
        assert_eq!(
            command,
            "GIT_TERMINAL_PROMPT=0 SSH_ASKPASS_REQUIRE=never git commit -m '更新 文章'"
        );
    }

    #[test]
    fn validates_supported_remote_urls_without_embedded_https_credentials() {
        assert!(validate_remote_url("git@github.com:user/repo.git").is_ok());
        assert!(validate_remote_url("ssh://git@github.com/user/repo.git").is_ok());
        assert!(validate_remote_url("https://github.com/user/repo.git").is_ok());
        assert!(validate_remote_url("https://user:token@github.com/user/repo.git").is_err());
        assert!(validate_remote_url("ssh://user:password@github.com/user/repo.git").is_err());
        assert!(validate_remote_url("file:///tmp/repo.git").is_err());
        assert!(validate_remote_url("--upload-pack=bad").is_err());
    }

    #[test]
    fn validates_non_interactive_github_repository_names() {
        assert!(validate_github_repository_name("notes").is_ok());
        assert!(validate_github_repository_name("owner/cloud-stack").is_ok());
        assert!(validate_github_repository_name("owner/repo/extra").is_err());
        assert!(validate_github_repository_name("-danger").is_err());
        assert!(validate_github_repository_name("owner/repo name").is_err());
    }

    #[test]
    fn managed_scope_excludes_remembered_articles_and_their_assets() {
        let root = PathBuf::from("/tmp/cloudstack-scope-test");
        let mut config = ProjectConfig::default();
        config.git.excluded_articles = vec!["draft.md".into()];
        let ctx = ProjectContext {
            content_root: root.join("src/content/blog"),
            config_path: root.join(".cloudstack.json"),
            root,
            config,
        };
        let status = parse_porcelain_v2(
            &porcelain(&[
                "? src/content/blog/draft.md",
                "? src/content/blog/draft/photo.png",
                "? src/content/blog/published.md",
            ]),
            "src/content/blog",
        );

        assert_eq!(
            ManagedScope::from_status(&ctx, &status).paths(),
            ["src/content/blog/published.md"]
        );
        assert!(ManagedScope::from_selected(&ctx, &status, &["README.md".to_string()]).is_err());
    }

    #[test]
    fn outer_article_exclusion_does_not_hide_a_nested_article_or_its_assets() {
        let dir = tempfile::tempdir().unwrap();
        let content_root = dir.path().join("src/content/blog");
        std::fs::create_dir_all(content_root.join("hello/nested")).unwrap();
        std::fs::write(content_root.join("hello.md"), "outer").unwrap();
        std::fs::write(content_root.join("hello/nested.md"), "nested").unwrap();
        std::fs::write(content_root.join("hello/nested/photo.png"), "image").unwrap();
        let mut config = ProjectConfig::default();
        config.git.excluded_articles = vec!["hello.md".into()];
        let ctx = ProjectContext {
            root: dir.path().to_path_buf(),
            content_root,
            config_path: dir.path().join(".cloudstack.json"),
            config,
        };
        let status = parse_porcelain_v2(
            &porcelain(&[
                "? src/content/blog/hello.md",
                "? src/content/blog/hello/outer.png",
                "? src/content/blog/hello/nested.md",
                "? src/content/blog/hello/nested/photo.png",
            ]),
            "src/content/blog",
        );

        assert_eq!(
            ManagedScope::from_status(&ctx, &status).paths(),
            [
                "src/content/blog/hello/nested.md",
                "src/content/blog/hello/nested/photo.png",
            ]
        );
    }

    // ---- 以下用真实 git 仓库跑集成测试 ----

    fn run(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} 失败");
    }

    fn commit_all(root: &Path, message: &str) {
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", message]);
    }

    fn init_repo() -> (tempfile::TempDir, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        // 测试仓库必须自带身份与签名策略，不能依赖开发机 ~/.gitconfig；CI 的
        // 干净 Arch 用户没有全局身份，而 publish() 内部会执行真实 git commit。
        run(&root, &["config", "user.name", "CloudStack Test"]);
        run(
            &root,
            &["config", "user.email", "cloudstack-test@example.invalid"],
        );
        run(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(root.join("src/content/blog")).unwrap();
        let ctx = ProjectContext {
            root: root.clone(),
            content_root: root.join("src/content/blog"),
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig {
                content_dir: "src/content/blog".into(),
                ..ProjectConfig::default()
            },
        };
        (dir, ctx)
    }

    fn init_bare(dir: &Path) -> PathBuf {
        let canon = dir.canonicalize().unwrap();
        run(&canon, &["init", "-q", "--bare"]);
        canon
    }

    #[test]
    fn status_on_fresh_repo_shows_untracked_managed_file() {
        let (_dir, ctx) = init_repo();
        std::fs::write(ctx.content_root.join("a.md"), "---\ntitle: a\n---\nbody").unwrap();

        let status = super::status(&ctx).unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream, None);
        let a = status
            .changes
            .iter()
            .find(|c| c.path == "src/content/blog/a.md")
            .unwrap();
        assert_eq!(a.kind, ChangeKind::Untracked);
        assert!(a.managed);
    }

    #[test]
    fn snapshot_represents_a_non_git_project_without_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let content_root = root.join("src/content/blog");
        std::fs::create_dir_all(&content_root).unwrap();
        let ctx = ProjectContext {
            root: root.clone(),
            content_root,
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig::default(),
        };
        let current = snapshot(&ctx).unwrap();
        assert_eq!(current.topology, RepositoryTopology::NotInitialized);
        assert_eq!(current.sync, SyncRelation::Unknown);
        assert!(current.worktree.is_clean());
    }

    #[test]
    fn snapshot_redacts_credentials_from_existing_remote_urls() {
        let (_dir, ctx) = init_repo();
        run(
            &ctx.root,
            &[
                "remote",
                "add",
                "origin",
                "https://alice:secret@example.com/org/repo.git",
            ],
        );

        let current = snapshot(&ctx).unwrap();
        assert_eq!(current.remotes.len(), 1);
        assert_eq!(current.remotes[0].name, "origin");
        assert_eq!(
            current.remotes[0].url.as_deref(),
            Some("https://***@example.com/org/repo.git")
        );
        assert!(!current.remotes[0]
            .url
            .as_deref()
            .unwrap()
            .contains("secret"));
    }

    #[test]
    fn initialize_creates_an_independent_main_repository_and_trace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let content_root = root.join("src/content/blog");
        std::fs::create_dir_all(&content_root).unwrap();
        let ctx = ProjectContext {
            root: root.clone(),
            content_root,
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig::default(),
        };

        let result = initialize(&ctx).unwrap();
        assert!(result.succeeded(), "{:?}", result.error);
        assert_eq!(result.report.traces.len(), 1);
        assert!(result.report.traces[0].command.contains(" init -b main"));
        assert!(root.join(".git").is_dir());
        let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
        assert!(exclude.lines().any(|line| line == ".cloudstack.json"));
        assert!(exclude.lines().any(|line| line == ".blog-editor.json"));
        assert_eq!(
            snapshot(&ctx).unwrap().topology,
            RepositoryTopology::NoCommit
        );

        let identity = configure_identity(&ctx, "CloudStack User", "user@example.invalid").unwrap();
        assert!(identity.succeeded());
        assert_eq!(identity.report.traces.len(), 2);
        assert_eq!(
            snapshot(&ctx).unwrap().identity,
            Some(GitIdentity {
                name: "CloudStack User".into(),
                email: "user@example.invalid".into(),
            })
        );
    }

    #[test]
    fn snapshot_does_not_treat_a_parent_repository_as_the_project_repository() {
        let (dir, _parent_context) = init_repo();
        let root = dir.path().join("nested-project");
        let content_root = root.join("src/content/blog");
        std::fs::create_dir_all(&content_root).unwrap();
        let root = root.canonicalize().unwrap();
        let ctx = ProjectContext {
            root: root.clone(),
            content_root,
            config_path: root.join(".cloudstack.json"),
            config: ProjectConfig::default(),
        };

        let current = snapshot(&ctx).unwrap();
        assert_eq!(current.topology, RepositoryTopology::ParentRepository);
        assert!(status(&ctx).is_err());
        assert!(initialize(&ctx).is_err());
        assert!(!root.join(".git").exists());
    }

    #[test]
    fn snapshot_keeps_topology_sync_and_worktree_as_separate_dimensions() {
        let (_dir, ctx) = init_repo();
        let initial = snapshot(&ctx).unwrap();
        assert_eq!(initial.topology, RepositoryTopology::NoCommit);
        assert_eq!(initial.sync, SyncRelation::Unknown);
        assert!(initial.worktree.is_clean());

        std::fs::write(ctx.content_root.join("a.md"), "base\n").unwrap();
        std::fs::write(ctx.root.join("README.md"), "base\n").unwrap();
        commit_all(&ctx.root, "init");
        assert_eq!(
            snapshot(&ctx).unwrap().topology,
            RepositoryTopology::NoRemote
        );

        let origin_dir = tempfile::tempdir().unwrap();
        let origin_path = init_bare(origin_dir.path());
        run(
            &ctx.root,
            &["remote", "add", "origin", origin_path.to_str().unwrap()],
        );
        assert_eq!(
            snapshot(&ctx).unwrap().topology,
            RepositoryTopology::NoUpstream
        );
        run(&ctx.root, &["push", "-q", "-u", "origin", "main"]);

        std::fs::write(ctx.content_root.join("a.md"), "managed\n").unwrap();
        std::fs::write(ctx.root.join("README.md"), "unmanaged\n").unwrap();
        let current = snapshot(&ctx).unwrap();
        assert_eq!(current.topology, RepositoryTopology::Tracking);
        assert_eq!(current.sync, SyncRelation::Synced);
        assert_eq!(current.worktree.managed_changes, 1);
        assert_eq!(current.worktree.unmanaged_changes, 1);
    }

    #[test]
    fn publish_commits_only_managed_files() {
        let (_dir, ctx) = init_repo();
        std::fs::write(ctx.root.join("README.md"), "hello\n").unwrap();
        std::fs::write(ctx.root.join(".cloudstack.json"), "{\"version\":1}\n").unwrap();
        std::fs::write(ctx.content_root.join("a.md"), "original\n").unwrap();
        commit_all(&ctx.root, "init");

        std::fs::write(ctx.root.join("README.md"), "changed outside\n").unwrap();
        std::fs::write(
            ctx.root.join(".cloudstack.json"),
            "{\"version\":1,\"assets\":{\"mode\":\"colocated\"}}\n",
        )
        .unwrap();
        std::fs::write(ctx.content_root.join("a.md"), "changed inside\n").unwrap();

        let result = publish(&ctx, "更新文章", false).unwrap();
        assert!(result.staged);
        assert!(result.committed);
        assert!(result.commit_hash.is_some());
        assert!(!result.pushed);
        assert_eq!(result.error_stage, None);
        assert_eq!(result.report.traces.len(), 2);
        assert!(result.report.traces.iter().all(|trace| trace.success));
        assert!(result.report.traces[0].command.contains(" add --"));
        assert!(result.report.traces[1].command.contains(" commit -m"));
        assert_eq!(
            result.staged_files,
            vec!["src/content/blog/a.md".to_string()]
        );

        let show = Command::new("git")
            .current_dir(&ctx.root)
            .args(["show", "HEAD:README.md"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&show.stdout), "hello\n");

        let status_after = Command::new("git")
            .current_dir(&ctx.root)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&status_after.stdout);
        assert!(s.contains("README.md"));
        assert!(s.contains(".cloudstack.json"));
        assert!(!s.contains("a.md"));
    }

    #[test]
    fn publish_selected_commits_only_the_chosen_article_and_assets() {
        let (_dir, ctx) = init_repo();
        std::fs::create_dir_all(ctx.content_root.join("a")).unwrap();
        std::fs::write(ctx.content_root.join("a.md"), "a0\n").unwrap();
        std::fs::write(ctx.content_root.join("a/photo.png"), "a0\n").unwrap();
        std::fs::write(ctx.content_root.join("b.md"), "b0\n").unwrap();
        commit_all(&ctx.root, "init");

        std::fs::write(ctx.content_root.join("a.md"), "a1\n").unwrap();
        std::fs::write(ctx.content_root.join("a/photo.png"), "a1\n").unwrap();
        std::fs::write(ctx.content_root.join("b.md"), "b1\n").unwrap();
        let selected = vec![
            "src/content/blog/a.md".to_string(),
            "src/content/blog/a/photo.png".to_string(),
        ];

        let result = publish_selected(&ctx, "只更新 A", false, &selected).unwrap();
        assert!(result.committed);
        assert_eq!(result.staged_files, selected);
        let remaining = status(&ctx).unwrap();
        assert_eq!(
            remaining
                .changes
                .iter()
                .filter(|change| change.managed)
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["src/content/blog/b.md"]
        );
    }

    #[test]
    fn stop_tracking_config_keeps_the_file_and_commits_its_removal() {
        let (_dir, ctx) = init_repo();
        std::fs::write(
            &ctx.config_path,
            "{\"version\":1,\"contentDir\":\"src/content/blog\"}\n",
        )
        .unwrap();
        std::fs::write(ctx.content_root.join("a.md"), "a\n").unwrap();
        commit_all(&ctx.root, "init");
        assert!(snapshot(&ctx).unwrap().config_tracked);

        let result = stop_tracking_project_config(&ctx).unwrap();
        assert!(result.succeeded(), "{:?}", result.error);
        assert_eq!(result.report.traces.len(), 2);
        assert!(ctx.config_path.is_file());
        assert!(!snapshot(&ctx).unwrap().config_tracked);
        let exclude = std::fs::read_to_string(ctx.root.join(".git/info/exclude")).unwrap();
        assert!(exclude.lines().any(|line| line == ".cloudstack.json"));
        let tree = Command::new("git")
            .current_dir(&ctx.root)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&tree.stdout).contains(".cloudstack.json"));
    }

    #[test]
    fn stop_tracking_config_without_identity_never_changes_the_index() {
        let (_dir, ctx) = init_repo();
        std::fs::write(
            &ctx.config_path,
            "{\"version\":1,\"contentDir\":\"src/content/blog\"}\n",
        )
        .unwrap();
        commit_all(&ctx.root, "init");
        run(&ctx.root, &["config", "user.name", ""]);
        run(&ctx.root, &["config", "user.email", ""]);

        assert!(stop_tracking_project_config(&ctx).is_err());
        assert!(snapshot(&ctx).unwrap().config_tracked);
        assert!(ctx.config_path.is_file());
    }

    #[test]
    fn publish_with_no_changes_reports_stage_error_without_running_git() {
        let (_dir, ctx) = init_repo();
        std::fs::write(ctx.content_root.join("a.md"), "x\n").unwrap();
        commit_all(&ctx.root, "init");

        let result = publish(&ctx, "空提交", false).unwrap();
        assert!(!result.staged);
        assert!(!result.committed);
        assert_eq!(result.error_stage.as_deref(), Some("stage"));
        assert_eq!(
            result.error.as_ref().map(ErrorPayload::code),
            Some("git_nothing_to_commit")
        );
    }

    #[test]
    fn publish_rejects_unmerged_files_without_changing_the_index() {
        let (_dir, ctx) = init_repo();
        let article = ctx.content_root.join("a.md");
        std::fs::write(&article, "base\n").unwrap();
        commit_all(&ctx.root, "base");

        run(&ctx.root, &["checkout", "-q", "-b", "other"]);
        std::fs::write(&article, "other\n").unwrap();
        commit_all(&ctx.root, "other");
        run(&ctx.root, &["checkout", "-q", "main"]);
        std::fs::write(&article, "main\n").unwrap();
        commit_all(&ctx.root, "main");

        let merge = Command::new("git")
            .current_dir(&ctx.root)
            .args(["merge", "other"])
            .output()
            .unwrap();
        assert!(!merge.status.success());

        let unmerged_before = run_git(&ctx.root, &["ls-files", "-u"]).unwrap().stdout;
        let result = publish(&ctx, "不应提交", false).unwrap();
        let unmerged_after = run_git(&ctx.root, &["ls-files", "-u"]).unwrap().stdout;

        assert!(!result.staged);
        assert!(!result.committed);
        assert_eq!(result.error_stage.as_deref(), Some("stage"));
        assert_eq!(
            result.error.as_ref().map(ErrorPayload::code),
            Some("git_unresolved_conflicts")
        );
        assert_eq!(unmerged_after, unmerged_before);
    }

    #[test]
    fn publish_push_succeeds_against_local_bare_remote() {
        let (_dir, ctx) = init_repo();
        let origin_dir = tempfile::tempdir().unwrap();
        let origin_path = init_bare(origin_dir.path());
        run(
            &ctx.root,
            &["remote", "add", "origin", origin_path.to_str().unwrap()],
        );
        std::fs::write(ctx.content_root.join("a.md"), "x\n").unwrap();
        commit_all(&ctx.root, "init");
        run(&ctx.root, &["push", "-q", "-u", "origin", "main"]);

        std::fs::write(ctx.content_root.join("a.md"), "y\n").unwrap();
        let result = publish(&ctx, "更新", true).unwrap();
        assert!(result.staged);
        assert!(result.committed);
        assert!(result.pushed);
        assert_eq!(result.error_stage, None);
    }

    #[test]
    fn fetch_and_fast_forward_pull_never_merge_or_touch_a_dirty_worktree() {
        let (_dir, ctx) = init_repo();
        let origin_dir = tempfile::tempdir().unwrap();
        let origin_path = init_bare(origin_dir.path());
        run(
            &ctx.root,
            &["remote", "add", "origin", origin_path.to_str().unwrap()],
        );
        std::fs::write(ctx.content_root.join("a.md"), "base\n").unwrap();
        commit_all(&ctx.root, "base");
        run(&ctx.root, &["push", "-q", "-u", "origin", "main"]);

        let contributor_dir = tempfile::tempdir().unwrap();
        run(
            contributor_dir.path(),
            &[
                "clone",
                "-q",
                "--branch",
                "main",
                origin_path.to_str().unwrap(),
                "clone",
            ],
        );
        let contributor = contributor_dir.path().join("clone");
        run(&contributor, &["config", "user.name", "Contributor"]);
        run(
            &contributor,
            &["config", "user.email", "contributor@example.invalid"],
        );
        std::fs::write(contributor.join("src/content/blog/a.md"), "remote\n").unwrap();
        commit_all(&contributor, "remote");
        run(&contributor, &["push", "-q"]);

        let fetched = fetch(&ctx).unwrap();
        assert!(fetched.succeeded());
        assert!(fetched.report.traces[0].command.contains(" fetch --prune"));
        assert_eq!(status(&ctx).unwrap().behind, 1);

        let article = ctx.content_root.join("a.md");
        std::fs::write(&article, "local dirty\n").unwrap();
        assert!(publish(&ctx, "must not diverge", false).is_err());
        std::fs::write(&article, "base\n").unwrap();

        let unrelated = ctx.root.join("README.local");
        std::fs::write(&unrelated, "dirty\n").unwrap();
        assert!(pull_fast_forward(&ctx).is_err());
        std::fs::remove_file(unrelated).unwrap();

        let pulled = pull_fast_forward(&ctx).unwrap();
        assert!(pulled.succeeded());
        assert!(pulled.report.traces[0].command.contains(" pull --ff-only"));
        assert_eq!(
            std::fs::read_to_string(ctx.content_root.join("a.md")).unwrap(),
            "remote\n"
        );
    }

    #[test]
    fn publish_push_without_upstream_reports_friendly_message() {
        let (_dir, ctx) = init_repo();
        let origin_dir = tempfile::tempdir().unwrap();
        let origin_path = init_bare(origin_dir.path());
        run(
            &ctx.root,
            &["remote", "add", "origin", origin_path.to_str().unwrap()],
        );

        std::fs::write(ctx.content_root.join("a.md"), "x\n").unwrap();
        let result = publish(&ctx, "首次提交", true).unwrap();
        assert!(result.committed);
        assert!(!result.pushed);
        assert_eq!(result.error_stage.as_deref(), Some("push"));
        assert_eq!(
            result.error.as_ref().map(ErrorPayload::code),
            Some("git_push_no_upstream")
        );

        let pushed = push_upstream(&ctx).unwrap();
        assert!(pushed.succeeded());
        assert!(pushed.report.traces[0]
            .command
            .contains(" push --set-upstream origin main"));
        assert_eq!(
            status(&ctx).unwrap().upstream.as_deref(),
            Some("origin/main")
        );
    }
}
