# 安全策略

CloudStack 是面向个人本地 Markdown 和 Git 工作流的 Arch Linux Wayland 应用。安全修复会
优先跟进最新发布版本和 `main` 分支。

请不要在公开 Issue、Pull Request 或聊天中发布凭据、私钥、真实项目内容或可直接利用的
漏洞细节。请通过 GitHub 仓库的 **Security → Report a vulnerability** 私下提交，并尽量包含：

- 受影响的 CloudStack 版本和系统环境；
- 最小复现步骤或测试项目；
- 影响范围和可能的利用方式；
- 不包含真实凭据的日志或截图。

应用设计上不会读取或展示 GitHub token；Git 命令被设置为非交互，命令记录会脱敏；预览中的
用户 HTML、危险链接、远程资源和通用 `file:` 访问均受到限制。
