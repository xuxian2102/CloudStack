use std::ops::Range;

use pulldown_cmark::{CowStr, Event, Options, Parser};

/// 返回 CloudStack 所有 Markdown 操作共用的方言配置。
///
/// 渲染、图片引用扫描和路径改写必须使用同一组选项，避免同一份正文被不同
/// 子系统解释成不同的事件流。
pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
}

/// 按 CloudStack 的统一 Markdown 方言返回带原始 UTF-8 字节范围的事件流。
///
/// pulldown-cmark 的数学语法基本符合需求，但会把 `$x$2` 识别为公式。Obsidian 与
/// Pandoc 风格的行内公式要求结束 `$` 后不能紧跟数字；不满足时把整个事件降级为
/// 原始文本，同时保留完全相同的源码范围。所有 Markdown 消费者必须使用此入口，
/// 避免渲染、图片扫描和路径改写对同一正文产生不同解释。
pub fn events_with_offsets(source: &str) -> impl Iterator<Item = (Event<'_>, Range<usize>)> + '_ {
    Parser::new_ext(source, options())
        .into_offset_iter()
        .map(move |(event, span)| {
            if matches!(event, Event::InlineMath(_))
                && source
                    .as_bytes()
                    .get(span.end)
                    .is_some_and(u8::is_ascii_digit)
            {
                (Event::Text(CowStr::Borrowed(&source[span.clone()])), span)
            } else {
                (event, span)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enables_the_supported_markdown_dialect() {
        let options = options();
        for expected in [
            Options::ENABLE_TABLES,
            Options::ENABLE_STRIKETHROUGH,
            Options::ENABLE_TASKLISTS,
            Options::ENABLE_FOOTNOTES,
            Options::ENABLE_MATH,
        ] {
            assert!(options.contains(expected));
        }
    }

    #[test]
    fn rejects_inline_math_whose_closer_is_followed_by_a_digit() {
        let source = "原价 $5，现在只需$2；公式 $x$ 2；转义 \\$5";
        let events = events_with_offsets(source).collect::<Vec<_>>();
        let math = events
            .iter()
            .filter_map(|(event, span)| match event {
                Event::InlineMath(tex) => Some((tex.as_ref(), span.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(math.len(), 1);
        assert_eq!(math[0].0, "x");
        assert_eq!(&source[math[0].1.clone()], "$x$");
        assert!(events.iter().any(|(event, span)| {
            matches!(event, Event::Text(text) if text.as_ref() == "$5，现在只需$")
                && &source[span.clone()] == "$5，现在只需$"
        }));
    }
}
