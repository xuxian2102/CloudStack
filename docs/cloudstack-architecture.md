# CloudStack 原生架构

运行契约只有 **Arch Linux rolling + 原生 Wayland**。应用是 Rust workspace，不包含
Tauri、Node.js 或浏览器前端构建链。

## Workspace

```text
crates/cloudstack-core/       文件、配置、Frontmatter、草稿、图片与 Git 领域逻辑
crates/cloudstack-renderer/   Markdown 方言、纯 Rust KaTeX 和内嵌 CSS/字体
crates/cloudstack-gtk/        GTK4/libadwaita/GtkSourceView/WebKitGTK 应用
fixtures/test-blog/     只读领域测试与 Wayland smoke 项目
packaging/arch/         Arch VCS 包、desktop entry 和 Wayland 启动器
```

依赖始终从 UI 指向领域层：`cloudstack-gtk -> cloudstack-renderer -> cloudstack-core`，同时 GTK 可以直接
调用 core 的文章、资产、草稿和 Git 服务。core 与 renderer 不依赖 GTK。

## 国际化

用户可见文案由 `cloudstack-gtk/src/i18n` 的 `UiMessage` 语义枚举索引，实际翻译存放在
`cloudstack-gtk/locales/{locale}/main.ftl`，通过 Fluent 在编译期嵌入二进制。首批已迁移保存
提示、Git 主按钮和设置面板；当前支持 `zh-CN`、`en-US`，按系统语言在启动时选择，无法匹配时
回退到显式的 `en-US`。语言切换暂不在运行时刷新，修改后重启应用生效。

`cloudstack-core` 继续返回结构化错误和诊断数据，不负责翻译；错误到用户消息的映射将在后续
迁移错误提示时补齐。测试会解析两个 catalog，检查语法、key 集合、变量集合和所有
`UiMessage` 词条，避免单个语言缺少消息时被 loader 的 fallback 掩盖。

## 文档状态与写入

`EditorState` 保存当前 `ProjectContext`、文章 revision、dirty/busy 状态、文档 epoch、
待提交图片和串行草稿队列。GtkSourceView 只装载 Markdown 正文；原始 Frontmatter
独立保存在 `PostDocument`，通过右侧抽屉做 lossless 字段更新，保存时才重新组装文件。

窗口层中的几类状态判定已经收敛到可独立测试的 Rust 边界：

- `app/save.rs` 负责比较 document id、document epoch 和 edit generation，分类异步保存
  完成结果，并只在结果仍对应当前编辑版本时更新正文、revision 和未保存集合；
- `app/settings.rs` 的 `SettingsWriter` 维护完整 `AppSettings` 快照，保证同一时刻只有
  一个设置写盘任务，后续修改只保留最新 pending 快照，避免异步完成乱序覆盖新设置；
- `ControlModel` 将 `EditorState` 映射为控件可用性，`EffectivePrimaryAction` 再把仓库建议
  动作与 busy、未保存文章数量组合为 Git 面板当前主按钮动作。

`window.rs` 只负责 GTK 事件编排、异步任务派发和模型渲染，不再承担上述纯状态判定。
交互式保存、批量保存和关闭流程由全局 `busy` 保证单飞：busy 期间会禁用会触发新操作
的控件和编辑器，因此当前不引入额外的 `SaveState`/`op_id` 状态机。若将来允许保存期间
继续编辑或并行操作，再以实际竞态为依据增加操作代次，而不是预先扩大状态模型。

### 草稿与批量保存审计结论

`window/drafts.rs` 的 `DraftQueue` 使用 `active + pending` FIFO 队列串行执行草稿写入、
删除、批量保存和放弃清理。自动保存产生的新快照会排在当前任务之后；保存成功后排入的
草稿删除也会等待此前的写入完成，因此旧写入不会覆盖随后清理。批量保存按文章逐项生成
结果：成功文章立即从 `unsaved_documents` 移除并清理对应草稿，失败文章保留，任何失败
都会阻止关闭或继续 Git 操作。

文章读取恢复回调同时检查 `document_epoch`、文章 ID、`busy` 和当前 dirty 状态。批量保存
与放弃关闭流程在 `busy` 期间禁止项目切换和窗口关闭，旧项目的批量回调不会落到新项目。
自动草稿写入和删除不占用全局 busy，项目切换可以与它们并行；这类任务只在失败时提示，
其回调现在同时校验项目根路径和文章 ID，避免两个项目存在同名文章时把旧项目错误显示到
新项目。当前没有证据表明需要引入更大的 Draft 状态机或操作 ID。

所有磁盘与 Git 操作都通过 `gio::spawn_blocking` 离开 GTK 主线程。保存使用 revision
比较和同目录原子替换；外部修改、路径穿越、符号链接逃逸和无效文章 ID 在 core 中
拒绝。草稿写入与删除进入同一串行队列，防止旧自动保存覆盖正常保存后的清理。

打开普通文件夹时，缺少配置是独立的 `MissingProjectConfig` 状态，不再退化成不透明错误。
GTK 先展示初始化向导；core 只在用户确认后创建文章目录，并以 `persist_noclobber` 原子写入
`.cloudstack.json`。已有新/旧配置、非法相对路径和符号链接逃逸都会在写入前拒绝。
配置存在但文章目录被外部删除时会进入独立修复流程：用户可重建原目录或选择新的项目内
相对目录，core 再以同一套路径和符号链接规则校验并保存。

## 实时预览

编辑变化先进入自适应防抖：100 KiB 以内 200ms、100–500 KiB 350ms、更大正文
500ms。任意时刻最多运行一个渲染；运行期间的新输入只保留最新快照。结果回到主线程
后必须同时匹配 `document_epoch` 与 `generation`，因此切换文章和快速输入不会让旧内容
回跳。

`cloudstack-renderer` 一次遍历 pulldown-cmark 事件，生成静态 HTML + MathML，并保留公式
UTF-8 字节范围。WebView 首次只加载固定外壳，之后通过
`call_async_javascript_function` 的参数传递 HTML；正文绝不拼接到脚本源码。

命名隔离 world 中的应用脚本只负责：

- 替换 `#preview-root`；
- 保留并设置滚动比例；
- 按动画帧把预览滚动比例回传 GTK。

文档 CSP 为默认拒绝，不允许自身脚本、frame、object、表单、网络连接或远程资源。

## 预览资源与导航

`cloudstack:` 自定义协议分为两个命名空间：

- `/app/` 只提供 renderer 编译时嵌入的 KaTeX CSS 与字体；
- `/current/` 只读取当前文章相对路径引用的图片。

图片读取复用 core 的 percent decode、canonicalize、内容根目录、文件类型和 25 MiB
限制，不开放 `file:` 或任意路径读取。资源错误只影响预览，编辑、草稿和保存继续工作。

页内锚点留在预览。只有用户手势点击的 `http`、`https`、`mailto` 会交给系统应用；
相对文件、`javascript:`、`data:`、`file:`、未知 scheme、弹窗和下载全部阻止。

## 图片与 Frontmatter

Wayland 图片粘贴由 GDK clipboard 直接读取。图片先写入文章 stem 同名目录并登记为
pending；文章保存后确认，放弃修改时只清理仍未被磁盘 Markdown 引用的 pending 文件。
重命名只移动解析器确认被正文引用的同名目录图片，并同步改写 Markdown 目标。

Frontmatter 可以缺省。添加时按项目字段配置生成默认值；日期字段使用独立年月日控件并
自动约束闰年和每月天数，Tags 以可移除标签块编辑。删除和字段编辑都只修改内存中的
当前文章，随后走统一 dirty、草稿和保存流程。

## Git 发布

左侧底部 Git 停靠区默认折叠为一行，保留分支/upstream、改动数和当前主操作；展开后
显示仓库拓扑、同步关系、远端和逐项改动，并可拖动分隔线改变高度。面板据此选择初始化、
提交、配置远端、首次推送、push 或纯快进同步。Git/gh 检测、状态与操作均在后台执行；
`gh auth login` 仍由用户在终端完成。

所有提交通过同一个 `ManagedScope` 生成明确路径列表，只包含用户在发布窗口勾选的文章
以及文章 stem 同名目录里的图片。项目配置属于本机状态，打开独立仓库时写入仓库本地的
`.git/info/exclude`，永不进入普通发布范围；旧仓库已跟踪配置时由显式操作创建删除提交。
不存在 `git add .` 或 `git add -A` 路径。
项目目录必须等于 Git top-level；如果只检测到父目录仓库，所有写操作都会停止。

每条外部命令生成 `CommandTrace`，记录脱敏命令、stdout/stderr、退出码与耗时。发布按
stage → commit → 可选 push 执行并保留部分成功；GitHub 建仓失败同样只报告当前状态，
不会自动删除可能已经创建的远端仓库。

远端更新只允许 `pull --ff-only`，而且要求 upstream 存在、本地不 ahead、整个工作区
干净。behind 加本地改动、diverged、冲突、认证和任何非 fast-forward 情况都停止，
不自动 stash、merge、rebase 或强推。HTTPS 凭据助手只在用户明确勾选后通过
`gh auth setup-git --hostname github.com` 配置。

## 验证

CI 在 Arch rolling 容器中执行格式检查、workspace check、严格 Clippy、全部测试、
release build、Arch 包元数据检查，并在关闭 XWayland 的 headless Sway 中启动真实
GTK4/WebKitGTK 应用和发送原生关闭请求。
