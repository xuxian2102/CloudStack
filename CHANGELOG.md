# Changelog

本项目的重要变更记录在此。版本号遵循 [Semantic Versioning](https://semver.org/)。

## [Unreleased]

### Changed

- Rust workspace、crate、二进制、GTK App ID、预览协议、E2E 与 Arch 安装产物统一使用
  CloudStack 标识。
- 新项目配置名改为 `.cloudstack.json`；旧 `.blog-editor.json` 继续原地读写，草稿也可
  从旧应用数据目录恢复，避免升级打扰现有项目。

## [0.2.0] - 2026-08-05

### Added

- 全面迁移到 Rust GTK4、libadwaita、GtkSourceView 5 与 WebKitGTK 6。
- 增加纯 Rust Markdown/KaTeX 静态渲染、公式诊断与窄范围价格 `$` 兼容规则。
- 增加后台防抖实时预览、旧 generation 淘汰、双向滚动同步和本地图片资源协议。
- 增加可选 Frontmatter 属性抽屉、GTK 日期选择器和原生 Git 发布对话框。

### Security

- 用户 HTML 默认转义，危险 Markdown URL 被中和，KaTeX 使用 `trust=false`。
- 预览使用严格 CSP、隔离脚本 world 和参数化 DOM 更新，不开放 `file:` 通用访问。
- 本地图片读取限制在内容目录内，并拒绝遍历、符号链接逃逸、非图片与超过 25 MiB 的文件。

### Changed

- 原生项目以“云栈 CloudStack”在独立仓库继续维护；旧 Tauri 仓库保留为 legacy，
  `.blog-editor.json` 等内部标识继续兼容现有项目。
- Arch 包更名为 `cloudstack-git`，安装时替换旧 `blog-editor-git` 包。
- 删除 Tauri、React、CodeMirror、Node.js、pnpm 和 Vite 构建链。
- Arch 包改为直接构建 Rust workspace；CI 精简为原生检查、release build、包检查和
  无 XWayland 的 GTK4/WebKitGTK smoke。
- 预览不再启动 Astro；相同 Markdown/数学语法可用于博客文章和普通笔记。

## [0.1.2] - 2026-08-05

### Added

- 集中管理中文界面文案，并以共享清单约束 Rust 与 TypeScript 的结构化错误码和插值参数。
- 为自定义对话框补齐初始焦点、焦点圈定、Escape 语义、焦点恢复及真实 WebKitGTK 回归。
- 主窗口启用严格 CSP；生产 custom protocol 与桌面 Vite 开发路径均纳入 Wayland E2E 验收。

### Fixed

- 修复弹窗打开时半成品标签被意外提交、文章新建或重命名输入被静默取消的问题。
- 修复叠加弹窗的视觉层级与键盘焦点栈不一致，导致不可见确认框接收操作的问题。
- 修复预览启动超时或运行后崩溃时，最后一段 stdout/stderr 可能未进入诊断日志的问题。
- 修复桌面端 `tauri dev` 直接加载 Vite 时没有实际应用开发 CSP 的问题。

### Changed

- 错误 payload 改用参数类型明确的命名构造器，序列化不再因手工参数不匹配而退化为不透明 IPC 错误。
- 发布检查增加 Biome 可访问性与 Hook 规则、前后端错误协议校验及更完整的原生 Wayland 场景。

## [0.1.1] - 2026-08-05

### Fixed

- 修复安装版窗口收到关闭请求后因缺少 `destroy` 权限而无法退出。
- 修复中文输入法组合期间，光标之外的已渲染图片退回 Markdown 源码。

### Added

- release 构建写入有大小与数量上限的本地诊断日志。
- 原生 Wayland E2E 增加由 Sway compositor 发起的真实关闭握手回归；发布标签同时验证文件日志。

## [0.1.0] - 2026-08-04

- 首个可用版本：安全的 Markdown/Frontmatter 编辑、实时排版、Wayland 图片导入、
  Astro 实页预览、草稿恢复、文章与资产管理、Git 发布以及 Arch `makepkg` 安装。
