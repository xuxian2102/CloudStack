# Markdown 与公式渲染

原生版本通过 `blog-editor-renderer` 在 Rust 进程内把 Markdown 转为静态 HTML。
数学公式由纯 Rust `katex-rs 0.2.4` 生成 HTML + MathML；WebView 不运行数学
JavaScript。KaTeX 0.16.25 的 CSS 和字体保存在 renderer crate 中，由 GTK 的
`blog-editor:` 只读资源协议加载。

## Markdown 方言

统一启用表格、删除线、任务列表、脚注和 pulldown-cmark 原生数学语法。图片
扫描、重命名路径改写和渲染共享同一组选项。源码 HTML 默认显示为转义文本，
只允许 renderer 自己生成的公式标记作为 HTML 输出。

## `$` 方言行为

解析以 pulldown-cmark 0.13.4 为基础，只加入一条窄范围价格兼容规则：行内公式
结束 `$` 后紧跟 ASCII 数字时，把该数学事件降级为普通文本。转义、空白、代码、
链接和 `$$...$$` 块公式仍完全使用 pulldown 原生规则，源码字节范围不变。

| 输入 | 数学事件 |
| --- | --- |
| `$E=mc^2$` | 行内 `E=mc^2` |
| `$$E=mc^2$$` | 块级 `E=mc^2` |
| `原价 $5，现在只需 $2` | 无（结束 `$` 前有空格） |
| `原价 $5，现在只需$2` | 无（结束 `$` 后紧跟数字） |
| `原价 $5，现在只需$ 2` | 行内 `5，现在只需` |
| `\$5`、`\$ 5` | 无 |
| `未闭合 $x` | 无 |
| `` `$HOME` ``、`` `$x$` `` | 无 |
| `https://svelte.dev/docs/svelte/$state` | 无 |
| 链接或图片 URL 中的 `$` | 无 |

`\$` 始终作为字面美元符号。这个兼容层不试图复制 Obsidian 的完整私有方言，
只消除已经确认的价格误判。

## 错误与安全边界

公式解析错误不会中止整篇渲染：输出可见、已转义的占位，同时返回源码字节
范围、公式源码、错误信息和行内/块级类型。KaTeX 使用 `trust = false`；
`\href`、`\htmlClass` 等需要信任的命令只会生成不可执行的 unsupported-command
文本，不会产生用户指定的 URL、class 或 style。渲染器还会中和 Markdown 中的
`javascript:`、`data:`、`file:` 与协议相对地址。WebView 只启用命名隔离 world
里的应用脚本，用于参数化正文替换和滚动同步；文档自身由 CSP 禁止执行脚本，
并且所有导航仍在 GTK 层拦截。
