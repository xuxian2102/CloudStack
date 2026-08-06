# 可靠性待办（源自 v0.2.4 第三方审阅，2026-08-06）

审阅对象：`main` HEAD `274aca3`（v0.2.4）。审阅结论中的关键技术断言已逐条核实（三路并行只读核实，见下方各条的"证据"），全部属实，未发现被证伪的条目。这里只记录待办本身，完整审阅原文不在此复述。

状态说明：`[ ]` 待处理 · `[x]` 已完成 · 每条尽量给出下一次实现时该看的入口文件/函数。

## P1（建议在下一个强调可靠性的版本前修复）

全部 5 项已修复（分四轮实施，`main` 在 v0.2.4 之后的提交里）。规划见 `/home/xuxian/.claude/plans/stateful-toasting-key.md`。

- [x] **Git 状态刷新缺少 generation/token 校验，存在乱序覆盖窗口**（第 1 轮）
  `EditorState` 加 `git_refresh_generation: u64`；`git_panel::refresh()` 完成回调改用新增的纯函数 `app::should_apply_git_refresh`（`crates/cloudstack-gtk/src/app/git_refresh.rs`）同时校验 root 与 generation。

- [x] **Git porcelain v2 解析：冲突路径（`u` 记录）遇空格截断** + **有损 UTF-8 转换回传 git 当 pathspec**（第 2 轮，合并成一次改动，同一处代码）
  `parse_porcelain_v2` 改为 `(&[u8], &str) -> Result<GitStatus, AppError>`，全程按字节切分，只在放进 `FileChange`/分支名时严格 `str::from_utf8`，失败即拒绝（新增 `git::UNSUPPORTED_PATH_ENCODING_ERROR`，GTK 侧有专门的 `UiMessage::GitErrorUnsupportedPathEncoding` 提示，不止是日志）。`u` 记录改成固定字段数解析。`status()` 不再自己 `from_utf8_lossy`。范围按讨论结果收敛为"1+"：没有做 `FileChange`/`ManagedScope` 全链路 `OsString` 化（那是明显更大的一次改动，评估后判断当前不值得，见下方"未来再考虑"）。

- [x] **图片导入未做内容级验证，允许任意字节伪装成图片**（第 3 轮）
  新增 `SupportedImageFormat` 枚举统一原来三处分散的扩展名清单；`save_image` 两个分支（拖拽/剪贴板）都强制内容嗅探，无法识别直接拒绝；扩展名与内容不符时改名而不是报错；`read_image_asset` 改用 `Read::take` 有界读取替代"`fs::metadata` 查大小→整体 `fs::read`"，content-type 由嗅探结果决定；`content_matches_pending_revision` 改成有界流式哈希。彻底移除 SVG 支持（`infer` 无法可靠嗅探 SVG，且项目里没有真实 SVG 用例）。

- [x] **文章重命名不是崩溃安全的事务**（第 4 轮，四轮里最大的一块）
  新增 `crates/cloudstack-core/src/services/operations.rs`：`rename_post` 在开始移动任何文件之前把操作意图（old_id/new_id/asset_moves）写入 fsync 过的 journal；崩溃恢复走 forward-only（不做回滚）——图片移动、文章重命名两步都幂等地"看当前状态补没做的部分"，正文重写复用 `rewrite_colocated_image_paths` 天然的幂等性。in-process 失败时仍走原有的 best-effort 回滚，只有回滚失败才保留 journal 交给下次启动恢复。恢复挂在 `open_project` 里，列出文章之前跑，成功后 toast 提示用户。
  **范围收敛**：只覆盖 `rename_post`，不含 `delete_post`（已经走系统回收站，本身可恢复，风险明显更低，留给以后需要时再扩展 `operations.rs` 加 `DeleteOperation`）。
  **测试缺口**：`rollback_asset_moves` 本身失败（"回滚的回滚也失败"）这个分支的自动化测试没有写——工程上很难在不侵入代码的前提下可靠地只让"回滚"这一步失败而不影响它要回滚的那次"正向"操作（两者需要同一目录的写权限，无法用静态 chmod 区分方向，又没有代码钩子能在函数执行中途插入扰动）。逻辑本身是一个简单的布尔与运算，人工审阅可信度足够高，但如果以后要重构这块代码建议先补一个依赖注入点再补测试。

## P2（代码质量与长期可维护性，不紧急）

- [ ] **层内模块继续变大**：`git.rs`（72K）、`posts.rs`/`assets.rs`（各 40K）体量已大；`window.rs`（72K）虽已拆出 `window/{articles,frontmatter,publish,git_panel,drafts,recent,settings,welcome}.rs` 等子模块，但 `window.rs` 本体和 `git_panel.rs`（1716 行）、`drafts.rs`（1047 行）仍偏大，可按职责继续下沉（如 `services/git/{command,status,scope,publish,remote}.rs`）。
- [ ] **结构化错误未贯彻到底**：`AppError::Io(String)`/`Git(String)`/`Config(String)` 等仍用字符串包裹；stage/commit/push 的错误 detail 可能从原始 stderr 直接构造，脱敏入口不统一。方向：细化 `GitError`/`FileError` 变体，所有 stderr 在进入 trace/payload/日志前统一脱敏。
- [ ] **大量路径暂存可能触发 ARG_MAX**：`add_args`/`commit_args`（`git.rs` 约 L1181-1182, 1202-1203）把所有路径放进 argv。方向：`git add --pathspec-from-file=- --pathspec-file-nul` 走 stdin；commit message 增加非空/无 NUL/长度上限校验（目前 `message: &str` 直接拼进 `commit_args`，约 L1202，完全无校验）。
- [ ] **路径校验仍有本地 TOCTOU 窗口**：canonicalize/符号链接检查与实际打开文件之间可能被本地进程替换。方向（仅 Linux 目标可行）：`openat2` + `RESOLVE_BENEATH`/`RESOLVE_NO_SYMLINKS`，或 `cap-std`/`rustix` 能力目录。
- [ ] **缺少三类专门测试**：① rename 各阶段的故障注入测试（模拟崩溃/重启后验证一致性）；② porcelain 解析、Markdown URL 过滤、frontmatter lossless 修改、图片路径 percent-decode、重命名路径改写的 fuzz/property test；③ MSRV 独立 CI 任务、`cargo deny`、GitHub Actions 完整 SHA 固定。

## 建议实施顺序（原审阅给出，供参考）

1. `fix(git): byte-safe porcelain v2 parser`
2. `fix(gtk): reject stale Git snapshot completion`
3. `hardening(assets): bounded reads and decoded image validation`
4. `feat(core): durable rename operation journal`
5. `refactor(core): split git/posts/assets services by responsibility`
6. `test(core): failure injection and parser fuzz targets`
7. `ci: MSRV, dependency policy and immutable action pins`
8. `feat(core): project file monitor and external-change workflow`


## 实际体验缺少的功能

md文件可以拖拽可以复制到当前工作文件夹

## 未来版本路线预测（原审阅给出，方向性建议，未核实，仅供参考）

审阅认为应先把上面 P1 做完（即 v0.3 = Reliability Release），再考虑功能扩展；插件系统、任意 JS 扩展、实时远程图片、跨平台目前都不建议做（README 已明确排除，也是当前代码质量高的原因之一）。

### v0.3：可靠性版本
不上插件/跨平台/复杂渲染，先把现有基础做可靠：

| 功能 | 价值 | 对底层的要求 |
| --- | --- | --- |
| 操作恢复中心 | 崩溃后恢复重命名、删除、图片导入 | Durable operation journal |
| 外部文件监控 | 及时发现其他编辑器修改 | GIO FileMonitor + revision |
| 冲突比较界面 | 提供"重载、保留、另存、对比" | 文本 diff 和明确状态机 |
| 安全图片管线 | 验证、压缩、旋转、去 metadata | 解码/重新编码 |
| 统一任务票据 | 消除所有旧异步结果回跳 | ProjectEpoch + RequestId |
| 保留换行风格 | 避免 CRLF 文件保存后统一变 LF | 文档模型记录 EOL |

其中"外部修改对比"优先级高于单纯报错——revision 校验已能阻止覆盖，但用户还需要知道接下来怎么处理。

### v0.4：写作效率版本
- **全项目搜索与索引**：正文全文搜索、标题/标签/日期/draft 筛选、结果片段与行号、最近编辑/未发布筛选。数据量小先用后台扫描，项目大了再上 Tantivy 或 SQLite FTS。
- **大纲、链接与知识结构**：当前文章标题大纲（点击跳转）、Markdown 内部链接补全、反向链接、断链检查、缺失图片检查、未使用图片检测。建议在 core 加一个只读 `ProjectIndex`，不要让 GTK 层自己反复扫描文件。
- **新文章模板与 Frontmatter schema 增强**：在现有 string/date/boolean/tags 基础上加 enum、number、string list、最小/最大长度、正则/slug 规则、日期允许未来值、字段间约束、不同目录用不同模板；新建文章可选"空白/博客文章/课程笔记/实验报告/自定义模板"。当前日期控件硬编码 2000 年至今天，限制了预约发布和旧文档迁移，建议改成可配置约束。
- **本地版本历史**：不靠自动 Git commit，而是在应用数据目录维护有限快照（每篇最多 N 个、按时间+内容哈希去重、可视化 diff、一键恢复为新编辑版本、总空间上限自动清理）。与草稿/Git 分工：草稿=崩溃恢复，本地历史=用户主动回看，Git=正式发布历史。

### v0.5：发布工作流版本
- **发布前 diff**：每篇文章 Markdown diff、新增/修改/删除的图片、将被提交的精确路径、非受管改动为何不会被提交、push 前 ahead/behind 状态。
- **提交历史与安全恢复**：当前分支最近提交、某次提交改了哪些文章、查看历史版本、"恢复此文章内容为新工作区修改"；不提供危险的 hard reset / 强推按钮。
- **声明式站点适配器**：支持 Astro/Hugo/Zola/Jekyll 等，用声明式配置（`site.type` / `previewCommand` / `buildCommand` / `outputDir`）而非硬编码；执行仍遵循现有命令安全原则（不经 shell、明确 argv、环境变量清理、超时、独立进程组、输出上限、用户确认，最好允许 bubblewrap 隔离）。
- **远程图片**（如果做）：应做成"用户点击后下载到本地资产目录"，不要让预览直接联网，避免破坏现有 CSP 和离线安全边界。

### 更后期再考虑：多标签页
价值高但成本大——当前核心状态仍是单个 `document`，虽已有 `unsaved_documents`，但未保存时会限制导航。真正的多标签页需要把模型升级为：

```text
DocumentSession { document, revision, generation, dirty, pending_assets, preview_state }
HashMap<DocumentId, DocumentSession>
```

审阅建议放在可靠性、事务恢复和项目索引都完成之后再做。
