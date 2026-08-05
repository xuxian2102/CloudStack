---
# 这条注释必须在保存后原样保留
pubDate: 2026-01-02 # 字段顺序故意不常规：日期在标题前面
title: '单引号标题'
customTool:
  nested: "编辑器不认识的嵌套字段"
  count: 3
aliases: [old-url, another-url]
draft: true
tags:
  - "双引号标签"
  - plain-tag
legacy_field: 保持原样 # 行尾注释也要保留
---
# test 

这篇文章的 frontmatter 专门用来验收"保存不破坏原始结构"：(已编辑)

1. 置顶注释、行尾注释
2. 单引号、双引号、无引号混用
3. 编辑器配置里没有声明的 `customTool`、`aliases`、`legacy_field`
4. 不常规的字段顺序
