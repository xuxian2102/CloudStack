# 云栈 CloudStack

[![Arch Wayland CI](https://github.com/xuxian2102/CloudStack/actions/workflows/linux.yml/badge.svg)](https://github.com/xuxian2102/CloudStack/actions/workflows/linux.yml)

云栈（CloudStack）是一个只面向 **Arch Linux rolling + 原生 Wayland** 的个人 Markdown
编辑器。`v0.2.1` 使用纯 Rust、GTK4、libadwaita、GtkSourceView 5 和 WebKitGTK 6，
不再包含 Tauri、React、Node.js 或 Vite 构建链。

早期 Tauri 版本及其历史保留在
[`Astro_Editor`](https://github.com/xuxian2102/Astro_Editor) legacy 仓库中。CloudStack
从原生 GTK4 版本开始维护；新项目使用 `.cloudstack.json`，并继续兼容已有项目的
`.blog-editor.json`。

## 功能

- GtkSourceView 源码编辑、Markdown 高亮、查找替换和原生撤销栈；
- WebKitGTK 双栏实时预览，Markdown 与 KaTeX 公式全部在 Rust 中静态渲染；
- Frontmatter 默认隐藏在右侧属性抽屉，日期使用 GTK 日历选择；
- Wayland 剪贴板图片粘贴、文章同名资产目录和安全的本地图片预览；
- 新建、重命名、可恢复删除、外部修改冲突检测和崩溃草稿恢复；
- 展示 Git 分支/upstream/ahead-behind，只提交受管文件并可选择推送；
- 深浅主题、编辑/预览双向比例滚动和公式错误源码跳转。

项目不支持 X11/XWayland、其他 Linux 发行版、Windows、macOS、MDX、Mermaid、
插件或远程图片加载。

## 安装

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

## 博客项目配置

要打开的目录需要包含 `.cloudstack.json`；只有旧配置时会原地读写
`.blog-editor.json`，不会自动改名。两个文件同时存在时应用会要求只保留一个。
编辑器也可以作为普通笔记工具使用；项目目录和文章扩展名不依赖 Astro。最小配置：

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
  "assets": { "mode": "colocated" }
}
```

Frontmatter 可以按文章一键添加或移除；未配置字段、YAML 注释、字段顺序和引号风格
会在修改已配置字段时保留。

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

## 许可证

本项目采用 [Apache License 2.0](LICENSE)。
