use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use blog_editor_core::model::ProjectContext;
use blog_editor_core::services::assets;
use blog_editor_renderer::{MarkdownRenderer, MathIssue, RenderedDocument};
use gtk::{gio, glib};
use webkit::prelude::*;

use crate::tasks;

const SCRIPT_WORLD: &str = "blog-editor-preview";
const SCRIPT_HANDLER: &str = "previewScroll";
const PREVIEW_BASE_URI: &str = "blog-editor://preview/current/";
const UPDATE_FUNCTION: &str = r#"
if (globalThis.__blogEditorPreview) {
  globalThis.__blogEditorPreview.update(html);
}
"#;
const SET_SCROLL_FUNCTION: &str = r#"
if (globalThis.__blogEditorPreview) {
  globalThis.__blogEditorPreview.setScrollRatio(ratio);
}
"#;

const PREVIEW_SCRIPT: &str = r#"
(() => {
  let suppressScrollMessage = false;
  let scrollFrame = 0;

  const ratio = () => {
    const range = Math.max(0, document.documentElement.scrollHeight - window.innerHeight);
    return range > 0 ? Math.min(1, Math.max(0, window.scrollY / range)) : 0;
  };

  globalThis.__blogEditorPreview = Object.freeze({
    update(html) {
      const previous = ratio();
      const root = document.getElementById('preview-root');
      if (!root) return;
      root.innerHTML = html;
      suppressScrollMessage = true;
      requestAnimationFrame(() => {
        const range = Math.max(0, document.documentElement.scrollHeight - window.innerHeight);
        window.scrollTo(0, previous * range);
        requestAnimationFrame(() => { suppressScrollMessage = false; });
      });
    },
    setScrollRatio(value) {
      const normalized = Math.min(1, Math.max(0, Number(value) || 0));
      const range = Math.max(0, document.documentElement.scrollHeight - window.innerHeight);
      suppressScrollMessage = true;
      window.scrollTo(0, normalized * range);
      requestAnimationFrame(() => { suppressScrollMessage = false; });
    }
  });

  addEventListener('scroll', () => {
    if (suppressScrollMessage || scrollFrame) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = 0;
      if (!suppressScrollMessage && window.webkit?.messageHandlers?.previewScroll) {
        window.webkit.messageHandlers.previewScroll.postMessage(ratio());
      }
    });
  }, { passive: true });
})();
"#;

const PREVIEW_SHELL: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src blog-editor:; style-src blog-editor: 'unsafe-inline'; font-src blog-editor:; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'">
<link rel="stylesheet" href="blog-editor://preview/app/katex.min.css">
<style>
:root { color-scheme: light dark; --bg:#fff; --fg:#202124; --muted:#667085; --border:#d8dee8; --code:#f3f5f7; --accent:#3584e4; --error:#c01c28; }
@media (prefers-color-scheme: dark) { :root { --bg:#1d1d20; --fg:#f3f3f3; --muted:#a9b0bb; --border:#41444b; --code:#292b30; --accent:#78aeed; --error:#ff7b86; } }
* { box-sizing:border-box; }
html { background:var(--bg); color:var(--fg); font:16px/1.72 system-ui,-apple-system,"Noto Sans CJK SC",sans-serif; }
body { margin:0; min-height:100vh; }
#preview-root { width:min(100%, 840px); margin:0 auto; padding:32px 38px 80px; overflow-wrap:anywhere; }
h1,h2,h3,h4,h5,h6 { line-height:1.28; margin:1.5em 0 .65em; }
h1 { font-size:2em; } h2 { font-size:1.55em; border-bottom:1px solid var(--border); padding-bottom:.28em; }
a { color:var(--accent); }
img { display:block; max-width:100%; height:auto; margin:1.2em auto; border-radius:8px; }
blockquote { margin:1em 0; padding:.15em 1em; color:var(--muted); border-left:4px solid var(--border); }
pre,code { font-family:"Adwaita Mono","Noto Sans Mono",monospace; background:var(--code); }
code { padding:.12em .32em; border-radius:4px; }
pre { overflow:auto; padding:1em; border-radius:8px; } pre code { padding:0; }
table { border-collapse:collapse; width:100%; } th,td { border:1px solid var(--border); padding:.45em .7em; } th { background:var(--code); }
.task-list-item { list-style:none; }
.math-error { color:var(--error); border-bottom:1px wavy currentColor; }
.math-error-display { display:block; padding:.6em; text-align:center; }
.preview-placeholder { color:var(--muted); text-align:center; margin-top:24vh; }
</style>
</head>
<body><main id="preview-root"><p class="preview-placeholder">选择文章后显示实时预览</p></main></body>
</html>"#;

#[derive(Clone)]
struct ResourceTarget {
    context: ProjectContext,
    post_id: String,
}

#[derive(Clone)]
struct RenderRequest {
    epoch: u64,
    generation: u64,
    source: String,
}

#[derive(Default)]
struct QueueState {
    active: bool,
    pending: Option<RenderRequest>,
}

impl QueueState {
    fn enqueue(&mut self, request: RenderRequest) -> Option<RenderRequest> {
        self.pending = Some(request);
        if self.active {
            None
        } else {
            self.active = true;
            self.pending.take()
        }
    }

    fn finish(&mut self) -> Option<RenderRequest> {
        if let Some(next) = self.pending.take() {
            Some(next)
        } else {
            self.active = false;
            None
        }
    }
}

struct Inner {
    webview: webkit::WebView,
    renderer: Arc<MarkdownRenderer>,
    timeout: RefCell<Option<glib::SourceId>>,
    generation: Cell<u64>,
    epoch: Cell<u64>,
    queue: RefCell<QueueState>,
    last_applied: RefCell<Option<(u64, String)>>,
    shell_ready: Cell<bool>,
    pending_html: RefCell<Option<String>>,
    resources: Rc<RefCell<Option<ResourceTarget>>>,
    issues: RefCell<Vec<MathIssue>>,
    diagnostic_button: gtk::Button,
    buffer: sourceview::Buffer,
    editor: sourceview::View,
    editor_adjustment: gtk::Adjustment,
    suppress_editor_scroll: Cell<bool>,
    toast_overlay: adw::ToastOverlay,
}

#[derive(Clone)]
pub struct Preview {
    inner: Rc<Inner>,
}

impl Preview {
    pub fn new(
        buffer: &sourceview::Buffer,
        editor: &sourceview::View,
        editor_scroll: &gtk::ScrolledWindow,
        toast_overlay: &adw::ToastOverlay,
    ) -> Self {
        let resources = Rc::new(RefCell::new(None));
        let context = webkit::WebContext::new();
        if let Some(manager) = context.security_manager() {
            manager.register_uri_scheme_as_local("blog-editor");
            manager.register_uri_scheme_as_secure("blog-editor");
        }
        register_resource_scheme(&context, Rc::clone(&resources));

        let content_manager = webkit::UserContentManager::new();
        let script = webkit::UserScript::for_world(
            PREVIEW_SCRIPT,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::Start,
            SCRIPT_WORLD,
            &["blog-editor://preview/*"],
            &[],
        );
        content_manager.add_script(&script);
        let registered =
            content_manager.register_script_message_handler(SCRIPT_HANDLER, Some(SCRIPT_WORLD));
        if !registered {
            log::warn!("无法注册预览滚动消息处理器");
        }

        let webview = webkit::WebView::builder()
            .web_context(&context)
            .user_content_manager(&content_manager)
            .hexpand(true)
            .vexpand(true)
            .build();
        if let Some(settings) = webkit::prelude::WebViewExt::settings(&webview) {
            settings.set_enable_javascript(true);
            settings.set_javascript_can_open_windows_automatically(false);
            settings.set_allow_file_access_from_file_urls(false);
            settings.set_allow_universal_access_from_file_urls(false);
            settings.set_enable_developer_extras(false);
            settings.set_enable_media(false);
            settings.set_enable_media_stream(false);
            settings.set_enable_webgl(false);
            settings.set_auto_load_images(true);
        }
        webview.set_background_color(&gtk::gdk::RGBA::TRANSPARENT);

        let diagnostic_button = gtk::Button::builder()
            .icon_name("dialog-warning-symbolic")
            .tooltip_text("跳到第一条公式错误")
            .visible(false)
            .css_classes(["flat"])
            .build();
        let inner = Rc::new(Inner {
            webview,
            renderer: Arc::new(MarkdownRenderer::default()),
            timeout: RefCell::new(None),
            generation: Cell::new(0),
            epoch: Cell::new(0),
            queue: RefCell::new(QueueState::default()),
            last_applied: RefCell::new(None),
            shell_ready: Cell::new(false),
            pending_html: RefCell::new(None),
            resources,
            issues: RefCell::new(Vec::new()),
            diagnostic_button,
            buffer: buffer.clone(),
            editor: editor.clone(),
            editor_adjustment: editor_scroll.vadjustment(),
            suppress_editor_scroll: Cell::new(false),
            toast_overlay: toast_overlay.clone(),
        });

        connect_scroll_message(&content_manager, &inner);
        connect_editor_scroll(&inner);
        connect_diagnostics(&inner);
        connect_navigation(&inner);
        connect_load_lifecycle(&inner);
        inner
            .webview
            .load_html(PREVIEW_SHELL, Some(PREVIEW_BASE_URI));

        Self { inner }
    }

    pub fn widget(&self) -> &webkit::WebView {
        &self.inner.webview
    }

    pub fn diagnostic_button(&self) -> &gtk::Button {
        &self.inner.diagnostic_button
    }

    pub fn set_document(
        &self,
        context: ProjectContext,
        post_id: String,
        epoch: u64,
        source: String,
    ) {
        self.inner.epoch.set(epoch);
        *self.inner.resources.borrow_mut() = Some(ResourceTarget { context, post_id });
        self.inner.last_applied.borrow_mut().take();
        self.schedule(source, true);
    }

    pub fn clear(&self, epoch: u64) {
        self.inner.epoch.set(epoch);
        self.inner
            .generation
            .set(self.inner.generation.get().wrapping_add(1));
        if let Some(timeout) = self.inner.timeout.borrow_mut().take() {
            timeout.remove();
        }
        self.inner.queue.borrow_mut().pending = None;
        self.inner.resources.borrow_mut().take();
        self.inner.last_applied.borrow_mut().take();
        self.inner.set_issues(Vec::new());
        self.inner
            .update_html("<p class=\"preview-placeholder\">选择文章后显示实时预览</p>".to_string());
    }

    pub fn schedule(&self, source: String, immediate: bool) {
        if self
            .inner
            .last_applied
            .borrow()
            .as_ref()
            .is_some_and(|(epoch, applied)| *epoch == self.inner.epoch.get() && applied == &source)
        {
            return;
        }
        if let Some(timeout) = self.inner.timeout.borrow_mut().take() {
            timeout.remove();
        }
        let generation = self.inner.generation.get().wrapping_add(1);
        self.inner.generation.set(generation);
        let request = RenderRequest {
            epoch: self.inner.epoch.get(),
            generation,
            source,
        };
        if immediate {
            Inner::enqueue(&self.inner, request);
            return;
        }

        let delay = debounce_duration(request.source.len());
        let weak = Rc::downgrade(&self.inner);
        let source = glib::timeout_add_local_once(delay, move || {
            if let Some(inner) = weak.upgrade() {
                inner.timeout.borrow_mut().take();
                Inner::enqueue(&inner, request);
            }
        });
        *self.inner.timeout.borrow_mut() = Some(source);
    }
}

impl Inner {
    fn enqueue(this: &Rc<Self>, request: RenderRequest) {
        let next = this.queue.borrow_mut().enqueue(request);
        if let Some(next) = next {
            Self::start(this, next);
        }
    }

    fn start(this: &Rc<Self>, request: RenderRequest) {
        let renderer = Arc::clone(&this.renderer);
        let source = request.source.clone();
        let weak = Rc::downgrade(this);
        glib::spawn_future_local(async move {
            let rendered = gio::spawn_blocking(move || renderer.render(&source)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            match rendered {
                Ok(document) => inner.finish_request(request, document),
                Err(_) => {
                    inner.toast("Markdown 后台渲染异常终止");
                    inner.finish_without_result();
                }
            }
        });
    }

    fn finish_request(self: &Rc<Self>, request: RenderRequest, document: RenderedDocument) {
        if should_apply(&request, self.epoch.get(), self.generation.get()) {
            self.update_html(document.body_html);
            self.set_issues(document.issues);
            *self.last_applied.borrow_mut() = Some((request.epoch, request.source));
        }
        self.finish_without_result();
    }

    fn finish_without_result(self: &Rc<Self>) {
        let next = self.queue.borrow_mut().finish();
        if let Some(next) = next {
            Self::start(self, next);
        }
    }

    fn update_html(&self, html: String) {
        if !self.shell_ready.get() {
            *self.pending_html.borrow_mut() = Some(html);
            return;
        }
        let arguments = glib::VariantDict::new(None);
        arguments.insert("html", html);
        let arguments = arguments.end();
        let overlay = self.toast_overlay.clone();
        self.webview.call_async_javascript_function(
            UPDATE_FUNCTION,
            Some(&arguments),
            Some(SCRIPT_WORLD),
            Some(PREVIEW_BASE_URI),
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(error) = result {
                    log::warn!("更新实时预览失败：{error}");
                    overlay.add_toast(adw::Toast::new("实时预览更新失败"));
                }
            },
        );
    }

    fn set_preview_scroll(&self, ratio: f64) {
        let arguments = glib::VariantDict::new(None);
        arguments.insert("ratio", ratio.clamp(0.0, 1.0));
        let arguments = arguments.end();
        self.webview.call_async_javascript_function(
            SET_SCROLL_FUNCTION,
            Some(&arguments),
            Some(SCRIPT_WORLD),
            Some(PREVIEW_BASE_URI),
            None::<&gio::Cancellable>,
            |_| {},
        );
    }

    fn set_issues(&self, issues: Vec<MathIssue>) {
        let count = issues.len();
        *self.issues.borrow_mut() = issues;
        self.diagnostic_button.set_visible(count > 0);
        self.diagnostic_button
            .set_label(&format!("公式错误 {count}"));
    }

    fn toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }
}

fn should_apply(request: &RenderRequest, epoch: u64, generation: u64) -> bool {
    request.epoch == epoch && request.generation == generation
}

fn debounce_duration(bytes: usize) -> Duration {
    if bytes > 500 * 1024 {
        Duration::from_millis(500)
    } else if bytes > 100 * 1024 {
        Duration::from_millis(350)
    } else {
        Duration::from_millis(200)
    }
}

fn register_resource_scheme(
    context: &webkit::WebContext,
    resources: Rc<RefCell<Option<ResourceTarget>>>,
) {
    context.register_uri_scheme("blog-editor", move |request| {
        if request.http_method().as_deref() != Some("GET") {
            finish_error(
                request,
                gio::IOErrorEnum::NotSupported,
                "预览资源只支持 GET",
            );
            return;
        }
        let Some(path) = request.path() else {
            finish_error(
                request,
                gio::IOErrorEnum::InvalidArgument,
                "预览资源路径无效",
            );
            return;
        };
        if let Some(asset_path) = path.strip_prefix("/app/") {
            match blog_editor_renderer::static_asset(asset_path) {
                Some(asset) => finish_bytes(request, asset.bytes.into_owned(), asset.content_type),
                None => finish_error(request, gio::IOErrorEnum::NotFound, "找不到内嵌预览资源"),
            }
            return;
        }
        let Some(markdown_path) = path.strip_prefix("/current/") else {
            finish_error(
                request,
                gio::IOErrorEnum::PermissionDenied,
                "预览资源路径不受信任",
            );
            return;
        };
        let Some(target) = resources.borrow().clone() else {
            finish_error(request, gio::IOErrorEnum::NotFound, "当前没有打开文章");
            return;
        };
        let markdown_path = markdown_path.to_string();
        let content_type = image_content_type(&markdown_path);
        let request = request.clone();
        tasks::run(
            move || assets::read_image_asset(&target.context, &target.post_id, &markdown_path),
            move |result| match result {
                Ok(bytes) => finish_bytes(&request, bytes, content_type),
                Err(error) => finish_error(
                    &request,
                    gio::IOErrorEnum::NotFound,
                    &format!("读取图片失败：{error}"),
                ),
            },
        );
    });
}

fn image_content_type(path: &str) -> &'static str {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn finish_bytes(request: &webkit::URISchemeRequest, bytes: Vec<u8>, content_type: &str) {
    let length = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let bytes = glib::Bytes::from_owned(bytes);
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    request.finish(&stream, length, Some(content_type));
}

fn finish_error(request: &webkit::URISchemeRequest, kind: gio::IOErrorEnum, message: &str) {
    let mut error = glib::Error::new(kind, message);
    request.finish_error(&mut error);
}

fn connect_scroll_message(manager: &webkit::UserContentManager, inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    manager.connect_script_message_received(Some(SCRIPT_HANDLER), move |_, value| {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if !value.is_number() {
            return;
        }
        let ratio = value.to_double().clamp(0.0, 1.0);
        let adjustment = &inner.editor_adjustment;
        let range = adjustment.upper() - adjustment.page_size();
        if range <= 0.0 {
            return;
        }
        let target = ratio * range;
        if (adjustment.value() - target).abs() < 1.0 {
            return;
        }
        inner.suppress_editor_scroll.set(true);
        adjustment.set_value(target);
        inner.suppress_editor_scroll.set(false);
    });
}

fn connect_editor_scroll(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    inner
        .editor_adjustment
        .connect_value_changed(move |adjustment| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.suppress_editor_scroll.get() {
                return;
            }
            let range = adjustment.upper() - adjustment.page_size();
            let ratio = if range > 0.0 {
                (adjustment.value() / range).clamp(0.0, 1.0)
            } else {
                0.0
            };
            inner.set_preview_scroll(ratio);
        });
}

fn connect_diagnostics(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    inner.diagnostic_button.connect_clicked(move |_| {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let Some(issue) = inner.issues.borrow().first().cloned() else {
            return;
        };
        let source = inner
            .buffer
            .text(&inner.buffer.start_iter(), &inner.buffer.end_iter(), true)
            .to_string();
        if issue.span.end > source.len()
            || !source.is_char_boundary(issue.span.start)
            || !source.is_char_boundary(issue.span.end)
        {
            inner.toast("公式错误位置已经过期，请等待预览刷新");
            return;
        }
        let start_offset =
            i32::try_from(source[..issue.span.start].chars().count()).unwrap_or(i32::MAX);
        let end_offset =
            i32::try_from(source[..issue.span.end].chars().count()).unwrap_or(i32::MAX);
        let start = inner.buffer.iter_at_offset(start_offset);
        let end = inner.buffer.iter_at_offset(end_offset);
        inner.buffer.select_range(&start, &end);
        let mut start = start;
        inner
            .editor
            .scroll_to_iter(&mut start, 0.15, false, 0.0, 0.2);
        inner.editor.grab_focus();
    });
}

fn connect_navigation(inner: &Rc<Inner>) {
    let weak: Weak<Inner> = Rc::downgrade(inner);
    inner
        .webview
        .connect_decide_policy(move |_, decision, decision_type| {
            if !matches!(
                decision_type,
                webkit::PolicyDecisionType::NavigationAction
                    | webkit::PolicyDecisionType::NewWindowAction
            ) {
                return false;
            }
            let Ok(navigation) = decision
                .clone()
                .downcast::<webkit::NavigationPolicyDecision>()
            else {
                return false;
            };
            let Some(action) = navigation.navigation_action() else {
                return false;
            };
            let Some(uri) = action.request().and_then(|request| request.uri()) else {
                decision.ignore();
                return true;
            };
            let uri = uri.as_str();
            if !action.is_user_gesture()
                && (uri == "about:blank" || uri.starts_with("blog-editor://preview/current/"))
            {
                return false;
            }
            if uri.starts_with("blog-editor://preview/current/#") {
                return false;
            }

            decision.ignore();
            if action.is_user_gesture()
                && (uri.starts_with("https://")
                    || uri.starts_with("http://")
                    || uri.starts_with("mailto:"))
            {
                if let Err(error) =
                    gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>)
                {
                    if let Some(inner) = weak.upgrade() {
                        log::warn!("打开外部链接失败：{error}");
                        inner.toast("无法使用系统应用打开该链接");
                    }
                }
            } else if let Some(inner) = weak.upgrade() {
                inner.toast("预览已阻止不受支持的链接");
            }
            true
        });
}

fn connect_load_lifecycle(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    inner.webview.connect_load_changed(move |_, event| {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        match event {
            webkit::LoadEvent::Started => inner.shell_ready.set(false),
            webkit::LoadEvent::Finished => {
                inner.shell_ready.set(true);
                if let Some(html) = inner.pending_html.borrow_mut().take() {
                    inner.update_html(html);
                }
            }
            _ => {}
        }
    });

    let weak = Rc::downgrade(inner);
    inner.webview.connect_load_failed(move |_, _, uri, error| {
        let Some(inner) = weak.upgrade() else {
            return false;
        };
        inner.shell_ready.set(false);
        log::warn!("实时预览页面加载失败：uri={uri} error={error}");
        inner.toast("实时预览页面加载失败，编辑和保存仍可继续");
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(generation: u64) -> RenderRequest {
        RenderRequest {
            epoch: 1,
            generation,
            source: generation.to_string(),
        }
    }

    #[test]
    fn pending_render_keeps_only_the_latest_request() {
        let mut queue = QueueState::default();
        assert_eq!(queue.enqueue(request(1)).unwrap().generation, 1);
        assert!(queue.enqueue(request(2)).is_none());
        assert!(queue.enqueue(request(3)).is_none());
        assert_eq!(queue.finish().unwrap().generation, 3);
        assert!(queue.finish().is_none());
        assert!(!queue.active);
    }

    #[test]
    fn stale_generation_or_document_epoch_is_never_applied() {
        let current = request(8);
        assert!(should_apply(&current, 1, 8));
        assert!(!should_apply(&current, 2, 8));
        assert!(!should_apply(&current, 1, 9));
    }

    #[test]
    fn debounce_scales_with_document_size() {
        assert_eq!(debounce_duration(10), Duration::from_millis(200));
        assert_eq!(debounce_duration(101 * 1024), Duration::from_millis(350));
        assert_eq!(debounce_duration(501 * 1024), Duration::from_millis(500));
    }

    #[test]
    fn image_mime_type_is_determined_without_query_or_fragment() {
        assert_eq!(image_content_type("Photo/a.png?x=1"), "image/png");
        assert_eq!(image_content_type("Photo/a.SVG#icon"), "image/svg+xml");
    }
}
