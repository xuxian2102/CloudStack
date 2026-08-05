# Arch Linux 安装

该目录提供只面向 Arch Linux 原生 Wayland 会话的 `cloudstack-git` 包。

```bash
sudo pacman -S --needed base-devel git
cd packaging/arch
makepkg -si
```

`makepkg` 会从 GitHub `main` 获取源码，按 lockfile 直接构建 Rust workspace 的
release 二进制，再通过 pacman 安装：

- `/usr/bin/cloudstack`：检查 `WAYLAND_DISPLAY` 并强制 GTK Wayland backend；
- `/usr/lib/cloudstack/cloudstack`：实际 GTK4/libadwaita 二进制；
- desktop entry 与 hicolor 应用图标。

更新时在本目录重新运行 `makepkg -si`；卸载使用：

```bash
sudo pacman -Rns cloudstack-git
```

这是 VCS 包，只构建已经推送到 GitHub 的提交；工作区中的未提交内容不会进入安装包。
项目采用 Apache License 2.0；安装包会将完整许可证放在
`/usr/share/licenses/cloudstack-git/LICENSE`。

运行时依赖由 pacman 安装，包括 GTK4、libadwaita、GtkSourceView 5 和 WebKitGTK 6；
Git 是内置发布功能的可选依赖。不需要 Node.js、pnpm 或 Tauri。
