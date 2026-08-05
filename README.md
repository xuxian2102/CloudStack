<p align="center">
  <img src="https://raw.githubusercontent.com/xuxian2102/CloudStack/main/packaging/arch/icons/dev.xuxian.cloudstack.svg" width="128" alt="CloudStack 图标">
</p>

<h1 align="center">云栈 CloudStack</h1>

<p align="center">
  面向 Arch Linux Wayland 的原生 GTK4 Markdown 编辑器
</p>

<p align="center">
  <a href="https://github.com/xuxian2102/CloudStack/actions/workflows/linux.yml"><img src="https://github.com/xuxian2102/CloudStack/actions/workflows/linux.yml/badge.svg" alt="Arch Wayland CI"></a>
  <a href="https://github.com/xuxian2102/CloudStack/releases/latest"><img src="https://img.shields.io/github/v/release/xuxian2102/CloudStack?display_name=tag&sort=semver" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/xuxian2102/CloudStack" alt="Apache License 2.0"></a>
</p>

云栈（CloudStack）是一个只面向 **Arch Linux rolling + 原生 Wayland** 的个人 Markdown
编辑器。当前版本 `v0.2.2` 使用纯 Rust、GTK4、libadwaita、GtkSourceView 5 和
WebKitGTK 6，不再包含 Tauri、React、Node.js 或 Vite 构建链。

早期 Tauri 版本及其历史保留在
[`Astro_Editor`](https://github.com/xuxian2102/Astro_Editor) legacy 仓库中。CloudStack
从原生 GTK4 版本开始维护；新项目使用 `.cloudstack.json`，并继续兼容已有项目的
`.blog-editor.json`。

## 项目状态

CloudStack 目前围绕“写 Markdown、预览内容、管理本地图片、透明发布到 Git”这条个人工作流
持续完善。它是一个明确限定运行环境的个人项目，不追求跨平台兼容；欢迎在支持的环境中提交
可复现的问题和小范围改进建议。

## 功能

- GtkSourceView 源码编辑、Markdown 高亮、查找替换和原生撤销栈；
- WebKitGTK 双栏实时预览，Markdown 与 KaTeX 公式全部在 Rust 中静态渲染；
- Frontmatter 默认隐藏在右侧属性抽屉，日期使用年月日选择器，Tags 显示为可移除标签块；
- Wayland 剪贴板图片粘贴、文章同名资产目录和安全的本地图片预览；
- 新建、重命名、可恢复删除、外部修改冲突检测和崩溃草稿恢复；
- 左侧常驻 Git 状态与应用内初始化、远端、GitHub 建仓、按文章提交、推送和纯快进同步；
- 深浅主题、编辑/预览双向比例滚动和公式错误源码跳转。

项目不支持 X11/XWayland、其他 Linux 发行版、Windows、macOS、MDX、Mermaid、
插件或远程图片加载。

GitHub 建仓功能可选依赖 `github-cli`，并要求用户先在终端完成 `gh auth login`；应用
不会读取或展示 token，也不会代替用户处理登录。

## 快速开始

通过仓库里的 Arch VCS 包构建安装：

```bash
sudo pacman -S --needed base-devel git
git clone https://github.com/xuxian2102/CloudStack.git
cd CloudStack/packaging/arch
makepkg -si
```

启动器会检查 `WAYLAND_DISPLAY` 并强制 GTK 使用 Wayland。卸载：

```bash
sudo pacman -Rns cloudstack-git
```

也可以直接从源码运行：

```bash
cargo run -p cloudstack-gtk --bin cloudstack
```

第一次打开普通文件夹时，CloudStack 会提供项目初始化向导；不需要预先手写配置文件。

## 博客项目配置

直接打开一个普通文件夹即可开始：如果目录中还没有配置，CloudStack 会识别常见文章
目录并询问是否创建 `.cloudstack.json`，也可以同时加入常用博客属性。只有旧配置时会原地
读写 `.blog-editor.json`，不会自动改名；两个文件同时存在时应用会要求只保留一个。
编辑器也可以作为普通笔记工具使用；项目目录和文章扩展名不依赖 Astro。手工配置示例：

```json
{
  "version": 1,
  "contentDir": "src/content/blog",
  "extensions": [".md"],
  "frontmatter": {
    "fields": [
      { "name": "title", "type": "string", "required": true },
      { "name": "pubDate", "type": "date", "required": true },
      { "name": "draft", "type": "boolean", "default": false },
      { "name": "tags", "type": "tags" }
    ]
  },
  "assets": { "mode": "colocated" },
  "git": { "excludedArticles": [] }
}
```

Frontmatter 可以按文章一键添加或移除；未配置字段、YAML 注释、字段顺序和引号风格
会在修改已配置字段时保留。

`.cloudstack.json`/`.blog-editor.json` 只保存本机编辑器设置，不属于博客内容。CloudStack
会把它们加入当前 checkout 的 `.git/info/exclude`；如果旧项目已经跟踪配置，可在 Git
面板执行“停止跟踪配置”。发布窗口只暂存勾选文章及其同名图片目录，选择可记入本地
配置长期保留；`.env`、构建产物和其他项目文件始终显示为非受管，不会被应用提交。

## 开发

使用 Arch 官方 Rust，不需要 rustup：

```bash
sudo pacman -S --needed \
  rust git gtk4 libadwaita gtksourceview5 webkitgtk-6.0
cargo run -p cloudstack-gtk --bin cloudstack
```

完整本地检查：

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --release --locked
```

原生 Wayland 冒烟测试还需要 `sway`、`dbus` 和 `ttf-dejavu`：

```bash
cargo build -p cloudstack-gtk --features e2e --bin cloudstack --locked
bash test/native-wayland-smoke.sh target/debug/cloudstack
```

## 安全边界

GTK 是唯一持有文件与 Git 权限的 UI。预览文档的原始 HTML会被转义，KaTeX 使用
`trust=false`；WebView 的 CSP 禁止文档脚本、远程资源和通用文件访问。隔离脚本只做
参数化正文替换与滚动同步，本地图片只能经 `cloudstack:` 协议和 core 路径校验读取。

架构细节见 [原生架构](docs/cloudstack-architecture.md)，Markdown 方言见
[Markdown 与公式渲染](docs/markdown-rendering.md)，Arch 包说明见
[packaging/arch/README.md](packaging/arch/README.md)。

## 项目反馈

CloudStack 目前暂不开放常规外部贡献。安全问题仍请按照 [安全策略](SECURITY.md) 私下报告，
不要在公开渠道披露凭据或可直接利用的漏洞细节。

## 许可证

本项目采用 [Apache License 2.0](LICENSE)。
