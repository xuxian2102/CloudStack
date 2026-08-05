# 原生 GTK4 迁移

## 目标

CloudStack 已从 legacy Tauri/WebView 双层应用迁移为只面向 Arch Linux Wayland 的 Rust
原生应用。迁移没有复刻 React 组件树，而是保留已经验证过的领域规则，并让 GTK、
GDK、GIO 和 libadwaita 接管平台能力。

## 新结构

```text
Cargo.toml                 原生应用 workspace
crates/cloudstack-core/          不依赖任何 UI 框架的领域核心
crates/cloudstack-renderer/      Markdown 与纯 Rust KaTeX 静态渲染
crates/cloudstack-gtk/           GTK4/libadwaita 应用和交互状态
test/                      无 XWayland 的原生 Wayland 冒烟测试
```

`cloudstack-core` 当前包含：

- 项目配置读取、验证和原子更新；
- 文章列表、Frontmatter 拆装、原子写入与外部修改冲突检测；
- 路径越界和符号链接防护；
- 图片资产、待提交图片事务和安全预览读取；
- 崩溃草稿；
- Git 状态、受管内容提交和推送。

它不依赖 Tauri、GTK 或窗口句柄，因此可以被 GTK UI、命令行测试工具和未来的索引
进程共同使用。

## 已接通的原生流程

- `AdwApplication` 窗口与原生关闭生命周期；
- GTK 文件夹选择器打开含 `.cloudstack.json` 的项目，并兼容旧 `.blog-editor.json`；
- 左侧文章列表；
- GtkSourceView 只编辑 Markdown 正文，Frontmatter 不在文中显示；
- Markdown 语法高亮、行号、当前行、括号匹配、自动缩进和原生撤销栈；
- 编辑器配色随 libadwaita 深浅模式自动切换；
- `Ctrl+F`/`Ctrl+H` 原生查找替换栏、F3 前后导航、区分大小写与全部替换；
- `Ctrl+N` 新建、F2 重命名、`Ctrl+Delete` 可恢复删除文章；
- 项目扫描、文章打开/保存和文章管理通过 GIO I/O 线程池执行，不阻塞 GTK 主线程；
- 编辑停顿 700ms 后自动写入 XDG 应用数据目录，重新打开文章时可恢复草稿；
- 草稿写入、正常保存后的清理严格串行，放弃修改关闭时会等待恢复草稿清理完成；
- Frontmatter 可按文章选择添加或移除，新文章默认是普通 Markdown；
- 原生 Frontmatter 属性面板支持字符串、布尔值和标签字段，日期通过 GTK 日历选择；
- 表单通过 lossless YAML CST 定点修改字段，保留注释、顺序、布局和未配置字段；
- 独立 `cloudstack-renderer` 使用 pulldown-cmark 与纯 Rust KaTeX 生成静态 HTML + MathML；
- Markdown 渲染、图片扫描和路径改写共用同一套方言配置；
- 公式错误带源码字节范围返回，用户原始 HTML 默认转义；
- WebKitGTK 6 左右双栏实时预览，按正文大小做 200/350/500ms 防抖；
- Markdown 在 GIO 后台线程渲染，单任务队列只保留最新输入，并用文章 epoch 与
  generation 丢弃过期结果；
- KaTeX CSS/字体由 `cloudstack:` 只读协议提供，文章图片仍通过 core 的路径、
  符号链接、类型与 25 MiB 限制读取，不开放 `file:`；
- 预览使用严格 CSP 和隔离脚本 world，参数化替换正文，外部导航只放行用户点击的
  HTTP(S)/mailto 并交给系统应用；
- 编辑器与预览双向比例滚动同步，公式诊断可选中并滚动到对应源码；
- Frontmatter 属性面板改为 HeaderBar 按钮控制的右侧 overlay 抽屉，默认关闭；
- HeaderBar 发布按钮在后台读取 Git 状态，展示分支、upstream、ahead/behind 以及
  受管/非受管改动；有未保存正文时先完成保存，再打开发布对话框；
- 发布对话框要求提交信息，有 upstream 时默认推送，并保留 stage/commit/push
  分阶段结果；冲突或没有受管改动时不会执行发布；
- `Ctrl+O` 和 `Ctrl+S` 原生 action；
- 基于 revision 的冲突检测和原子保存；
- 未保存修改的关闭保护；
- GDK 直接读取 Wayland 图片剪贴板，保存到文章同名目录并插入 Markdown；
- 保存时确认图片事务，放弃修改时清理未提交图片。

普通文本粘贴、中文输入法 composition、光标、选区、撤销和重做由 `GtkSourceView`
负责，不再经过浏览器事件和 Tauri IPC。

## 有意改变的设计

1. UI 不再镜像旧 React 组件边界，当前只维持一个主窗口状态对象。
2. 编辑区只显示正文；Frontmatter 和正文同属当前 `PostDocument`，元数据只通过右侧
   属性面板编辑，保存时再由 core 组装成完整 Markdown 文件。
3. 文件服务由 GTK 直接调用 core，不建立一个仿 Tauri 的内部 RPC 层。
4. 图片粘贴使用 GDK clipboard，不再依赖 `arboard`、`wl-clipboard-rs` 或外部
   `wl-paste`。
5. Tauri/React/Node/Vite 构建链已在原生功能闭环后删除。

## 迁移结果

`v0.2.0` 的打开、编辑、图片、草稿、Frontmatter、实时预览和 Git 发布均由原生
workspace 完成。Arch 包直接执行 `cargo build --release --workspace`；CI 不再安装
Node.js 或运行 Tauri 步骤。

## 本地依赖

`v0.2.0` 运行依赖：

```bash
sudo pacman -S --needed rust gtk4 libadwaita gtksourceview5 webkitgtk-6.0
```

## 验证

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo run -p cloudstack-gtk --bin cloudstack
```
