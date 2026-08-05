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

## 文档状态与写入

`EditorState` 保存当前 `ProjectContext`、文章 revision、dirty/busy 状态、文档 epoch、
待提交图片和串行草稿队列。GtkSourceView 只装载 Markdown 正文；原始 Frontmatter
独立保存在 `PostDocument`，通过右侧抽屉做 lossless 字段更新，保存时才重新组装文件。

所有磁盘与 Git 操作都通过 `gio::spawn_blocking` 离开 GTK 主线程。保存使用 revision
比较和同目录原子替换；外部修改、路径穿越、符号链接逃逸和无效文章 ID 在 core 中
拒绝。草稿写入与删除进入同一串行队列，防止旧自动保存覆盖正常保存后的清理。

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

Frontmatter 可以缺省。添加时按项目字段配置生成默认值；日期字段使用 GTK Calendar。
删除和字段编辑都只修改内存中的当前文章，随后走统一 dirty、草稿和保存流程。

## Git 发布

发布前若文章 dirty，必须先完成同一 revision 保护的保存。状态与发布均在后台执行。
对话框展示 branch、upstream、ahead/behind、受管和非受管改动；只有内容目录和项目
实际采用的配置文件会被暂存。新项目使用 `.cloudstack.json`，旧项目可继续原地使用
`.blog-editor.json`。未解决冲突、空提交信息或没有受管改动时停止。

发布按 stage → commit → 可选 push 执行，结果保留已经成功的阶段。push 失败不会伪装
成 commit 失败，UI 会刷新状态并显示 commit hash 与具体停止阶段。

## 验证

CI 在 Arch rolling 容器中执行格式检查、workspace check、严格 Clippy、全部测试、
release build、Arch 包元数据检查，并在关闭 XWayland 的 headless Sway 中启动真实
GTK4/WebKitGTK 应用和发送原生关闭请求。
