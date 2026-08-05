mod math;

use std::borrow::Cow;
use std::ops::Range;

use cloudstack_core::services::markdown;
use pulldown_cmark::{html, CowStr, Event, Tag};
use rust_embed::RustEmbed;

use math::MathRenderer;

#[derive(RustEmbed)]
#[folder = "assets/katex"]
struct KatexAssets;

/// WebView 可以安全读取的一项内嵌静态资源。
pub struct StaticAsset {
    pub bytes: Cow<'static, [u8]>,
    pub content_type: &'static str,
}

/// 读取内嵌的 KaTeX CSS 或字体。路径相对于 `assets/katex`。
///
/// 许可证和其他非展示文件仍会被打包用于合规，但不会通过预览协议公开。
pub fn static_asset(path: &str) -> Option<StaticAsset> {
    let path = path.trim_start_matches('/');
    let content_type = match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => return None,
    };
    KatexAssets::get(path).map(|asset| StaticAsset {
        bytes: asset.data,
        content_type,
    })
}

/// 一次 Markdown 渲染的静态 HTML 正文与非致命公式问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDocument {
    pub body_html: String,
    pub issues: Vec<MathIssue>,
}

/// 一条无法渲染的数学公式诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathIssue {
    /// 公式（包含 `$` 或 `$$` 定界符）在 Markdown 源码中的 UTF-8 字节范围。
    pub span: Range<usize>,
    /// pulldown-cmark 去掉定界符后提供的 TeX 源码。
    pub source: String,
    pub message: String,
    pub display: bool,
}

/// 可复用的纯 Rust Markdown 渲染器。
///
/// 内部 KaTeX 上下文会跨公式和跨文档复用；公共接口不暴露第三方类型。
#[derive(Default)]
pub struct MarkdownRenderer {
    math: MathRenderer,
}

impl MarkdownRenderer {
    /// 把 Markdown 渲染为不依赖 JavaScript 的 HTML + MathML。
    ///
    /// 用户写入的原始 HTML 会按普通文本转义。数学语法错误不会中断整篇
    /// 文档，而是生成可见占位并记录到 `issues`。
    pub fn render(&self, source: &str) -> RenderedDocument {
        let mut issues = Vec::new();
        let events = markdown::events_with_offsets(source).map(|(event, span)| match event {
            Event::InlineMath(tex) => self.render_math(tex, false, span, &mut issues),
            Event::DisplayMath(tex) => self.render_math(tex, true, span, &mut issues),
            // pulldown-cmark 默认原样透传源码 HTML。把它降级成 Text 后，
            // push_html 会负责正确转义；只有本模块生成的公式 HTML 可透传。
            Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) if !is_safe_destination(dest_url.as_ref(), false) => Event::Start(Tag::Link {
                link_type,
                dest_url: CowStr::Borrowed("#"),
                title,
                id,
            }),
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) if !is_safe_destination(dest_url.as_ref(), true) => Event::Start(Tag::Image {
                link_type,
                dest_url: CowStr::Borrowed(""),
                title,
                id,
            }),
            other => other,
        });

        let mut body_html = String::new();
        html::push_html(&mut body_html, events);
        RenderedDocument { body_html, issues }
    }

    fn render_math<'a>(
        &self,
        tex: CowStr<'a>,
        display: bool,
        span: Range<usize>,
        issues: &mut Vec<MathIssue>,
    ) -> Event<'a> {
        match self.math.render(tex.as_ref(), display) {
            Ok(rendered) => Event::InlineHtml(rendered.into()),
            Err(message) => {
                issues.push(MathIssue {
                    span,
                    source: tex.to_string(),
                    message: message.clone(),
                    display,
                });
                Event::InlineHtml(error_placeholder(tex.as_ref(), &message, display).into())
            }
        }
    }
}

fn is_safe_destination(destination: &str, image: bool) -> bool {
    let destination = destination.trim_matches(|character: char| {
        character.is_ascii_whitespace() || character.is_ascii_control()
    });
    if destination.is_empty() || destination.starts_with('#') {
        return true;
    }
    if destination.starts_with("//") {
        return false;
    }

    let scheme_end = destination.find(':').filter(|colon| {
        destination[..*colon]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
            && !destination[..*colon].contains(['/', '?', '#'])
    });
    let Some(colon) = scheme_end else {
        return true;
    };
    if image {
        return false;
    }
    matches!(
        destination[..colon].to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

fn error_placeholder(source: &str, message: &str, display: bool) -> String {
    let class = if display {
        "math-error math-error-display"
    } else {
        "math-error"
    };
    format!(
        "<span class=\"{class}\" title=\"{}\"><code>{}</code></span>",
        escape_html(message),
        escape_html(source)
    )
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests;
