# Changelog

本项目的重要变更记录在此。版本号遵循 [Semantic Versioning](https://semver.org/)。

## [Unreleased]

## [0.2.4] - 2026-08-06

### Added

- 增加 Fluent 国际化基础设施，提供中文、英文资源、系统语言识别、英文 fallback、动态参数和复数支持。
- 国际化保存、设置、工作区、文章操作、Frontmatter、草稿恢复、Git 发布、搜索和预览界面。
- 增加结构化用户错误映射，将本地化提示与原始诊断信息分离。
- 增加用户可见硬编码文案检查，并接入 Arch Wayland CI。

### Fixed

- 修复异步保存完成回调可能覆盖新编辑状态的问题。
- 修复设置快速连续修改时异步写盘乱序覆盖最新设置的问题。
- 加固草稿批量保存、项目切换和图片资产清理的完成边界。
- 修正 Git 主按钮在未保存文章、空工作区和忙碌状态下的动作与文案。

### Changed

- 将保存完成判定和 GTK 控件状态推导迁移到可独立测试的纯 Rust 逻辑模块。
- Git 发布流程继续保留原始命令和诊断输出，同时将用户提示统一纳入本地化资源。

## [0.2.3] - 2026-08-06

### Added

- 增加原生设置面板，支持跟随系统、浅色和深色配色方案。
- 支持启动时自动打开最近项目，并可恢复每个项目上次打开的文章。
- 欢迎页和最近项目记录保存最后打开的文章，失效文章会安全跳过。

### Fixed

- 保存完成时使用文档 epoch 和编辑 generation 校验异步结果，避免覆盖保存期间的新修改。
- 保存期间阻止图片粘贴，避免新插入的图片被旧保存结果吞掉。
- 保存后按正文实际引用清理待处理图片，并保护被外部修改过的文件。

### Changed

- 设置文件使用版本化 JSON、原子写入，并在损坏或超限时自动隔离。

## [0.2.2] - 2026-08-06

### Added

- 打开没有配置的普通文件夹时提供项目创建向导，自动建议已有常见文章目录；确认后创建
  `.cloudstack.json` 和缺失的文章目录，并可选择加入常用博客 Frontmatter 字段。
- 配置指向的文章目录被删除时提供恢复向导，可重建原目录或改用新的项目内目录。
- 左侧文章栏增加可折叠、可纵向拖动的 Git 停靠区；折叠时保留分支、改动数和主操作，
  展开后显示远端、同步状态与逐项改动，并按仓库拓扑推导安全操作。
- 增加应用内 Git 初始化、仓库级提交身份、origin、GitHub 建仓、首次推送、fetch、普通
  push 和 `pull --ff-only` 流程；所有外部命令均展示脱敏后的命令、输出、退出码与耗时。
- 发布窗口可逐篇选择文章及其图片并把排除偏好保存在本地配置；Frontmatter 日期改用
  年月日选择器，Tags 改用可移除标签块。

### Fixed

- 修复左侧 Git 主按钮无法打开提交窗口的问题，并把唯一的提交入口保留在 Git 面板；
  顶栏保存按钮改为带文字的强调按钮。
- 修复 Git 详情折叠后因分栏上限延迟更新而偶尔残留大块空白区域的问题。

### Security

- 首次提交与日常提交只使用用户选择的文章及同名图片路径，不执行 `git add .` 或
  `git add -A`；本地 CloudStack 配置自动加入 `.git/info/exclude`，不参与发布。
- 禁止凭据终端/AskPass 交互、拒绝内嵌 HTTPS 凭据，并拒绝误操作项目上层的 Git 仓库。
- behind、diverged、冲突或脏工作区不满足纯快进条件时停止自动同步，不自动 stash、
  rebase、merge、强推或删除部分创建成功的 GitHub 仓库。

## [0.2.1] - 2026-08-06

### Fixed

- 在窗口首次映射后重新应用编辑/预览分栏位置，并为预览保留最小宽度，避免高 DPI
  Wayland 布局把实时预览压缩为零宽。

### Changed

- Rust workspace、crate、二进制、GTK App ID、预览协议、E2E 与 Arch 安装产物统一使用
  CloudStack 标识。
- 更新为“云朵 + 内容栈”应用图标，并同时提供 hicolor PNG 与 scalable SVG 资源。
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
