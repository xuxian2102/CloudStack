use pulldown_cmark::Event;
use std::fs;

use super::*;

fn math_events(source: &str) -> Vec<(String, bool, Range<usize>)> {
    markdown::events_with_offsets(source)
        .filter_map(|(event, span)| match event {
            Event::InlineMath(tex) => Some((tex.into_string(), false, span)),
            Event::DisplayMath(tex) => Some((tex.into_string(), true, span)),
            _ => None,
        })
        .collect()
}

#[test]
fn renders_inline_and_display_math_as_html_and_mathml() {
    let rendered = MarkdownRenderer::default().render("行内 $E=mc^2$。\n\n$$\\frac{1}{2}$$\n");

    assert!(rendered.issues.is_empty(), "{:?}", rendered.issues);
    assert!(rendered.body_html.contains("katex"));
    assert!(rendered.body_html.contains("katex-display"));
    assert!(rendered.body_html.contains("<math"));
    assert!(!rendered.body_html.contains("$E=mc^2$"));
}

#[test]
fn invalid_math_is_non_fatal_and_reports_source_span() {
    let source = "before $\\notacommand{<img>}$ after";
    let rendered = MarkdownRenderer::default().render(source);

    assert_eq!(rendered.issues.len(), 1);
    let issue = &rendered.issues[0];
    assert_eq!(&source[issue.span.clone()], "$\\notacommand{<img>}$");
    assert_eq!(issue.source, "\\notacommand{<img>}");
    assert!(!issue.display);
    assert!(rendered.body_html.contains("math-error"));
    assert!(!rendered.body_html.contains("<img>"));
    assert!(rendered.body_html.contains("\\notacommand{&lt;img&gt;}"));
}

#[test]
fn escapes_all_user_supplied_raw_html() {
    let source = "<script>alert(1)</script>\n\n<img src=x onerror=alert(2)>\n\n<iframe srcdoc='<script>x</script>'></iframe>";
    let rendered = MarkdownRenderer::default().render(source);

    assert!(!rendered.body_html.contains("<script>"));
    assert!(!rendered.body_html.contains("<img "));
    assert!(!rendered.body_html.contains("<iframe "));
    assert!(rendered.body_html.contains("&lt;script&gt;"));
    assert!(rendered
        .body_html
        .contains("&lt;img src=x onerror=alert(2)&gt;"));
}

#[test]
fn unsafe_katex_commands_are_not_trusted() {
    let rendered = MarkdownRenderer::default().render(
        r"$\href{javascript:alert(1)}{click}$ and $\htmlClass{evil}{x}$ and $\text{<script>}$",
    );

    // KaTeX 把未受信命令安全地渲染成 unsupported-command 文本，而不是
    // ParseError；关键不变量是它们不能生成攻击者指定的属性。
    assert!(rendered.issues.is_empty());
    assert!(!rendered.body_html.contains("href=\"javascript:"));
    assert!(!rendered.body_html.contains("class=\"evil\""));
    assert!(!rendered.body_html.contains("<script"));
}

#[test]
fn renderer_reuses_its_context_across_documents() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MarkdownRenderer>();

    let renderer = MarkdownRenderer::default();
    for source in ["$x^2$", "$$\\sum_{i=1}^n i$$", "$中文+x$"] {
        let rendered = renderer.render(source);
        assert!(
            rendered.issues.is_empty(),
            "{source}: {:?}",
            rendered.issues
        );
        assert!(rendered.body_html.contains("katex"));
    }
}

#[test]
fn pulldown_native_dollar_matrix_is_stable() {
    let cases = [
        ("$E=mc^2$", vec![("E=mc^2", false)]),
        ("$$E=mc^2$$", vec![("E=mc^2", true)]),
        ("原价 $5，现在只需 $2", vec![]),
        ("原价 $5，现在只需$2", vec![]),
        ("原价 $5，现在只需$ 2", vec![("5，现在只需", false)]),
        (r"\$5 和 \$ 5", vec![]),
        ("未闭合 $x", vec![]),
        ("`$HOME`", vec![]),
        ("https://svelte.dev/docs/svelte/$state", vec![]),
        ("[链接](https://example.com/$state)", vec![]),
        ("![图片](Photo/$file.png)", vec![]),
        ("`$x$`", vec![]),
    ];

    for (source, expected) in cases {
        let actual = math_events(source)
            .into_iter()
            .map(|(tex, display, _)| (tex, display))
            .collect::<Vec<_>>();
        let expected = expected
            .into_iter()
            .map(|(tex, display)| (tex.to_owned(), display))
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "native math events changed for {source:?}"
        );
    }
}

#[test]
fn supports_representative_katex_syntax() {
    let source = r#"
$\frac{a}{b}$

$$\begin{matrix}a & b \\ c & d\end{matrix}$$

$$\begin{aligned}a&=b+c\\d&=e\end{aligned}$$

$\def\RR{\mathbb{R}}\RR$
"#;
    let rendered = MarkdownRenderer::default().render(source);
    assert!(rendered.issues.is_empty(), "{:?}", rendered.issues);
}

#[test]
fn bundles_only_static_katex_assets() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/katex");
    let css = fs::read_to_string(root.join("katex.min.css")).unwrap();
    assert!(css.contains("fonts/KaTeX_Main-Regular.woff2"));
    assert!(root.join("fonts/KaTeX_Main-Regular.woff2").is_file());
    assert!(root.join("fonts/KaTeX_Main-Regular.woff").is_file());
    assert!(root.join("fonts/KaTeX_Main-Regular.ttf").is_file());
    assert!(root.join("LICENSE").is_file());

    let forbidden = fs::read_dir(&root)
        .unwrap()
        .chain(fs::read_dir(root.join("fonts")).unwrap())
        .filter_map(Result::ok)
        .any(|entry| entry.path().extension().is_some_and(|value| value == "js"));
    assert!(!forbidden, "KaTeX 静态资源目录不得包含 JavaScript");
    let css = static_asset("katex.min.css").expect("embedded KaTeX CSS");
    assert_eq!(css.content_type, "text/css; charset=utf-8");
    assert!(std::str::from_utf8(css.bytes.as_ref())
        .unwrap()
        .contains("KaTeX_Main-Regular"));
    assert!(static_asset("fonts/KaTeX_Main-Regular.woff2").is_some());
    assert!(static_asset("LICENSE").is_none());
}

#[test]
fn neutralizes_executable_link_and_image_schemes() {
    let rendered = MarkdownRenderer::default().render(
        "[bad](javascript:alert(1)) [ok](https://example.com) ![bad](data:image/png;base64,AAAA)",
    );

    assert!(!rendered.body_html.contains("href=\"javascript:"));
    assert!(!rendered.body_html.contains("src=\"data:"));
    assert!(rendered.body_html.contains("href=\"#\""));
    assert!(rendered.body_html.contains("href=\"https://example.com\""));
}
