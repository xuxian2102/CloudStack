## 变更说明

<!-- 简要说明改了什么，以及为什么需要这个改动。 -->

## 验证

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --workspace --locked`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features --locked`
- [ ] 与改动相关的 Wayland/UI 手工验证已完成

## 兼容性与安全

- [ ] 没有提交 `.cloudstack.json`、`.blog-editor.json`、凭据或构建产物
- [ ] 没有扩大支持范围之外的 Windows、macOS、X11/XWayland 或其他发行版分支
- [ ] 如果改动了项目配置、Markdown、Git 或预览安全边界，已补充测试和文档
