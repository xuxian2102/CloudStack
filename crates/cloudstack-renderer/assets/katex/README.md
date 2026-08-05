# KaTeX 静态资源

此目录来自官方 npm 包 `katex@0.16.25`，与 `katex-rs 0.2.4` 跟踪的
KaTeX 版本一致。

- 仅保留 `dist/katex.min.css`、`dist/fonts/*` 和上游 `LICENSE`；
- 不包含 `katex.min.js` 或任何其他 JavaScript；
- npm 完整性：`sha512-woHRUZ/iF23GBP1dkDQMh1QBad9dmr8/PAwNA54VrSOVYgI12MAcE14TqnDdQOdzyEonGzMepYnqBMYdsoAr8Q==`。

下一阶段由 GTK 的本地资源处理器提供这些文件，CSS 中的相对 `fonts/` URL
保持不变。
