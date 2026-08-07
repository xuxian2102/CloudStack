# 可靠性待办（源自 v0.2.4 第三方审阅，2026-08-06）

审阅对象：`main` HEAD `274aca3`（v0.2.4）。审阅结论中的关键技术断言已逐条核实（三路并行只读核实，见下方各条的"证据"），全部属实，未发现被证伪的条目。这里只记录待办本身，完整审阅原文不在此复述。

状态说明：`[ ]` 待处理 · `[x]` 已完成 · 每条尽量给出下一次实现时该看的入口文件/函数。

## P1（建议在下一个强调可靠性的版本前修复）

全部 5 项已修复（分四轮实施 + 一轮针对 rename journal 恢复状态机的修复，`main` 在 v0.2.4 之后的提交里）。

- [x] **Git 状态刷新缺少 generation/token 校验，存在乱序覆盖窗口**（第 1 轮）
  `EditorState` 加 `git_refresh_generation: u64`；`git_panel::refresh()` 完成回调改用新增的纯函数 `should_apply_git_refresh` 同时校验 root 与 generation。这个函数当时新建在 `crates/cloudstack-gtk/src/app/git_refresh.rs`，后来随"新增 `cloudstack-application` crate"一节的第 1 轮原样迁移到了 `crates/cloudstack-application/src/git_refresh.rs`，现在通过 `cloudstack_application::should_apply_git_refresh` 引用。

- [x] **Git porcelain v2 解析：冲突路径（`u` 记录）遇空格截断** + **有损 UTF-8 转换回传 git 当 pathspec**（第 2 轮，合并成一次改动，同一处代码）
  `parse_porcelain_v2` 改为 `(&[u8], &str) -> Result<GitStatus, AppError>`，全程按字节切分，只在放进 `FileChange`/分支名时严格 `str::from_utf8`，失败即拒绝（新增 `git::UNSUPPORTED_PATH_ENCODING_ERROR`，GTK 侧有专门的 `UiMessage::GitErrorUnsupportedPathEncoding` 提示，不止是日志）。`u` 记录改成固定字段数解析。`status()` 不再自己 `from_utf8_lossy`。范围按讨论结果收敛为"1+"：没有做 `FileChange`/`ManagedScope` 全链路 `OsString` 化（那是明显更大的一次改动，评估后判断当前不值得，见下方"未来再考虑"）。

- [x] **图片导入未做内容级验证，允许任意字节伪装成图片**（第 3 轮）
  新增 `SupportedImageFormat` 枚举统一原来三处分散的扩展名清单；`save_image` 两个分支（拖拽/剪贴板）都强制内容嗅探，无法识别直接拒绝；扩展名与内容不符时改名而不是报错；`read_image_asset` 改用 `Read::take` 有界读取替代"`fs::metadata` 查大小→整体 `fs::read`"，content-type 由嗅探结果决定；`content_matches_pending_revision` 改成有界流式哈希。彻底移除 SVG 支持（`infer` 无法可靠嗅探 SVG，且项目里没有真实 SVG 用例）。

- [x] **文章重命名不是崩溃安全的事务**（第 4 轮，四轮里最大的一块）
  新增 `crates/cloudstack-core/src/services/operations.rs`：`rename_post` 在开始移动任何文件之前把操作意图（old_id/new_id/asset_moves）写入 fsync 过的 journal；崩溃恢复走 forward-only（不做回滚）——图片移动、文章重命名两步都幂等地"看当前状态补没做的部分"，正文重写复用 `rewrite_colocated_image_paths` 天然的幂等性。in-process 失败时仍走原有的 best-effort 回滚，只有回滚失败才保留 journal 交给下次启动恢复。恢复挂在 `open_project` 里，列出文章之前跑，成功后 toast 提示用户。
  **范围收敛**：只覆盖 `rename_post`，不含 `delete_post`（已经走系统回收站，本身可恢复，风险明显更低，留给以后需要时再扩展 `operations.rs` 加 `DeleteOperation`）。
  **测试缺口**：`rollback_asset_moves` 本身失败（"回滚的回滚也失败"）这个分支的自动化测试没有写——工程上很难在不侵入代码的前提下可靠地只让"回滚"这一步失败而不影响它要回滚的那次"正向"操作（两者需要同一目录的写权限，无法用静态 chmod 区分方向，又没有代码钩子能在函数执行中途插入扰动）。逻辑本身是一个简单的布尔与运算，人工审阅可信度足够高，但如果以后要重构这块代码建议先补一个依赖注入点再补测试。

  **后续修复（针对第 4 轮的代码评审反馈，同一个 P1 之内）**：
  - 恢复状态机原来把"source/target 都存在（冲突）"和"source/target 都不存在（缺失）"两种反常状态直接当成"已经完成"处理，会把不确定状态误判为恢复成功、删掉 journal。改成显式的 `MoveRecoveryState`（`Pending`/`Completed`）+ `classify_move_state`：用 `symlink_metadata` 而不是 `exists()` 区分普通文件/符号链接/目录，只有"明确没做"或"明确做完"这两种状态才继续，其余一律整体中止本次恢复、保留 journal，不猜测哪种是"正确"的现实状态。文章重命名那一步用同一个函数判断。新增 4 个测试覆盖冲突/双缺/类型不对三种反常状态。
  - journal 原来存的是 `asset_moves: Vec<(PathBuf, PathBuf)>` 绝对路径，恢复时直接信任。改成只存 `asset_names: Vec<String>`（文件名），恢复时用当前 `ProjectContext` 重新推导 asset 目录（重新走一遍 `resolve_post_path`/`asset_dir_for_post` 里的路径守卫），把 journal 当成不可信的持久化输入处理；恢复前会先对 journal 里全部路径做一遍只读分类，任何一个不合法就整体中止，不会移动到一半才发现后面的路径有问题。
  - `save_image` 剪贴板内容寻址分支里 `fs::read(&desired_path)? == bytes` 的去重比较是无界读取，改成复用 `read_bounded_file`（上限设为 `bytes.len()`，大小不等直接判定内容不同，不读完整个文件）。
  - `read_rename_journal` 原来是整体 `fs::read` 后再检查 256 KiB 上限，改成 `Read::take` 有界读取，和图片那边的有界读取原则保持一致。
  - **有意不做的部分**：journal 自身的写入是 fsync 过的原子替换，但 `fs::rename` 之后没有对受影响目录逐一 `fsync`，journal 删除后也没有再同步 `operations/` 目录本身——当前只保证"应用进程被杀掉"这一档的崩溃安全（process crash safety），不是内核崩溃/断电也不丢状态的 power-loss safety。真要做到后者，需要在每次目录项变更后都跟一次目录 fsync（journal → fsync operations/ → 资产 rename → fsync 各自 parent → 文章 rename → fsync 各自 parent → 重写正文并 fsync → 删 journal → 再 fsync operations/），这会给每次重命名操作增加好几次额外的 fsync 开销。对桌面博客编辑器来说进程被杀比断电/内核崩溃常见得多，这个取舍是有意的，不是遗漏；`operations.rs` 顶部模块注释里也写明了这个范围边界。

  **第三轮后续修复（针对上一轮修复的再评审）**：
  - `asset_dir_for_post` 只做路径拼接（`content_root.join(post_stem_path(post_id))`），不校验结果——恢复逻辑重新推导出 `old_asset_dir`/`new_asset_dir` 后如果不加校验就直接用，资产目录这一级本身如果在崩溃后被换成指向项目外的符号链接（比如 `world -> /tmp/outside`），图片就可能被移出项目边界。新增 `validate_asset_directory`：已存在必须是非符号链接的普通目录且 canonical 路径在 `content_root` 下；不存在则向上找最深已存在祖先做同样校验（防止后续 `create_dir_all` 顺着祖先符号链接在项目外建目录）。在 `apply_rename_recovery` 里对 `old_asset_dir`/`new_asset_dir` 都调用，在做任何分类/移动之前。
  - `classify_move_state` 原来用 `fs::symlink_metadata(path).ok()` 把所有错误（包括权限错误）都当成"不存在"。改成 `metadata_if_exists` helper，只有 `ErrorKind::NotFound` 才转成 `None`，其他 IO 错误原样上抛、中止恢复。
  - 新增 2 个测试：`recover_rejects_symlinked_old_asset_directory`、`recover_rejects_symlinked_new_asset_directory`。

  **第四轮后续修复（两个边界收尾，评审认为修完可以正式停止继续加固）**：
  - `asset_names` 为空（纯文章重命名，不涉及任何图片）时，原来仍然无条件推导并校验 `old_asset_dir`/`new_asset_dir`，会让一个跟这次操作完全无关的同名 stem 目录（哪怕只是恰好存在、或者被换成符号链接）挡住本来不需要碰它的纯文章恢复。改成整个资产目录的推导/校验/收尾清理都包在 `if !operation.asset_names.is_empty()` 里，没有图片意图就完全不碰同名目录。
  - journal 里的 `asset_names` 如果被篡改成含重复文件名（比如两个不同来源目录派生出同一个文件名），预扫描阶段两项都会分类成 `Pending`，执行阶段第一项成功、第二项才因为 target 已存在而失败，变成"校验全部通过后仍然只部分执行"。在分类循环里加了 `BTreeSet` 去重检查，遇到重复文件名立即拒绝整个操作，不移动任何文件。
  - 新增 2 个测试：`recover_post_only_ignores_unrelated_asset_directory`、`recover_rejects_duplicate_asset_names_before_moving_anything`。

## P2（代码质量与长期可维护性，不紧急）

- [ ] **层内模块继续变大**：`git.rs`（72K）、`posts.rs`/`assets.rs`（各 40K）体量已大；`window.rs`（72K）虽已拆出 `window/{articles,frontmatter,publish,git_panel,drafts,recent,settings,welcome}.rs` 等子模块，但 `window.rs` 本体和 `git_panel.rs`（1716 行）、`drafts.rs`（1047 行）仍偏大，可按职责继续下沉（如 `services/git/{command,status,scope,publish,remote}.rs`）。GTK 侧这块的完整解法见下方"新增 `cloudstack-application` crate"一节——不只是拆文件，而是把应用状态机拆进独立 crate，形成编译期边界。
- [ ] **结构化错误未贯彻到底**：`AppError::Io(String)`/`Git(String)`/`Config(String)` 等仍用字符串包裹；stage/commit/push 的错误 detail 可能从原始 stderr 直接构造，脱敏入口不统一。方向：细化 `GitError`/`FileError` 变体，所有 stderr 在进入 trace/payload/日志前统一脱敏。
- [ ] **大量路径暂存可能触发 ARG_MAX**：`add_args`/`commit_args`（`git.rs` 约 L1181-1182, 1202-1203）把所有路径放进 argv。方向：`git add --pathspec-from-file=- --pathspec-file-nul` 走 stdin；commit message 增加非空/无 NUL/长度上限校验（目前 `message: &str` 直接拼进 `commit_args`，约 L1202，完全无校验）。
- [ ] **路径校验仍有本地 TOCTOU 窗口**：canonicalize/符号链接检查与实际打开文件之间可能被本地进程替换。方向（仅 Linux 目标可行）：`openat2` + `RESOLVE_BENEATH`/`RESOLVE_NO_SYMLINKS`，或 `cap-std`/`rustix` 能力目录。
- [ ] **缺少三类专门测试**：① rename 各阶段的故障注入测试（模拟崩溃/重启后验证一致性）；② porcelain 解析、Markdown URL 过滤、frontmatter lossless 修改、图片路径 percent-decode、重命名路径改写的 fuzz/property test；③ MSRV 独立 CI 任务、`cargo deny`、GitHub Actions 完整 SHA 固定。
- [ ] **（预置 bug，非本轮引入）reference 式图片重命名会断链**：`referenced_colocated_image_files()` 会把 `![cover][hero]` + `[hero]: hello/cover.png` 这种引用式定义里的图片也纳入移动计划（用的是解析后的 `dest_url`），但 `rewrite_colocated_image_paths()` 明确跳过引用式图片（`inline_image_destination_range` 找不到就 `continue`，注释写着"引用式图片的真实 URL 位于定义处，不属于这段源码"）。结果是图片文件被移动到新目录，但 `[hero]: hello/cover.png` 这行定义还指向旧路径，链接失效。建议作为独立的 correctness fix 尽快处理，不要等到大重构——不属于这轮 P1 的崩溃恢复/编码/图片校验范畴，是重命名路径改写逻辑本身的既有 bug。
- [ ] **编辑器 / 正式预览滚动同步延迟**（发现于 Phase 12D/12E Live Preview 性能验收，2026-08-07）：左侧 `GtkSourceView` 滚动时，右侧 WebKit 正式预览常常慢一拍才跟上；从其他程序切回 CloudStack 窗口后再滚动，右侧会先停顿一段时间才开始响应，然后才跟着动。在小文档和大文档（含 ~1 MiB）上都能复现，说明和 Live Preview 语义高亮、`apply_plan()`/`SourceIndex`（Phase 12E）无关——12E 只改了正文语义高亮的坐标解析路径，不涉及 `preview.rs` 的滚动同步，详见 `docs/LIVE_PREVIEW_V1_BASELINE.md` §8。方向：排查 `preview.rs` 里 editor↔WebKit 的滚动比例换算/消息投递/帧调度，而不是语义高亮路径。

## 新增 `cloudstack-application` crate（用户提出，2026-08-07，P1 收尾后的下一步）

**状态：第 1～8 项已全部完成（第 7 项拆成 7A/7B/7C 三轮，理由见下）。测试数量全程守恒可核对到 280，`cargo tree -p cloudstack-application --edges normal` 确认零 GTK/GLib/GIO/Adwaita/WebKit 依赖，且不依赖 `cloudstack-renderer`。application-layer 第一阶段收尾，`EditorState → WorkspaceSession` 留给下一阶段单独设计。**

```text
第 1 轮：新建 crate + 迁移 save/settings/git_refresh          [x] 21 个测试
第 2 轮：Git 主操作决策迁移（PrimaryGitAction/EffectiveGitAction） [x]  8 个测试
第 3 轮：ControlModel → WorkspaceCapabilities                [x]  7 个测试
第 4 轮：PreviewCoordinator（含"撤销覆盖预览"正确性修复）      [x]  4 个测试
第 5 轮：Recent 状态协调（恢复选择规则 + LastDocumentWriter）  [x] 11 个测试
第 6 轮：PublishPlan                                          [x]  8 个测试
第 7A 轮：草稿存储策略 + 批量保存/丢弃 use case               [x]  7 个测试
第 7B 轮：FIFO Coordinator + DraftOperation/DraftCompletion   [x]  4 个测试
第 7C 轮：恢复资格判定 + 草稿比较策略                          [x]  5 个测试
第 8 轮：OpenWorkspace use case                               [x]  5 个测试
```

依赖方向 `cloudstack-gtk → cloudstack-application → cloudstack-core`（外加 `cloudstack-gtk → cloudstack-renderer → cloudstack-core`）已经是编译期边界，不再只是"目录上的分层"。

P1 基本结束后、在继续加搜索/多标签页等功能前，应该先把应用逻辑从 `cloudstack-gtk` crate 中抽出来，后续维护成本会低很多。以下是这个决定当时的背景（历史记录，不是现状描述）：在第 1 轮之前，`crates/cloudstack-gtk/src/app/{save,settings,git_refresh}.rs` 已经明确不依赖 GTK，但仍然属于 `cloudstack-gtk` crate，只是"目录上的分层"，还没有形成编译期边界——这正是发起这轮拆分的理由。

### 目标依赖方向

`crates/cloudstack-application` 已经建好（第 1 轮），当前 workspace 是 `cloudstack-application`、`cloudstack-core`、`cloudstack-gtk`、`cloudstack-renderer` 四个 crate：

```text
cloudstack-gtk ───────→ cloudstack-application ───────→ cloudstack-core
       │
       └──────────────→ cloudstack-renderer ──────────→ cloudstack-core
```

* `core`：文件、Git、文章、草稿、配置等领域和基础设施能力；
* `application`：一次用户操作应该怎样协调、状态怎样转换；
* `gtk`：读取控件、调用 application、执行异步任务、渲染结果；
* `renderer`：Markdown → HTML，不需要知道 GTK。

不要把这些应用状态机继续塞进 `cloudstack-core`，否则 core 会逐渐变成"所有非 GTK 代码"的杂物箱。

### 待迁出逻辑，按优先级

1. [x] **`gtk/src/app/{save,settings,git_refresh}.rs`**——已原样迁移到 `cloudstack-application/src/{save,settings,git_refresh}.rs`（`git_refresh.rs` 保留原名，没有按最初设想改成 `task_ticket.rs`；改名加泛化成通用 `ProjectEpoch + RequestId` 票据抽象是独立的设计决策，留到真的有第二个消费者时再做，不是"原样迁移"这一轮该做的事）。`apply_successful_save()` 参数偏多的问题仍然存在，"收敛成 session 方法"仍是未来步骤，不是这一轮做的。

2. [x] **Git 操作决策**：`recommended_action()`/`effective_primary_action()` 及 `PrimaryAction`/`EffectivePrimaryAction` 已迁入并改名为 `application::git::{PrimaryGitAction, EffectiveGitAction, recommended_action, effective_action}`。`prioritized_changes()`/`MAX_DISPLAYED_CHANGES` **没有迁移**——实施时发现它内置了"Git 面板最多显示 100 行"这个具体面板显示容量策略，属于 presentation 而不是应用状态机，迁过去会让 application crate 从第一天开始承载具体 GTK 面板的显示策略，已按这个边界修正留在 GTK。`action_label()`、`localized_summary_text()` 等 presentation/i18n 一如既往留在 GTK。

3. [x] **整体控件状态模型**：`ControlModel`/`controls_for(&EditorState)` 已迁入 `application::controls::{WorkspaceCapabilitiesInput, WorkspaceCapabilities, capabilities_for}`，`render_controls(&Widgets, &WorkspaceCapabilities)` 留在 GTK。字段名沿用原 `ControlModel` 的命名（`open_enabled`/`home_enabled`/... 而不是这里最初示意的 `can_open_project`/`can_close_project`），减少无谓改名；`git_dirty`/`stable` 两处关键语义原样保留（`git_dirty` 只反映当前文章，不与其他未保存文章混淆；`stable` 只挡导航类操作，当前文档的编辑/保存/frontmatter/文章列表仍只看 `busy`）。`sync_controls()` 把 `EditorState` 的 `RefCell` 借用收窄到只覆盖读取快照那一步，`render_controls()` 触发的 GTK setter 调用不再持有借用。

4. [x] **预览任务状态机**：`preview.rs` 里原来的 `RenderRequest`/`QueueState`/`should_apply()`/`debounce_duration()` 已迁入并重构成 `cloudstack-application/src/preview.rs` 的 `PreviewCoordinator`（`PreviewTicket`/`PreviewRequest`/`PreviewAction`/`PreviewCompletion` + `set_document`/`clear`/`schedule`/`debounce_elapsed`/`complete_render`）。GTK 侧 `Inner` 只保留 `timeout: RefCell<Option<glib::SourceId>>`（实际 GLib 定时器，coordinator 不持有）和 `coordinator: RefCell<PreviewCoordinator>`，新增 `dispatch_action`/`cancel_timeout` 两个小 adapter；WebKit shell、JS 调用、URI scheme、编辑器与预览滚动同步等继续留在 GTK。`cloudstack-application` 不依赖 `cloudstack-renderer`——coordinator 只协调"什么时候该渲染哪段文本"，真正的 Markdown → HTML 转换仍由 GTK 持有的 `MarkdownRenderer` 完成。
   **顺手修的正确性问题**：原来的 `schedule()` 会在"新正文等于上次已应用内容"时直接提前返回，不取消尚未开始的 debounce/不推进 generation。场景：预览显示 A → 用户输入 B（进入 debounce 或已经在后台渲染）→ 用户快速撤销回 A → 因为 A 等于 last_applied 就什么都不做 → B 没被取消或失效 → B 最终完成后覆盖掉本该保持是 A 的预览。修复后 `schedule()` 无论是否需要重新渲染，都先无条件清空 `pending`/`debounced` 并推进 `generation`，让任何还在飞行或排队的旧请求（不管是不是恰好等于新内容）在完成时因为 generation 不匹配而被拒绝应用。新增回归测试 `returning_to_last_applied_source_invalidates_newer_render`，覆盖"B 还在 debounce"和"B 已经在后台渲染"两种子场景。
   **非阻塞的补充测试建议（未实施，留给以后方便时再做）**：补一个覆盖 debounce 与 active 交错时序的测试 `active_render_and_debounce_handoff_works_in_both_orders`——分别验证"debounce timer 先到、request 进入 pending，然后 active 完成并启动它"和"active 先完成、timer 后到，request 直接成为 active"两种顺序。当前实现对这两种顺序的处理逻辑看起来都正确，不阻塞任何提交。

5. [x] **Recent 状态协调**：`window/recent.rs` 里的三块纯规则/状态机已迁入 `cloudstack-application/src/recent.rs`（模块本身不在 crate 根重新导出，保持 `recent::` 领域命名）：
   - `choose_document_to_restore()` 从六个位置参数（含四个调用方预先算好的布尔值）改成 `DocumentRestoreInput`，函数内部直接比较 `current_project_root`/`expected_project_root` 和两个 epoch，而不是信任调用方传入的预算布尔值。
   - 新增 `choose_project_to_reopen()`/`ProjectReopenInput`，把"按 `last_opened_ms` 找真正最近打开的项目、而不是直接拿 pinned 排序后的列表第一项"这条规则从 `maybe_reopen_last_project()` 里搬出来。
   - `LastDocumentWrite`（原来只有 `in_flight: bool` + `pending`）重构成专用的 `LastDocumentWriter { next_generation, in_flight: Option<LastDocumentWrite>, pending: Option<LastDocumentWrite> }`，用 generation 防止迟到/错配的 completion 误清当前 active 状态。没有为了和 `SettingsWriter` 复用而做通用 `LatestWriteCoordinator<T>`，两个专用状态机分开写。

   `thread_local!` 运行时实例、`tasks::run` 派发、`recent::load/touch/set_last_document` 等实际读写、`app_data_dir`、欢迎页绑定都保留在 GTK；GTK 只负责按 application 返回的 `*Action`/`Option<...>` 执行副作用。

   **测试比原计划多一个**：除了迁移原有 5 个 `choose_document_to_restore_*` 测试、新增 2 个项目重开测试、新增 3 个 writer 测试（共 10 个）之外，额外补了 `choose_document_to_restore_returns_none_when_project_root_changed`——旧的布尔参数签名把"项目 root 是否还是当时那个"这条判断隐藏在调用方预算好的 `project_still_current: bool` 里，没法单独测；改成结构体输入、函数内部直接做比较之后，这条判断本身变成了函数逻辑的一部分，值得单独覆盖。因此 `cloudstack-application` 实际是 40 → 51（不是预想的 50），总数 253 → 259（不是预想的 258）。

6. [x] **发布选择模型**：`window/publish.rs` 原来把应用模型和控件混在一起（`ArticleChoice { article_id, paths, checkbox: gtk::CheckButton }`）的做法已经拆开。迁入 `cloudstack-application/src/publish.rs`：
   ```rust
   pub struct PublishChoice { pub article_id: Option<String>, pub paths: Vec<String>, pub selected: bool }
   pub enum PublishStatusBlocker { Conflicts, BehindRemote, NoManagedChanges }
   pub enum PublishBlocker { Working, Status(PublishStatusBlocker), NoSelection, EmptyMessage }
   pub struct PublishSubmission { pub message: String, pub push: bool, pub selected_paths: Vec<String>, pub updated_exclusions: Option<Vec<String>> }
   pub struct PublishPlan { .. }
   impl PublishPlan {
       pub fn new(context: &ProjectContext, posts: &[PostSummary], status: &GitStatus) -> Self;
       pub fn choices(&self) -> &[PublishChoice];
       pub fn set_selected(&mut self, index: usize, selected: bool);
       pub fn update_status(&mut self, status: &GitStatus);
       pub fn status_blocker(&self) -> Option<PublishStatusBlocker>;
       pub fn has_upstream(&self) -> bool;
       pub fn blocker(&self, message: &str, working: bool) -> Option<PublishBlocker>;
       pub fn prepare_submission(&self, message: &str, push_requested: bool, remember_choices: bool, working: bool) -> Result<PublishSubmission, PublishBlocker>;
   }
   ```
   `article_for_git_path()`（含"嵌套文章优先于外层资产目录"的分组规则）作为私有辅助函数一并迁入并保留原逻辑不变。GTK 侧 `PublishDialog` 现在只持有 `plan: RefCell<PublishPlan>` + `choice_buttons: Vec<gtk::CheckButton>`，`apply_status()`/`set_working()`/`sync_publish_button()` 改成读取 `plan` 的只读方法；发布按钮点击回调改成调用 `plan.borrow().prepare_submission(...)`，按返回的 `Result` 早退或解构 `PublishSubmission` 派发后台任务，后台闭包按 `updated_exclusions: Option<Vec<String>>` 是否为 `Some` 决定要不要重写项目配置。`publish_summary()`、`publish_operation_log()`、`detail_label()`、`localized_change_line()`、`change_kind_symbol()`、`MAX_STATUS_CHANGES` 等生成本地化文字/展示布局的逻辑原样留在 GTK presentation 层，没有顺带迁移真实 Git 发布 use case（`git::publish_selected` 调用仍在 GTK），也没有做 commit message hardening，严格按用户要求的范围执行。

   **测试**：8 个测试全部按预期迁入/新增（`nested_article_wins_over_an_outer_asset_directory` 从 GTK 迁移，其余 7 个为新增：`publish_plan_groups_article_and_colocated_assets`、`deleted_article_is_still_exposed_as_an_article_choice`、`excluded_articles_start_unselected_but_other_managed_paths_remain_selected`、`renamed_article_submission_includes_old_and_new_paths`、`submission_uses_current_selection_and_updates_exclusions`、`conflicts_behind_and_no_managed_changes_block_submission`、`working_empty_message_and_empty_selection_block_submission`）。`cloudstack-application` 51 → 59（+8），`cloudstack-gtk` 因为迁走了那一个测试 44 → 43（-1），workspace 总数 259 → 266（application 59、core 155、gtk 43、renderer 9），与预期完全吻合，没有偏差需要说明。

7. **草稿队列和批量保存**：`window/drafts.rs` 是目前 GTK 层最值得拆、同时改动也最大的一块，混合了草稿主目录/旧目录回退、FIFO operation queue、single-flight 执行、批量保存/丢弃、草稿恢复资格判断、completion 后更新 session、对话框和 toast。体量比前几项大得多，拆成 7A/7B/7C 三轮独立提交：
   ```text
   application/src/drafts/
     mod.rs            // pub use，不重新导出到 crate 根
     storage.rs        // primary + legacy 策略（7A）
     batch.rs          // save/discard reports（7A）
     coordinator.rs    // FIFO/single-flight（7B）
     recovery.rs        // 是否可提示恢复（7C）
   ```

   **7A（已完成）**：`DraftStorage`（`new`/`read`/`write`/`delete`，字段私有）和 `BatchFailure`/`BatchSaveReport`/`DiscardReport`/`save_documents`/`discard_documents` 已迁入 `cloudstack-application/src/drafts/{storage,batch}.rs`，`mod.rs` 用私有 `mod storage; mod batch;` + `pub use` 把类型拍平到 `cloudstack_application::drafts::` 一层（不是 `drafts::storage::`），风格上对齐 `publish::`/`recent::` 那种"域名一层就到类型"的引用方式；`lib.rs` 只加了 `pub mod drafts;`，没有再重新导出到 crate 根。

   按用户要求修正了一处边界：原来 `cleanup_warnings: Vec<String>` 直接生成中文整句（"{path}：文章已保存，但清理自动恢复草稿失败：{error}"），迁移后改成结构化的 `DraftCleanupWarning { relative_path, error }`，本地化文案组装挪到 GTK 侧新增的 `cleanup_warning_text()`。

   GTK 侧 `draft_storage()`（依赖 `glib::user_data_dir()`/`APPLICATION_ID`/`LEGACY_APPLICATION_ID`/`e2e` 环境变量）保留在 GTK，只是改成调用 `DraftStorage::new(primary, legacy)`；`DraftQueue`/`Operation`/`Completion`/`pump()`/`can_offer_recovery()`/`is_current_post()` 全部原样保留在 GTK，7A 完全没有碰队列和恢复 policy。`tempfile` dev-dependency 跟着测试从 `cloudstack-gtk/Cargo.toml` 移到了 `cloudstack-application/Cargo.toml`（GTK 里已经没有别的地方用它）。

   **测试**：原 `drafts.rs` 最后 7 个测试全部迁移，3 个进 `storage.rs`（`primary_draft_wins_and_legacy_is_the_fallback`、`writing_primary_removes_the_matching_legacy_draft`、`deleting_a_draft_clears_both_storage_locations`），4 个进 `batch.rs`（`batch_save_writes_each_snapshot_and_clears_its_draft`、`batch_save_keeps_external_conflict_as_a_failed_article`、`batch_save_keeps_success_and_failure_independent`、`batch_discard_removes_all_recovery_drafts`）。因为 `DraftStorage` 字段对 `batch` 模块不可见（只有 `storage` 子模块内部能访问私有字段），`batch.rs` 测试里原来直接写 `storage.primary` 路径的两处改成了通过公开的 `storage.write(...)` 方法落一份已存在的草稿，效果一致（`base_revision` 统一用字面量 `"revision"`，其中一处原来用的是 `failed.revision.clone()`，但两种写法下该字段都不影响这几个测试的断言，只是最小化迁移代码量的取舍）。`cloudstack-application` 59 → 66（+7），`cloudstack-gtk` 43 → 36（-7），workspace 总数 266 不变（application 66、core 155、gtk 36、renderer 9），与预期完全吻合。

   **7B（已完成）**：`DraftCoordinator`（ticket 化的 FIFO 单飞队列，`DraftTicket`/`DraftTask`/`DraftAction`）+ `DraftOperation`/`DraftCompletion`（从原来 GTK 私有的 `Operation`/`Completion` 改名迁入，字段和 `execute()`/`closes_window()` 原样保留）已迁入 `cloudstack-application/src/drafts/coordinator.rs`。`DraftAction::Execute` 按 clippy `large_enum_variant` 的提示包了一层 `Box<DraftTask>`（`DraftTask` 里嵌着 `DraftOperation`，最大变体 360+ 字节，跟零大小的 `None` 变体放在同一个 enum 里会触发这条 lint），这是规格之外补的一处必要改动。

   GTK 侧 `DraftQueue` 现在只剩 `{ coordinator: DraftCoordinator, timer: Option<glib::SourceId> }`；`enqueue()` 改成 `coordinator.enqueue(operation)` 拿到 `DraftAction` 后交给新增的 `dispatch_action()`；原来的 `pump()` 删除。按规格严格保证了顺序：`dispatch_action()` 的 completion 回调里先跑 `handle_completion()`（可能在其中触发新的 `enqueue_delete(...)`，此时 `coordinator.active` 还没释放，新操作只会排进 `pending`，不会抢跑），再用 `handle_completion()` 的返回值作为 `stop_queue` 调用 `coordinator.complete(ticket, stop_queue)`，用返回的下一个 `DraftAction` 递归调 `dispatch_action()`。这比原来"先在回调里无条件把 `active` 置回 `false`、再跑 `handle_completion()`、依赖同步重入的 `pump()` 把新 enqueue 的操作立刻启动"的写法更显式——原来的写法能工作是因为单线程 GTK 主循环里的重入巧合，不是设计出来的顺序保证。

   **测试**：新增 4 个，均按预期命名：`first_operation_starts_immediately`、`queued_operations_preserve_fifo_order`、`mismatched_completion_does_not_release_active_operation`、`stopping_after_completion_discards_remaining_queue`。`cloudstack-application` 66 → 70（+4），`cloudstack-gtk` 不变（`Operation`/`Completion` 整体搬走但没有测试跟着搬——原 `drafts.rs` 测试模块已经在 7A 整体迁完），workspace 总数 266 → 270，与预期完全吻合。

   **7C（已完成）**：`DraftRecoveryEligibilityInput`/`can_offer_recovery`（恢复资格判定，按计划加了 project root 校验——原来只查 busy/dirty/epoch/post id，现在补上第 5 项"root + epoch 双校验"同样的原则）、`CurrentDraftTargetInput`/`is_current_draft_target`（写入/删除失败要不要展示给用户，只看 root + post id，不看 busy/dirty/epoch）、`DraftRecoveryDecision`/`classify_recovery`（草稿与磁盘内容是否一致 → 直接清掉 `DeleteRedundant`，否则 `Offer { disk_changed_since_draft }`）已迁入 `cloudstack-application/src/drafts/recovery.rs`。

   GTK 侧 `is_current_post`/`can_offer_recovery` 保留原名作为薄封装（后者新增了 `project_root` 参数），内部只是从 `EditorState` 组出 input 结构体转发给 application 函数；`handle_completion()` 里原来内联的"内容相同就删、否则弹对话框"判断改成 `match classify_recovery(&document, &draft)`；`show_recovery_dialog()` 新增 `disk_changed_since_draft: bool` 参数（调用方已经知道分类结果，不用再在对话框内部重新比较 `draft.base_revision == document.revision`），对话框构建、两个响应回调、本地化文案原样留在 GTK；"restore"/"disk"两个响应闭包调用 `can_offer_recovery` 时补上了 `context.root`（"restore" 闭包额外克隆了一份 `restore_root`，因为 `context` 本体要移进后定义的"disk"闭包）。

   **测试**：新增 5 个，均按预期命名：`recovery_requires_idle_matching_project_post_and_epoch`（一次性覆盖 busy/dirty/root/post-id/epoch 五个否决条件，风格上对齐 `project_reopen_is_blocked_when_disabled_or_workspace_is_not_idle` 这类既有的组合测试，没有拆成 5 个独立测试函数）、`identical_draft_is_deleted_without_prompt`、`matching_base_revision_offers_normal_recovery`、`changed_base_revision_offers_disk_changed_warning`、`current_draft_target_requires_both_root_and_post_id`。`cloudstack-application` 70 → 75（+5），`cloudstack-gtk` 不变，workspace 总数 270 → 275，与预期完全吻合。

   第 7 项至此全部完成：`drafts.rs` 不再拥有存储策略、批量 use case、FIFO 队列、恢复 policy 这四类应用状态机，只保留 700ms 定时器、SourceView 读取、对话框、toast/status 展示这些确实只对当前 GTK 界面有意义的部分。

8. [x] **项目打开 use case**：`open_project()` 后台任务原来做的 `project::open_project → ensure_local_config_excluded → recover_pending_renames → list_posts` 四步、以及 `Opened`/`NeedsInitialization`/`NeedsContentRepair` 的区分，已迁入 `cloudstack-application/src/workspace.rs` 的 `pub fn open_workspace(root: &Path, app_data_dir: &Path) -> Result<OpenWorkspaceOutcome, AppError>`（原 GTK 本地的 `OpenProjectOutcome::Opened(ProjectContext, Vec<PostSummary>, Vec<RecoveredRename>)` 元组变体在迁移时顺手改成了更易读的结构体变体 `Opened { context, post_summaries, recovered_renames }`，字段语义不变）。四步的执行顺序和失败语义原样保留：`ensure_local_config_excluded` 失败仍是 best-effort（`let _ =`，不阻止打开一个本来有效的项目）；`recover_pending_renames` 必须先于 `list_posts` 执行（上次意外退出可能留下半完成的重命名，不先续完文章列表会同时看到消失的旧 id 和还没出现的新 id）；`MissingProjectConfig`/`MissingContentDirectory` 转成对应的引导 outcome，其余错误原样上抛。

   `app_data_dir()` 依赖 `glib::user_data_dir()`，仍然留在 GTK；`open_project()` 里原来在后台闭包内部才调用 `app_data_dir()`，现在改成在主线程先算出 owned `PathBuf`（`let application_data = app_data_dir();`），再随 `root` 一起移进后台闭包——避免从后台线程调用 GLib API，顺手把这一处也理清楚了。GTK 侧原来 `Opened` 分支内联的一大段（project/status label、`EditorState` 安装、`ListBox` 填充、SourceView placeholder、WebKit preview 清空、frontmatter 侧栏可见性、窗口标题、`content_stack` 切换、`recent::touch`/`maybe_reopen_last_document`、frontmatter/Git 面板刷新、重命名恢复 toast）原样提取成新的私有函数 `apply_opened_workspace()`，逻辑不变，只是从内联代码变成具名函数，让 `open_project()` 本身更容易读；这些全部是具体 GTK 展示/session 安装决策，不属于 application 层。`NeedsInitialization`/`NeedsContentRepair` 两个分支、E2E 自动打开首篇文章的逻辑、初始化和修复对话框、`busy` guard 都原样留在 GTK。

   本轮没有引入 `OpenWorkspaceCoordinator`/ticket/generation——当前入口已经有 `busy` guard 挡住重复打开，不存在两个 open operation 乱序覆盖的问题，加这些属于当前用不上的过度设计，留给以后允许后台预加载/多窗口/可取消打开时再做。

   **测试**：新增 5 个，均按预期命名：`missing_config_requests_initialization_with_existing_content_suggestion`、`missing_content_directory_requests_repair`、`opening_workspace_lists_posts`、`opening_git_workspace_excludes_cloudstack_config_locally`、`rename_recovery_runs_before_post_listing`（最后一个是这一项最有价值的测试——不是重复 core 层已经覆盖过的恢复算法本身，而是验证 application use case 按正确顺序调用了它，构造了一份手写的 rename journal JSON 放进 `app_data_dir/operations/`，断言 `open_workspace()` 返回时重命名已经恢复完成、`list_posts` 看到的是恢复后的文件名）。`cloudstack-application` 75 → 80（+5），`cloudstack-gtk` 不变，workspace 总数 275 → 280，与预期完全吻合。

   第 8 项完成后，application-layer 第一阶段（1～8 项）全部收尾。文章 create/rename/delete 中"执行操作后重新 `list_posts`"的组合、`EditorState → WorkspaceSession` 的最终收敛，都留给下一阶段单独排期，不在这轮范围内。

### 已迁入 core 的数据规则（原"应该迁到 core、而不是 application 的逻辑"，已完成）

`frontmatter.rs` 里的 `parse_tags()`/日历合法性判断/`days_in_month()` 属于数据规则，不是应用协调——原来的日期计算甚至依赖了 `gtk::glib::DateMonth`，只是为了做日历合法性判断。已迁入 `cloudstack-core/src/services/frontmatter/value.rs`（`parse_tags_input`/`parse_calendar_date`/`days_in_month`），用已有的 `chrono`，不再依赖 GLib。

**订正**：这里最初把整个 `parse_date_parts()` 函数和 `date_subtitle()` 也列为迁移候选，属于过度归类，实施时收窄了：
- `date_subtitle()` 只是"空字符串 → 未设置 / 非空 → 原样显示"的纯 presentation，不搬，留在 GTK。
- `parse_date_parts()` 本身继续留在 GTK，但收缩成一个很薄的 wrapper——只调用 core 的 `parse_calendar_date()` 判断日期是否真实存在，自己再叠加"只能选 2000 年到今天、不允许未来日期"这条编辑器控件专属的范围策略；这是当前这个日期控件的选择范围，不是 frontmatter 领域规则，以后如果产品明确规定"禁止未来日期"再升级成 domain policy。

区分原则：
* "2024-02-29 是否是有效日期" → core；
* "一个月有多少天" → core；
* "日期下拉框显示多少项" → GTK；
* "只能选 2000 年到今天、不允许未来日期" → 目前是 GTK 控件自己的展示范围策略，不是 core 配置或 application policy；
* "标签输入如何按逗号拆分并去重" → core；
* "标签 chip 长什么样" → GTK。

**完成状态**：`day_strings()`/`resize_numbered_options()`/`field_title()`/`build_date_row()`/`build_tags_row()`/`rebuild_tag_chips()` 连同全部 widget/chip 状态都留在 GTK 不动；`parse_date_parts()` 仍然存在于 GTK，只是收缩成薄 wrapper。GTK 侧移除了 `use std::collections::HashSet;` 和本地 `parse_tags()`/`days_in_month()`（含 `gtk::glib::DateMonth` 那一整块）——`rg 'HashSet|DateMonth|fn days_in_month|fn parse_tags' crates/cloudstack-gtk/src/window/frontmatter.rs` 零匹配。测试：core 新增 3 个（`tag_input_is_trimmed_deduplicated_and_empty_values_removed`、`calendar_date_rejects_invalid_dates`、`month_lengths_follow_the_gregorian_calendar`），GTK 侧原来的 3 个测试收窄成 2 个（删除已经迁移到 core 的 `tags_are_trimmed_...`，`date_parts_validate_month_lengths_and_leap_years` 改名为 `date_parts_respect_the_editor_date_range` 且只测编辑器范围策略，保留 `empty_date_has_a_clear_subtitle`）。`cloudstack-core` 174 → 177（+3），`cloudstack-gtk` 36 → 35（-1），`cloudstack-application`/`cloudstack-renderer` 不变（116/9），workspace 总数 335 → 337。这一轮之后按用户的决定，停止继续"还能不能再搬一点"式的架构清理。

### 不要因为函数是纯的就全部迁走

以下即使不直接调用 GTK，也仍然适合留在 presentation：`initial_content_split_position()`、`git_split_position()`、window title 格式、`localized_*`、`change_marker()`、搜索匹配计数的显示、publish summary 的自然语言拼接。它们表达的是"这个 GTK 界面怎样显示"，不是"应用应该做什么"。`SearchPanel` 基本完全围绕 GtkSourceView 的 `SearchContext`/`TextIter`/`Buffer`/`View`，无需为了追求纯度硬拆。

判断标准：**换成 CLI/TUI 后是否仍需要这段规则？** 仍需要 → core/application；只对当前界面有意义 → GTK/presentation。

### 最终目标状态

```rust
fn on_save_clicked(...) {
    let command = session.begin_save(buffer_text);
    run_effect(command.effect, move |event| {
        let transition = session.handle(event);
        render(&widgets, transition.view_model);
    });
}
```

不必现在就做完整 Redux/Elm 架构，也不需要引入复杂 command bus。先采用"纯状态结构 + 明确 request/ticket + completion transition + GTK effect adapter"就足够。

### 推荐实施顺序

```text
1. 新建 cloudstack-application crate
2. 原样迁移 app/save、app/settings、app/git_refresh
3. 迁移 Git action policy 和 ControlModel
4. 迁移 PreviewCoordinator
5. 迁移 recent restore / latest-write 状态机
6. 迁移 PublishPlan
7. 迁移 DraftCoordinator
8. 迁移 OpenWorkspace use case
9. 最后再把 EditorState 收敛为 WorkspaceSession
```

第一轮应保持纯机械迁移，不同时重写状态模型；等编译边界建立并全绿，再逐块把 `EditorState` 的变更改成 application 方法。**最值得现在立即做的是第 1～3 项**——完成后 `cloudstack-gtk` 就不会再拥有保存完成判定、Git 主操作决策和控件权限规则这三类关键业务状态机，分层会从"代码习惯"升级成"编译器强制"。

## 后续路线：WorkspaceSession → 无损文本合同 → Live Preview（用户提出，2026-08-07，application-layer 第一阶段收尾后）

**状态：阶段 9 全部完成并冻结（9A/9B/9C + 9C.1 修正）。`WorkspaceSession` 十个字段已全部私有化，`cloudstack-gtk` 不再直接读写任何一个，只通过方法和只读 getter。阶段 10（10A/10B/10C）全部完成并冻结。阶段 11～14 尚未开始。**

顺序固定，不要跳步：

```text
阶段 9   EditorState → WorkspaceSession（9A/9B/9C 三个提交）
阶段 10  无损文本文件合同：EOL + 末尾换行（10A core 合同 / 10B PostDocument 集成 / 10C 全链路验证）
阶段 11  Live Preview 三个技术 spike（不进 main，只留结论/ADR）
阶段 12  Live Preview V1：语义样式，不隐藏 Markdown 标记
阶段 13  Live Preview V2：行级 conceal
阶段 14  Live Preview V3：OverlayManager、checkbox、block image
```

不要继续零散搬函数，也不要现在重写 GTK 或直接开始完整 WYSIWYG；不要把 WorkspaceSession 和 Live Preview 塞进同一个提交。

### 阶段 9：EditorState → WorkspaceSession（拆成 9A/9B/9C）

当前 `cloudstack-gtk` 的 `EditorState` 仍直接持有项目、文章、当前文档、dirty/busy、三个 generation、Git snapshot 和未保存文章表——这是第 1～8 项之后剩下的最大架构问题。

**9A（已完成）：建立 Session 外壳，机械迁移字段。** 新增 `cloudstack-application/src/session.rs`，`WorkspaceSession` 持有原计划的十个字段（`project`/`posts`/`document`/`unsaved_documents`/`git_snapshot`/`dirty`/`busy`/`document_epoch`/`edit_generation`/`git_refresh_generation`），原有字段上的说明性注释（`edit_generation`/`git_refresh_generation`/`unsaved_documents`）原样保留。`lib.rs` 只加 `pub mod session;`，不重新导出到 crate 根，延续 `recent::`/`git::`/`publish::`/`drafts::`/`workspace::` 的模块命名风格。

**一处规格之外的显式设计决定**：十个字段全部声明为 `pub`。这一轮明确不新增状态转换方法（那是 9B 的工作），但 GTK 仍需要直接读写这些字段（例如 `editor_state.dirty = false;`、`editor_state.document_epoch = editor_state.document_epoch.wrapping_add(1);`），如果字段私有 + 只读 getter，就必须同时发明一整套 setter 方法，等于提前做了 9B 的工作、也违反"本轮不新增状态转换逻辑"。9B 给字段加上真正的状态转换方法后，这些字段会收回私有。

GTK 的 `EditorState` 改成只剩 `session: WorkspaceSession` + `loading_buffer`（GTK buffer 加载标志）+ `draft_queue`（草稿队列/定时器）+ `pending_assets`（待提交图片管理）。所有跨 `window.rs` 和 `window/{articles,drafts,frontmatter,git_panel,publish,recent}.rs`（`settings.rs`/`welcome.rs` 不触碰这些字段）对这十个字段的直接访问，都在保持 `<expr>.<field>` 语义完全不变的前提下改成了 `<expr>.session.<field>`——纯粹加一层，没有改变任何调用路径的读写时机或借用范围。没有引入 `WorkspaceState` enum / `DocumentSession` map / command bus / event reducer / effect trait / Redux-Elm 架构。

**验收**：`EditorState` 现在只有 `session`/`loading_buffer`/`draft_queue`/`pending_assets` 四个字段，不再直接拥有原来的十个字段；`cargo tree -p cloudstack-application --edges normal` 确认仍然只依赖 `cloudstack-core`。这一轮是纯结构迁移，没有新增或删除任何测试——workspace 总数维持 280（application 80、core 155、gtk 36、renderer 9）不变，与预期完全吻合。

**9B：把状态转换收进 Session，按三个语义簇分批做（用户提出，不要一次把所有字段的读写都换成方法）。**

**workspace 生命周期簇（已完成）**：`WorkspaceSession` 新增 `install_workspace(context, posts) -> WorkspaceInstalled { document_epoch }`、`close_workspace() -> WorkspaceClosed { document_epoch }`、`replace_posts(posts)`。三个方法分别对应 GTK 原来的三处内联逻辑：
- `install_workspace`：替换 `apply_opened_workspace()` 里"安装 project/posts、清空 document/dirty/git_snapshot/unsaved_documents、推进 document_epoch"那一整块；不触碰 `edit_generation`/`git_refresh_generation`——两者分别只在编辑和 Git 刷新时才有意义。
- `close_workspace`：替换 `close_project()` 里对应的清空块；`unsaved_documents.clear()` 仍然防御性地执行一遍（正常情况下调用前已经由 `ensure_no_unsaved_documents` 保证是空的，`.clear()` 在这种情况下是幂等的，不改变行为）。
- `replace_posts`：只替换 `posts`，不碰其他字段——对应 `articles.rs` 里 `create_post`/`rename_post` 完成后刷新文章列表的两处调用点。

**明确没有迁移的地方**：`articles.rs` 的 `delete_post()` 里"替换 posts + 清空 document/dirty + 推进 document_epoch"那一整块（因为删除的是当前正打开的文章）**保持原样、继续直接读写 `.session.*` 字段**——这是一个"清空当前文档"的转换，语义上属于 document 生命周期簇，不属于这一轮的 workspace 生命周期范围，留到下一批一起设计（避免 `replace_posts` 和"清空当前文档"的语义在这一轮混在一起）。

**测试**：新增 5 个——`install_workspace_clears_previous_document_git_snapshot_and_unsaved_map`、`install_workspace_advances_document_epoch`、`close_workspace_clears_project_state_and_advances_epoch`、`close_workspace_is_idempotent_when_already_empty`、`replace_posts_only_replaces_the_list`。冻结了原计划 7 条语义里跟 workspace 生命周期直接相关的两条（打开新 workspace 会清空旧文档/Git snapshot/unsaved map；打开/关闭 workspace 都推进 `document_epoch`）。`cloudstack-application` 80 → 85（+5），`cloudstack-gtk` 不变，workspace 总数 280 → 285，与预期吻合（这一批之前没有预先给出精确测试数预测，5 个是按上述语义反推出来的自然数量）。

剩下 5 条语义留给后面两个簇验证：
3. 切换文章时保留其他文章的未保存快照；
4. 编辑后 `edit_generation` 推进；
5. 保存期间又发生编辑，旧保存 completion 不能清掉 dirty；
6. 删除或重命名文章时，unsaved map 不残留旧 ID；
7. 安装文档不能错误地继承上一篇文章的 dirty 状态。

**document 生命周期簇（已完成）**：`WorkspaceSession` 新增 `install_document(document, dirty) -> DocumentInstalled { document_epoch }`、`clear_document() -> DocumentCleared { document_epoch }`、`remove_document(post_id)`（save/dirty 簇收尾时改名为 `remove_unsaved_document`，见下）。

- `install_document`：替换 `display_document()` 里"安装 document、设置 dirty、推进 document_epoch"那一小块。`dirty` 是显式参数（不是从上一篇文档继承），覆盖语义 7——两个方向都测了：上一篇 dirty 切到新文档传 `dirty=false` 必须变干净，上一篇干净切到"恢复未保存快照"传 `dirty=true` 必须变脏。不触碰 `unsaved_documents`：其他文章的未保存快照是 `mark_document_dirty`（save/dirty 簇，未做）持续写入的，不是切换文档时才快照一次，所以"切换文章保留其他未保存快照"（语义 3）天然成立，测试只需要证明这个方法完全不碰那张表。
- **一处规格之外的新增**：原始方法列表没有覆盖"删除当前正打开的文章"这个转换（`install_document` 只接收非空 `PostDocument`，装不下"清空"）。新增 `clear_document()`，只清 `document`/`dirty` 并推进 `document_epoch`，不碰 `posts`/`project`/`unsaved_documents`——这三者分别由 `replace_posts`/`remove_document` 处理。这是把 7A 结尾故意留下的 `delete_post()` 内联块（当时写明"这是一个清空当前文档的转换，语义上属于 document 生命周期簇"）迁移完整所必需的，不是范围蔓延。
- `remove_document`：从 `unsaved_documents` 移除指定 id。`delete_post()`/`rename_post()` 分别在删除/重命名成功后对旧 id 调用它，覆盖语义 6。GTK 当前的删除/重命名入口在有任何未保存文章时就整体拒绝操作，所以实际调用时这个 id 通常已经不在表里——显式调用把"删除/重命名不残留旧 id"这个不变量写成代码，不再隐式依赖调用方的守卫逻辑。

GTK 侧：`display_document()`（window.rs）改用 `install_document`；`articles.rs` 的 `delete_post()` 改用 `replace_posts` + `clear_document` + `remove_document`（原来的四行直接字段赋值合并成三次方法调用）；`rename_post()` 补上 `remove_document(&old_id)`。`drafts.rs` 的 `complete_batch_save`/`complete_discard`/草稿恢复对话框的 "restore" 分支，以及 `window.rs` 的 `mark_document_dirty()`，仍然直接读写 `.session.dirty`/`.session.document`——这些是 save/dirty 生命周期簇的范围，这一轮没有碰。

**测试**：新增 7 个：`install_document_replaces_current_document_and_advances_epoch`、`install_document_does_not_inherit_previous_dirty_state`、`install_document_does_not_touch_posts_or_other_unsaved_snapshots`、`clear_document_resets_current_document_and_advances_epoch`、`clear_document_does_not_touch_posts_project_or_unsaved_documents`、`remove_document_removes_only_the_given_entry`、`remove_document_is_a_noop_when_absent`。覆盖了原计划 7 条语义里的第 3/6/7 条。`cloudstack-application` 85 → 92（+7），`cloudstack-gtk` 不变，workspace 总数 285 → 292，与预期吻合。

**save/dirty 生命周期簇（已完成）**：`WorkspaceSession` 新增 `mark_document_dirty(body) -> Option<DocumentDirty>`、`set_current_frontmatter(raw_frontmatter) -> bool`、`apply_saved_document(saved, saved_document_epoch, saved_generation) -> SaveCompletionOutcome`、`apply_batch_saved(saved)`、`discard_unsaved_documents(discarded_ids)`；`remove_document` 按用户要求改名为 `remove_unsaved_document`（同一个类型里已经有 `install_document`/`clear_document`，`remove_document` 容易被误读成"删除当前文档"）。

- `mark_document_dirty`：`dirty = true` + `edit_generation` 推进 + 把最新正文快照写入 `unsaved_documents`（覆盖同 id 旧快照）。没有当前文档时返回 `None`，对应原 GTK 函数开头的提前返回。覆盖语义 4，以及"第二次编辑覆盖同 id 快照"。
- `set_current_frontmatter`：只改当前文档的 `raw_frontmatter`，不推进 generation、不写 `unsaved_documents`——frontmatter 变更后紧接着要用最新 buffer 正文调用一次 `mark_document_dirty`，两者一起才构成"未保存快照同时包含新 frontmatter 和新正文"。
- `apply_saved_document`：**复用**（不是复制）`crate::save::classify_save_completion`/`apply_successful_save`（第 1 轮就迁移到 application 的既有纯函数），只是把它们包进 `WorkspaceSession` 的方法，调用方不用再从 `EditorState` 里手工拆三个 `&mut` 字段传进去。覆盖语义 5（保存期间又编辑，`RevisionOnly` 分类下 dirty/unsaved 快照原样保留）和"保存旧 document epoch 不覆盖当前文档"（`NotCurrent` 分类）。不碰 `pending_assets`——那是会触碰磁盘的副作用，留在 GTK 的 `Clean` 分支处理。
- `apply_batch_saved`/`discard_unsaved_documents`：两者共享同一条私有规则 `recompute_dirty_from_unsaved`（"当前文档 id 是否仍在 `unsaved_documents` 里"）。`apply_batch_saved` 另外把成功保存的文档换成新副本（同步 revision/body/frontmatter）；部分失败的批量保存只清成功项，失败项原样留在 `unsaved_documents` 里，如果失败的正是当前文档，`dirty` 保持 true。不碰 `pending_assets`，理由同上。

GTK 侧：`mark_document_dirty()`（window.rs）改成读 buffer → `session.mark_document_dirty(body)` → 用返回的 `post_id` 更新侧栏标记，三个直接字段赋值（`dirty`/`edit_generation`/`unsaved_documents.insert`）合并成一次方法调用。`save_document_then()` 改成构造一个 `PostDocument`（`id`/`relative_path`/`raw_frontmatter`/`body` 来自保存开始时的快照，`revision` 是 `write_post` 返回的新值）传给 `session.apply_saved_document(...)`，不再直接调用 `classify_save_completion`/`apply_successful_save`（这两个纯函数还在 `save.rs` 里，只是不再被 GTK 直接引用，改由 `apply_saved_document` 内部调用）。`frontmatter.rs` 的三条路径（新增/修改字段/删除 frontmatter）全部把 `.session.document.as_mut()` 改成 `set_current_frontmatter(...)`，紧接着仍然调用（未参数化的）GTK 包装函数 `mark_document_dirty(widgets, state)`。`drafts.rs` 的 `complete_batch_save`/`complete_discard` 分别改成调用 `apply_batch_saved`/`discard_unsaved_documents`（`pending_assets.reconcile_saved_post` 的调用拆成独立的前置循环，跟 session 更新不再交错，观察行为不变，因为两者在原代码里也没有数据依赖）。草稿恢复对话框的 "restore" 分支改成 `set_current_frontmatter(...)` → `buffer.set_text(...)` → 调用 GTK 的 `mark_document_dirty()`，**删除了一处原来重复的 `state.session.dirty = true;`**——原代码在这行之后紧接着调用 `mark_document_dirty()`，后者本身就会设置 `dirty` 并推进 `edit_generation`，前一次赋值完全是死代码（已用 CI 全绿确认删除它不改变任何可观察行为）。

**残留检查**（用户指定）：
```bash
rg '\.session\.(document|dirty|edit_generation|unsaved_documents)\s*=' crates/cloudstack-gtk
rg '\.session\.document\.as_mut\(' crates/cloudstack-gtk
rg '\.session\.unsaved_documents\.(insert|remove|clear)' crates/cloudstack-gtk
```
三条全部为空——GTK 现在不再直接赋值/直接调用这四个字段的可变方法，只保留只读访问（例如 `if let Some(document) = &state.session.document`）。`.session.git_refresh_generation = ...` 仍然存在（`git_panel.rs`），按用户明确的决定不在 9B 收进方法，留给 9C。

**测试**：新增 11 个：`mark_document_dirty_advances_generation_and_snapshots_body`、`mark_document_dirty_returns_none_without_a_current_document`、`mark_document_dirty_overwrites_snapshot_for_the_same_id`、`set_current_frontmatter_replaces_raw_frontmatter_without_touching_generation_or_unsaved`、`set_current_frontmatter_then_mark_document_dirty_snapshots_both`、`apply_saved_document_clean_clears_dirty_and_removes_unsaved_entry`、`apply_saved_document_revision_only_keeps_dirty_when_generation_advanced`、`apply_saved_document_not_current_when_epoch_advanced_does_not_overwrite_document`、`apply_batch_saved_updates_current_document_revision_and_body`、`apply_batch_saved_partial_failure_only_clears_successful_entries`、`discard_unsaved_documents_recomputes_current_dirty`。覆盖了原计划 7 条语义里剩余的第 4/5 条，外加用户在批准时补充要求的"batch save 更新当前 revision"、"partial batch save 只清成功项"、"discard 重新计算当前 dirty"、"frontmatter 更新后 snapshot 包含新 frontmatter"四点。`cloudstack-application` 92 → 103（+11），`cloudstack-gtk` 不变，workspace 总数 292 → 303，与预期吻合。

至此 9B 三个生命周期簇全部完成。三个簇全部完成后本该统一把这十个字段私有化，但按用户的决定：`git_refresh_generation` 属于 9C（Git 刷新 request/completion 收进 Session 时才处理），9B 收尾时不提前私有化任何字段——9C 把 `git_refresh_generation` 也收进方法之后，再一次性把全部 10 个字段私有化，避免中途出现"半私有 API"。9B 结束时的实际状态是：字段依然全部 `pub`，但 GTK 对 `project`/`posts`/`document`/`dirty`/`document_epoch`/`edit_generation`/`unsaved_documents` 这 9 个字段只保留读，不再有任何直接写；`git_refresh_generation` 仍然读写皆有，等 9C 处理。

**9C（已完成）：统一能力计算、Git 刷新 generation、busy，并把全部字段私有化。**

- `capabilities(&self) -> WorkspaceCapabilities`：内部调用既有的 `capabilities_for(WorkspaceCapabilitiesInput { .. })`（第 3 项的产物，逻辑没有改写，只是把拼装输入这一步从 GTK 挪进 Session）。`sync_controls()` 从原来手工拼 6 个字段的 `WorkspaceCapabilitiesInput` 简化成一行 `state.borrow().session.capabilities()`。
- `begin_git_refresh(&mut self) -> Option<GitRefreshRequest>`：没有项目时清空 `git_snapshot` 并返回 `None`；有项目时推进 `git_refresh_generation`，返回携带当前 `ProjectContext`（不只是 root——后台任务需要完整 context 去调用 `git::snapshot`）和新 generation 的 request。
- `is_git_refresh_current(&self, request) -> bool` / `apply_git_snapshot(&mut self, request, snapshot) -> bool`：前者只读校验（复用既有的 `should_apply_git_refresh`，逻辑不变，只是从 GTK 手工拼 root+generation 校验参数改成读 `request.context.root`），后者在校验通过时才安装 `git_snapshot` 并返回是否真的安装了。`git_panel.rs` 的 `refresh()` 用这一对方法重写：`Ok(snapshot)` 分支调用 `apply_git_snapshot` 决定要不要渲染；`Err(error)` 分支单独调用 `is_git_refresh_current` 决定要不要展示错误——跟原代码"一次 staleness 检查同时挡住 Ok/Err 两个分支"的行为完全一致，只是拆成两次调用而不是一次前置检查，因为两个分支现在各自持有不同的借用范围。
- `set_busy(&mut self, busy: bool)`：只写 `busy` 字段。GTK 的 `set_busy()` 函数本身保留在 GTK——它还要根据 `busy`/`document`/`dirty`/`project`/`posts` 选择哪条本地化状态栏文案（`UiMessage::DocumentUnsavedStatus`/`DocumentStatus`/`ProjectOpenedStatus`/`ReadyStatus`），这是 presentation/i18n 决策，不属于 Session；GTK 现在只是把这几个字段的读改成调用只读 getter。
- 一处规格之外的新增：`replace_project_context(&mut self, context: ProjectContext) -> bool`——`window/publish.rs` 里发布成功后可能拿到一份更新过的 `ProjectContext`（比如 exclude 配置被顺带写入），原代码手工比较 root 是否匹配当前项目才替换；这类"完成结果是否仍对应当前状态"的校验逻辑照第 9C 轮的风格收进 Session（跟 `apply_git_snapshot`/`apply_saved_document` 同一个模式：内部校验，返回是否真的应用了），不属于 workspace/document/save-dirty 任何一个已有簇，但显然是同一类"异步结果到达时的 staleness 校验"问题，9C 顺手做掉。root 不匹配或没有项目时返回 `false`、不改变任何字段；root 匹配时只替换 `project`，不碰 `document`/`dirty`/`document_epoch`/`unsaved_documents`——这是"同一个 workspace 的 context 被重新写入配置后更新"，不是 `install_workspace()`。
- **只读 getter**：`project()`/`posts()`/`document()`/`dirty()`/`busy()`/`document_epoch()`/`edit_generation()`/`git_snapshot()`/`unsaved_document_count()`/`has_unsaved_documents()`/`unsaved_document(post_id)`/`unsaved_documents()`（后者返回 `impl Iterator<Item = &PostDocument>`，供 `save_and_close`/`save_all`/`discard_and_close`/关闭确认对话框这类"拿到全部未保存文档列表"的调用点使用）。**没有** `git_refresh_generation()`——按审阅意见去掉了：`begin_git_refresh`/`is_git_refresh_current`/`apply_git_snapshot` 已经把 generation 的生成/比较/更新完整封装，GTK 不需要也不该读原始 `u64`，暴露这个 getter 只会泄漏"Git 刷新用 generation 计数器实现"这个内部细节；`session.rs` 自己的测试通过同一模块内的私有字段访问验证 generation 语义，不需要公开 API。
- **字段全部私有化**：`WorkspaceSession` 的 10 个字段（`project`/`posts`/`document`/`dirty`/`busy`/`document_epoch`/`edit_generation`/`git_snapshot`/`git_refresh_generation`/`unsaved_documents`）不再有任何一个是 `pub`。`cloudstack-gtk` 里所有直接字段访问（包括 9B 结束时特意保留的 `git_refresh_generation`/`git_snapshot`/`busy` 三个写入点）都已经改成方法/getter 调用，跨 `window.rs` + `window/{articles,drafts,frontmatter,git_panel,publish,recent}.rs` 共处理约 110 处编译错误驱动出的调用点。9B 遗留的两处小问题也顺手清理：`session.rs` 顶部那句"字段保持 pub 直到 9B"的过时说明已更新为准确描述 9A→9B→9C 的实际时间线；`close_workspace_is_idempotent_when_already_empty` 已按建议改名为 `close_workspace_when_empty_stays_empty_and_advances_epoch`（该测试实际验证的是"从空状态关闭仍会推进 epoch"，不是真正的幂等性）。

**残留检查**：
```bash
rg '\.session\.(project|posts|document|dirty|busy|document_epoch|edit_generation|git_snapshot|git_refresh_generation|unsaved_documents)\b(?!\(\))' crates/cloudstack-gtk --pcre2
```
零匹配——`cloudstack-gtk` 里对这十个字段名的引用现在全部带 `()`（方法调用），没有任何直接字段访问残留。`cargo tree -p cloudstack-application --edges normal` 确认仍然只依赖 `cloudstack-core`。

**测试**：新增 10 个：`set_busy_updates_the_busy_flag`、`capabilities_reflects_current_state`、`begin_git_refresh_returns_none_and_clears_snapshot_without_a_project`、`begin_git_refresh_advances_generation_and_captures_current_context`、`apply_git_snapshot_installs_when_request_is_current`、`apply_git_snapshot_rejects_a_stale_generation`、`apply_git_snapshot_rejects_after_project_switched`、`replace_project_context_replaces_when_root_matches`（额外断言 document/dirty/document_epoch/unsaved_documents 都不受影响）、`replace_project_context_rejects_a_different_root`、`replace_project_context_rejects_without_a_project`。`cloudstack-application` 103 → 113（+10），`cloudstack-gtk` 不变，workspace 总数 303 → 313。

至此 `cloudstack-gtk` 的 `EditorState` 只剩 `session: WorkspaceSession`（业务状态，全部私有，只经方法/getter 访问）+ `loading_buffer`（SourceView buffer 加载标志）+ `draft_queue`（草稿 FIFO 队列 + GLib 定时器）+ `pending_assets`（待提交图片，跟剪贴板/图片 UI 生命周期绑定）四个字段，第 1～9 项（含 9A/9B/9C）规划的应用状态机迁移全部完成。

**9C.1（已完成，用户在远端复核时发现的边界问题）：Git 刷新 generation 要跨 workspace 生命周期失效，不能只跨同一次会话内的刷新失效。** `is_git_refresh_current` 原本只校验"当前项目 root == request root && 当前 generation == request generation"，而 `install_workspace`/`close_workspace` 都不推进 `git_refresh_generation`。理论序列：打开 `/project`（generation=5）→ 发起刷新 A（request=(/project,5)）→ 关闭→重新打开同一个 `/project`（generation 仍是 5，因为没人推进）→ 旧刷新 A 完成——此时 root 和 generation 都还匹配，A 会被误判成 current，把上一个 workspace 会话的 Git 快照装进新会话。GTK 当前的调用时序不容易实际触发（重新打开后很快会发起新刷新），但这是 `WorkspaceSession` 自身该维护的不变量，不该依赖调用方的时序习惯。

修法：`install_workspace`/`close_workspace` 都追加 `self.git_refresh_generation = self.git_refresh_generation.wrapping_add(1);`——把 `git_refresh_generation` 的语义从"每次 Git 刷新自增"稍微放宽成"Git 快照新鲜度 generation；任何会让旧快照请求失效的事件都推进"，`edit_generation` 不受影响（跟编辑/保存无关，继续保持原样）。

顺手收紧一处小的 API 泄漏：`GitRefreshRequest.generation` 字段改成私有（`context` 保持 `pub`，因为 GTK 确实需要它去执行 `git::snapshot()`）；确认 `cloudstack-gtk` 里没有任何地方读取过 `request.generation`，这个字段本来就只有 `WorkspaceSession` 自己需要看到。

**测试**：新增 1 个回归测试 `git_refresh_from_previous_same_root_workspace_is_stale`（打开 `/project` → 发起刷新 → 关闭 → 重新打开同一个 `/project` → 断言旧刷新请求不再是 current），并更新了 `install_workspace_clears_previous_document_git_snapshot_and_unsaved_map` 里对 `git_refresh_generation` 保持不变的过时断言（现在会推进）。`cloudstack-application` 113 → 114（+1），`cloudstack-gtk` 不变，workspace 总数 313 → 314。

### 阶段 10：无损文本文件合同（拆成 10A/10B/10C）

做 Live Preview 前必须先定义源码到底是什么。

**10A（已完成）：纯 `TextFileFormat` 合同，不碰 `PostDocument`，不碰 GTK。** 新增 `crates/cloudstack-core/src/text.rs`：

```rust
pub enum LineEnding { Lf, CrLf, Mixed }
pub struct TextFileFormat { pub line_ending: LineEnding, pub has_final_newline: bool }
pub struct DecodedText { pub text: String, pub format: TextFileFormat }  // text 内部永远只用 LF

pub fn decode_text(bytes: &[u8]) -> Result<DecodedText, AppError>;
pub fn encode_text(text: &str, line_ending: LineEnding) -> Result<Vec<u8>, AppError>;
```

`lib.rs` 顶层加 `pub mod text;`，跟 `path_guard` 同级（不放进 `services`，因为它不碰文件系统，是纯字节 ↔ 字符串转换）。

- `decode_text`：非法 UTF-8 直接报错（`AppError::Io`），不做有损猜测式解码——CloudStack 的文章/草稿本来就只支持 UTF-8。换行风格检测：单独出现、不成对的 `\r`（老式 Mac 换行）和"CRLF 与裸 LF 同时出现"都归为 `Mixed`；只有纯 CRLF 或纯 LF（含没有任何换行符的单行/空文件，默认判成 `Lf`）才归为对应的单一风格。`has_final_newline` 检查原始字节是否以 `\n` 或 `\r` 结尾（覆盖 LF/CRLF/裸 CR 三种收尾）。
- `encode_text`：**不接收 `TextFileFormat`，只接收 `LineEnding`**——这是用户在首次提交前的复核中发现并要求修正的设计问题（见下）。先防御性地把输入按 `\r\n`→`\n`、`\r`→`\n` 归一化一遍（不假设调用方一定传纯 LF），再按 `line_ending` 决定要不要把 `\n` 换回 `\r\n`；末尾有没有换行、有几个连续空行完全由 `text` 自身内容决定，`encode_text` 不做任何加/去尾部换行的操作。`Mixed` 在 encode 时等同于 `Lf`——按用户决定的策略，混合换行的文件保存时统一规范化为 LF，不尝试恢复原始的逐行混合模式，也不引入"项目默认 EOL"或"多数优先"（那会把配置系统拖进这一步，以后如果需要可以在这个合同之上再加一层）。

  **提交前修正的设计问题**：第一版 `encode_text(text, format)` 会按 `format.has_final_newline` 强制加/去 `text` 末尾的换行。这在纯 decode→encode 往返测试里看不出问题，但接入真实编辑器后是个 bug：磁盘文件原本有末尾换行，用户在 buffer 里用 Backspace 删掉它，保存时 `encode_text` 会拿旧的 `has_final_newline=true` 把刚删掉的换行加回去；反过来，用户在 EOF 按 Enter 新增的换行，也会被旧的 `has_final_newline=false` 删掉。根源是把"读盘时观察到的事实"当成了"保存时的强制指令"，而末尾换行在 source-first 编辑器里本来就是可编辑正文的一部分，不是保存层能替用户决定的格式属性；`has_final_newline` 已经完整体现在 `text` 内容本身里（`decode_text` 从不改动它），不需要也不应该在 encode 时再拿它覆盖一遍。修正为 `encode_text` 只做 EOL 风格转换，`has_final_newline` 保留在 `DecodedText.format` 里作为只读元数据（供诊断、外部文件格式变化检测等场景使用），不再传给 `encode_text`。

**测试**：13 个，在原有 10 个基础上（`encode_adds_or_strips_the_final_newline_before_converting_to_crlf` 随设计修正一起删除，因为它测的正是需要修掉的旧行为）新增 4 个直接针对这处修正的测试：`encode_preserves_absent_final_newline_from_text`、`encode_preserves_multiple_trailing_newlines_from_text`（用户明确要求的两条）、`encode_does_not_reintroduce_a_final_newline_the_user_deleted`、`encode_does_not_strip_a_final_newline_the_user_added`（对称补充的两个方向，直接复现用户描述的 bug 场景）。`cloudstack-core` 155 → 168（+13），其他 crate 不变，workspace 总数 314 → 327。

**10B（已完成）：`PostDocument` 携带 `TextFileFormat`，`read_post`/`write_post` 按用户冻结的三条不变量重写。**

```rust
pub struct PostDocument { .. , pub format: TextFileFormat }  // model.rs，新增字段

pub struct PostWriteResult { pub revision: String, pub format: TextFileFormat }  // posts.rs

pub fn write_post(ctx, id, raw_frontmatter, body, format: TextFileFormat, expected_revision)
    -> Result<PostWriteResult, AppError>;
```

- **不变量 1（revision 必须先于解码算出）**：`read_post` 改成 `revision_of(&bytes)` 在 `decode_text(&bytes)?` 之前调用，两行顺序本身就是这条不变量的落地——外部只改了 EOL 风格的修改现在也会被判定为冲突，不会被 `decode_text` 的归一化悄悄放过。
- **不变量 2（`format` 是 `PostDocument` 的一等字段）**：`model.rs` 直接加 `pub format: TextFileFormat`，没有塞进 Session；`read_post` 从 `decode_text` 返回的 `DecodedText.format` 填充它。
- **不变量 3（frontmatter-only 空正文的特判放在 `write_post`，不放回 `encode_text`）**：`join_markdown` 在 frontmatter-only 且正文为空时，总会在闭合的 `"---"` 后强制补一个 `\n`——这种情况下 `body` 是空字符串，不携带任何信息能区分磁盘原文到底有没有这个换行，是唯一需要 `has_final_newline` 元数据兜底的结构性歧义场景。`write_post` 在调用 `encode_text` 之前，用 `raw_frontmatter.is_some() && body.is_empty() && !format.has_final_newline` 精确判定这一种情况，命中时把 `join_markdown` 强加的那个 `\n` pop 掉；正文非空的一般情况完全不受影响（末尾有没有换行仍然只由 `body` 自身内容决定，`encode_text` 的合同没有被破坏）。
- **`write_post` 返回 `PostWriteResult { revision, format }`，`format` 重新从实际落盘的字节解码得到**，不是把调用方传入的 `format` 原样透传——`Mixed` 换行的文件保存一次之后 `format.line_ending` 会变成 `Lf`，跟磁盘真实状态一致。
- `create_post` 顺带改成 `encode_text(&content, LineEnding::Lf)` 写入（新建文件固定 LF，防御性地归一化调用方传入的任何 `\r\n`/`\r`），返回值仍然委托给 `read_post`，不需要单独返回 format。
- `rename_post`/`validate_rename`/`reapply_colocated_image_rewrite`/`delete_post_with` **没有改动**——按实现前的研究结论，这几个函数都直接对原始字节做局部替换后 `atomic_write`，从不经过 `join_markdown`/`encode_text`，天然无损；它们各自最终都通过 `read_post()` 取得返回值（`reapply_colocated_image_rewrite`/`delete_post_with` 甚至不返回 `PostDocument`），因此自动获得 `format` 字段，不需要任何直接修改。
- **`PostWriteResult` 放在 `posts.rs`，不是 `model.rs`**：跟 `PostDocument`/`PublishResult`/`GitOperationResult` 不同，它是纯 Rust 到 Rust 的写入返回值，不经过任何序列化边界，所以没有 `derive(Serialize)`，也没有放进 `model.rs` 那一批需要跨边界传递的类型里。

**级联到 `cloudstack-application`**：`apply_successful_save`（`save.rs`）新增 `format: TextFileFormat` 参数，在 `Clean` 和 `RevisionOnly` 两个分支都同步 `current.format`，`RevisionOnly` 分支**同时也同步 `unsaved_documents[id].format`**（连同已有的 `unsaved.revision` 更新，两者都描述"新磁盘基线"，语义上必须一起同步）。这一点是用户在复核时纠正的：我最初以为只字节层面等价（`encode_text` 把 `Mixed`/`Lf` 当成同一种编码结果）就够了，但真实问题在状态语义——`unsaved_documents` 里的快照会在用户切换文章再切回来时被直接安装成当前文档（`install_document`），如果 `format` 留着旧的 `Mixed` 标签，用户会看到一份 revision 明明已经是 LF 文件、却显示成 Mixed 的不一致状态。修正后 `RevisionOnly` 的完整不变量是：**只同步 revision/format 这两个磁盘基线属性，绝不覆盖 unsaved 快照里更新后的 body/frontmatter**。`session.rs` 的 `apply_saved_document` 把 `saved.format` 一并传给 `apply_successful_save`；`drafts/batch.rs` 的 `save_documents` 把 `document.format` 传给 `write_post`，用返回的 `PostWriteResult` 同步 `saved.revision`/`saved.format`。

**级联到 `cloudstack-gtk`**：`window.rs` 的 `save_document_then` 把 `task_document.format` 传给后台线程里的 `posts::write_post`，完成回调里用返回的 `PostWriteResult` 构造 `saved: PostDocument { .., revision: result.revision.clone(), format: result.format }`。

**5 个 `PostDocument` 字面量构造点全部加上了 `format` 字段**（10B 开始前用 Explore agent 穷举确认没有遗漏）：`posts.rs::read_post`（生产代码，来自 `decode_text`）、`save.rs::sample_document()`、`session.rs::document(id)`、`drafts/recovery.rs::document(..)`（均为测试 fixture）、`window.rs::save_document_then` 里的 `saved` 构造（生产代码，来自 `PostWriteResult`）。

**残留检查**：
```bash
rg -n "write_post\(" --type rust | grep -v "fn write_post"   # 5 处调用点，全部已传 format / 解构 PostWriteResult
rg -n "PostDocument\s*{" --type rust                          # 逐一核对每处字面量都带 format
```

**测试**：新增 1 个：`write_preserves_frontmatter_only_file_without_final_newline`（不变量 3 的直接回归测试：`"---\ntitle: a\n---"` 无末尾换行的 frontmatter-only 文件，读出 `body == ""` 且 `!format.has_final_newline`，原样写回后字节完全不变）。`read_write_roundtrip_and_revision`/`write_detects_external_modification` 两个既有测试改成解构 `PostWriteResult` 并额外断言 `result.format`；`cloudstack-application` 里 `apply_successful_save` 的三个既有测试补充了 `format`/`new_format` 的构造与断言，但没有新增测试函数。`cloudstack-core` 168 → **169**（+1），`cloudstack-application` 114（不变）、`cloudstack-gtk` 36（不变）、`cloudstack-renderer` 9（不变），workspace 总数 327 → **328**，与预期吻合（这一轮的改动主要是签名/字段级联和既有测试加断言，不是新增大量测试覆盖）。

**10C（已完成）：端到端格式保持验证 + Mixed EOL 提示。** 不引入新架构——GTK 保存路径在 10B 就已经把 `task_document.format` 传给 `write_post()`，这一轮只补测试和一个非阻塞提示。

**Core round-trip 回归测试（`posts.rs`，新增 5 个）**：
- `write_preserves_crlf_and_no_final_newline_after_edit`：CRLF、无末尾换行的 frontmatter 文章，`read_post` → 编辑正文（加一行，仍不按 Enter）→ `write_post`，断言磁盘字节精确到每个 `\r\n` 和缺失的末尾换行都原样保留。
- `write_reflects_user_added_or_removed_final_newline`：通过真实的 `posts::write_post()`（不是 `text.rs` 里的单元测试）复现"EOF 按 Enter / Backspace"两个方向，锁定这条不变量在集成层（含 `raw_frontmatter = None` 路径）也成立，不只是 `encode_text` 单元测试里成立。
- `write_normalizes_mixed_line_endings_to_lf_and_reports_it`：Mixed 换行文件保存后磁盘变成纯 LF，且 `PostWriteResult.format.line_ending == Lf`。
- `revision_changes_when_only_line_ending_style_changes`：同一段文字分别以 LF、CRLF 存盘，`read_post` 出的两个 revision 必须不同——端到端验证不变量 1（revision 基于原始字节）。
- `rename_with_image_rewrite_preserves_crlf_and_no_final_newline`：CRLF、无末尾换行、引用了同名目录图片的文章，重命名触发真正的图片路径 rewrite 分支（`old/` → `new/`），断言磁盘字节除了图片路径前缀之外逐字节不变——`rename_post` 在原始 UTF-8 字符串上做局部替换、从不经过 `encode_text`，这条测试把这个不变量锁死，防止以后有人觉得"反正有 `decode_text()`"就顺手在 rename 里也套一层，把 rename 变成一次隐式的格式规范化操作。

**Draft/application 合同（新增 2 个，未改 `DraftDocument`）**：`DraftDocument` 保持不携带 `format`——它只是未保存的 `body`/`raw_frontmatter` + `base_revision`，草稿恢复时永远先从磁盘重新 `read_post()`，格式天然继承当前磁盘基线；外部编辑器只改 EOL 也会被 raw-byte revision 感知为"disk changed"。
- `session.rs::draft_restore_snapshot_preserves_the_installed_documents_format`：安装一个 CRLF/无末尾换行的 `PostDocument` → 模拟草稿恢复对话框的 "restore" 分支（`set_current_frontmatter` + `mark_document_dirty`）→ 断言生成的 unsaved 快照仍然是 CRLF、无末尾换行，不会被恢复流程重置成默认值。
- `drafts/batch.rs::batch_save_preserves_crlf_line_ending`：CRLF 文件走批量保存的真实 `write_post` 路径，磁盘字节和返回的 `PostWriteResult.format` 都保持 CRLF。

**GTK：只加 Mixed EOL 提示，不加状态层**。`window.rs` 的 `display_document()` 在 `!dirty && document.format.line_ending == LineEnding::Mixed` 时弹一条非阻塞 `toast`（新增 `UiMessage::MixedLineEndingWarning`，走标准 i18n 路径，`zh-CN`/`en-US` 两个 catalog 都补了对应文案，`scripts/check-i18n-hardcoded.py` 复核通过）。`!dirty` 这个条件是关键：只在从磁盘干净加载时提示一次，恢复同一篇文章的 unsaved 快照（`dirty = true`）时不重复弹——用户已经在上一次干净加载时看过提示。**不提示 CRLF**（CRLF 是正常可保持的格式，不是问题），**不提示缺失末尾换行**（那是用户可编辑的正文内容，不是异常状态）。`display_document()` 本来就直接把 `decode_text` 归一化过的 `document.body` 填入 SourceView buffer，这一轮没有再做任何额外的 EOL 转换。

**明确没做的事**（按用户列出的范围边界）：没有给 `DraftDocument` 加 `TextFileFormat`；没有做逐行 EOL map；没有加"项目默认换行符"设置；没有为 Mixed 保存加确认对话框（只是非阻塞 toast）；没有自动补 EOF newline；没有为了让测试更好写而改动 `rename_post`/`delete_post_with` 的实现。

**测试**：新增 7 个（`cloudstack-core` +5、`cloudstack-application` +2，无既有测试改动）。`cloudstack-core` 169 → **174**，`cloudstack-application` 114 → **116**，`cloudstack-gtk` 36（不变——这一轮只加了生产代码里的一次 toast 调用，没有新增单测），`cloudstack-renderer` 9（不变），workspace 总数 328 → **335**，与预期吻合。额外跑了 `python3 scripts/check-i18n-hardcoded.py`（CI 的第一步，这次会话此前几轮都没有显式跑过，10C 引入新用户可见文案后专门确认了一遍）——零违规。

至此 Phase 10 验收冻结：

```text
10A  byte ↔ normalized text contract          ✅
10B  PostDocument/read/write integration      ✅
10C  editor/draft/create/rename round-trip    ✅
```

### 阶段 11：Live Preview 技术验证（三个互不依赖的 spike，不进 main）

1. **tree-sitter Point → TextIter**：验证 ASCII/中文/Emoji/组合字符/多行/空行/末尾无换行；目标 API `fn iter_at_point(buffer: &gtk::TextBuffer, point: tree_sitter::Point) -> Option<gtk::TextIter>`；明确处理 row 越界、byte column 越界、byte column 不在 UTF-8 字符边界、tree-sitter 与 buffer source 不同 generation 这四种情况。
2. **TextTag priority**：验证 GtkSourceView 内置 Markdown highlighting 与自定义 tag（heading scale/strong weight/inline-code background/link foreground/selection/search match/diagnostic）的覆盖顺序。V1 默认保留 GtkSourceView Markdown language，不立刻自定义 `.lang`。
3. **checkbox marker**：验证三个方案——A. alpha=0 透明文字是否仍占位；B. invisible marker + 缩进区图标；C. marker 可见但样式化/可点击。不要未经验证直接决定 overlay 方案。

Spike 在临时分支完成，最终只把测试结论、截图、ADR 决策合入 main，不保留实验垃圾代码。

### 阶段 12：Live Preview V1（语义样式，不隐藏标记）

支持 heading/strong/emphasis/strikethrough/inline code/fenced code/quote/link，**所有 Markdown 标记仍然可见**。建议结构：
```text
crates/cloudstack-gtk/src/live_preview/
  mod.rs
  analysis.rs   // tree-sitter 全量解析，生成 byte + Point ranges，不调用 GTK
  tags.rs       // 创建 CloudStack 私有 TextTag，设置显式 priority
  adapter.rs    // Point → TextIter，只应用/删除 CloudStack 自己的 tags，generation 校验
  fixtures.rs
```
第一版：debounce 后全量 tree-sitter parse；不维护 `InputEdit`；不做 conceal；不做 overlay；不做稳定 block ID；不做自定义 undo；不在 canonical buffer 插 anchor。正式预览继续由现有 renderer 负责，tree-sitter 只负责编辑器装饰。

### 现在明确不要做

不换 UI 框架；不做 AppFlowy/Notion block tree；不把 tree-sitter 当语义权威；不实现协作编辑；不实现稳定 block ID；不把 `TextChildAnchor` 插进 canonical buffer；不把 WorkspaceSession 和 Live Preview 塞进同一个提交；不顺手做外部文件 monitor；不在 V1 做图片、数学和表格 widget。

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
