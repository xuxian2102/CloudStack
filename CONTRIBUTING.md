# 贡献指南

感谢你关注 CloudStack。项目只支持 Arch Linux rolling、原生 Wayland、GTK4 和 WebKitGTK 6；
不接受为了 Windows、macOS、X11/XWayland 或其他发行版增加兼容分支的改动。

## 开始之前

- 先搜索已有 Issue，确认问题还没有被报告。
- Bug 请提供版本、桌面环境、复现步骤、预期结果和实际结果。
- UI 改动最好附截图；涉及 Wayland、剪贴板或 WebKit 的问题请附 CI 或本地日志。

## 本地检查

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

## 提交和 Pull Request

- 一个提交只解决一个主题，提交信息简洁说明结果。
- 不要提交构建产物、本地 `.cloudstack.json`、`.blog-editor.json`、凭据或个人配置。
- 改动行为时同时补测试或说明为什么无法自动测试。
- Pull Request 描述应说明改了什么、为什么改、如何验证，以及是否影响现有项目兼容性。

维护者会优先处理可复现、范围清楚并通过本地检查的改动。
