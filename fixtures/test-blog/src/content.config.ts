import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

const blog = defineCollection({
  loader: glob({ base: "./src/content/blog", pattern: "**/*.md" }),
  // 宽松 schema：夹具里故意有一篇没有 frontmatter 的文件（用来测编辑器的往返保真），
  // 字段全部可选才能让 astro dev 正常跑起来
  schema: z.object({
    title: z.string().default("(无标题)"),
    pubDate: z.coerce.date().optional(),
    draft: z.boolean().default(false),
    tags: z.array(z.string()).default([]),
  }),
});

export const collections = { blog };
