use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use cloudstack_application::preview::{PreviewAction, PreviewCoordinator, PreviewRequest};
use cloudstack_core::model::ProjectContext;
use cloudstack_core::services::assets;
use cloudstack_renderer::{MarkdownRenderer, MathIssue, RenderedDocument};
use gtk::{gio, glib};
use webkit::prelude::*;

use crate::i18n::{self, UiMessage};
use crate::tasks;

const SCRIPT_WORLD: &str = "cloudstack-preview";
const SCRIPT_HANDLER: &str = "previewScroll";
const PREVIEW_BASE_URI: &str = "cloudstack://preview/current/";
const UPDATE_FUNCTION: &str = r#"
if (globalThis.__cloudstackPreview) {
  globalThis.__cloudstackPreview.update(html);
}
"#;
const SET_SCROLL_FUNCTION: &str = r#"
if (globalThis.__cloudstackPreview) {
  globalThis.__cloudstackPreview.setScrollRatio(ratio);
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

  globalThis.__cloudstackPreview = Object.freeze({
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
<html lang="__CLOUDSTACK_LOCALE__">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src cloudstack:; style-src cloudstack: 'unsafe-inline'; font-src cloudstack:; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'">
<link rel="stylesheet" href="cloudstack://preview/app/katex.min.css">
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
<body><main id="preview-root"><p class="preview-placeholder">__CLOUDSTACK_PREVIEW_PLACEHOLDER__</p></main></body>
</html>"#;

fn preview_shell() -> String {
    let locale = i18n::current_locale().to_string();
    let placeholder = i18n::text(UiMessage::PreviewSelectArticlePlaceholder);
    PREVIEW_SHELL
        .replace("__CLOUDSTACK_LOCALE__", &locale)
        .replace("__CLOUDSTACK_PREVIEW_PLACEHOLDER__", &placeholder)
}

fn preview_placeholder_html() -> String {
    format!(
        r#"<p class="preview-placeholder">{}</p>"#,
        i18n::text(UiMessage::PreviewSelectArticlePlaceholder)
    )
}

#[derive(Clone)]
struct ResourceTarget {
    context: ProjectContext,
    post_id: String,
}

struct Inner {
    webview: webkit::WebView,
    renderer: Arc<MarkdownRenderer>,
    timeout: RefCell<Option<glib::SourceId>>,
    coordinator: RefCell<PreviewCoordinator>,
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
            manager.register_uri_scheme_as_local("cloudstack");
            manager.register_uri_scheme_as_secure("cloudstack");
        }
        register_resource_scheme(&context, Rc::clone(&resources));

        let content_manager = webkit::UserContentManager::new();
        let script = webkit::UserScript::for_world(
            PREVIEW_SCRIPT,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::Start,
            SCRIPT_WORLD,
            &["cloudstack://preview/*"],
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
            .width_request(360)
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
            .tooltip_text(i18n::text(UiMessage::PreviewDiagnosticTooltip))
            .visible(false)
            .css_classes(["flat"])
            .build();
        let inner = Rc::new(Inner {
            webview,
            renderer: Arc::new(MarkdownRenderer::default()),
            timeout: RefCell::new(None),
            coordinator: RefCell::new(PreviewCoordinator::default()),
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
        let shell = preview_shell();
        inner.webview.load_html(&shell, Some(PREVIEW_BASE_URI));

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
        *self.inner.resources.borrow_mut() = Some(ResourceTarget { context, post_id });
        Inner::cancel_timeout(&self.inner);
        let action = self
            .inner
            .coordinator
            .borrow_mut()
            .set_document(epoch, source);
        Inner::dispatch_action(&self.inner, action);
    }

    pub fn clear(&self, epoch: u64) {
        Inner::cancel_timeout(&self.inner);
        self.inner.coordinator.borrow_mut().clear(epoch);
        self.inner.resources.borrow_mut().take();
        self.inner.set_issues(Vec::new());
        self.inner.update_html(preview_placeholder_html());
    }

    pub fn schedule(&self, source: String, immediate: bool) {
        Inner::cancel_timeout(&self.inner);
        let action = self
            .inner
            .coordinator
            .borrow_mut()
            .schedule(source, immediate);
        Inner::dispatch_action(&self.inner, action);
    }
}

impl Inner {
    /// GTK 侧真正的定时器只在这里取消——这是平台副作用，coordinator 自己不
    /// 持有 `glib::SourceId`，所以每次要把新 action 交给它处理之前，调用方
    /// 都要先在这里清掉上一个还没触发的 debounce timer。
    fn cancel_timeout(this: &Rc<Self>) {
        if let Some(timeout) = this.timeout.borrow_mut().take() {
            timeout.remove();
        }
    }

    fn dispatch_action(this: &Rc<Self>, action: PreviewAction) {
        match action {
            PreviewAction::None => {}
            PreviewAction::Start(request) => Self::start(this, request),
            PreviewAction::Debounce { request, delay } => {
                let weak = Rc::downgrade(this);
                let source_id = glib::timeout_add_local_once(delay, move || {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    inner.timeout.borrow_mut().take();
                    let next = inner.coordinator.borrow_mut().debounce_elapsed(request);
                    if let Some(next) = next {
                        Self::start(&inner, next);
                    }
                });
                *this.timeout.borrow_mut() = Some(source_id);
            }
        }
    }

    fn start(this: &Rc<Self>, request: PreviewRequest) {
        let renderer = Arc::clone(&this.renderer);
        let source = request.source.clone();
        let weak = Rc::downgrade(this);
        glib::spawn_future_local(async move {
            let rendered = gio::spawn_blocking(move || renderer.render(&source)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            match rendered {
                Ok(document) => inner.finish_request(request, true, Some(document)),
                Err(_) => {
                    inner.toast(&i18n::text(UiMessage::PreviewRenderFailedToast));
                    inner.finish_request(request, false, None);
                }
            }
        });
    }

    /// 无论渲染成功还是失败都要走这里：让 coordinator 判断这次结果是否还
    /// 有资格应用（票据是否还是当前 epoch/generation），并按它的指示启动
    /// 排队里最新的下一个请求。
    fn finish_request(
        self: &Rc<Self>,
        request: PreviewRequest,
        success: bool,
        document: Option<RenderedDocument>,
    ) {
        let completion = self
            .coordinator
            .borrow_mut()
            .complete_render(request, success);
        if completion.apply_result {
            if let Some(document) = document {
                self.update_html(document.body_html);
                self.set_issues(document.issues);
            }
        }
        if let Some(next) = completion.next_request {
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
                    overlay.add_toast(adw::Toast::new(&i18n::text(
                        UiMessage::PreviewUpdateFailedToast,
                    )));
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
            .set_label(&i18n::text(UiMessage::PreviewMathIssues { count }));
    }

    fn toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }
}

fn register_resource_scheme(
    context: &webkit::WebContext,
    resources: Rc<RefCell<Option<ResourceTarget>>>,
) {
    context.register_uri_scheme("cloudstack", move |request| {
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
            match cloudstack_renderer::static_asset(asset_path) {
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
        let request = request.clone();
        tasks::run(
            move || assets::read_image_asset(&target.context, &target.post_id, &markdown_path),
            move |result| match result {
                // content_type 来自读取时对实际字节内容的嗅探，不是从 URL 后缀猜的——
                // 文件名后缀和真实内容不一致时也能返回正确的 Content-Type。
                Ok(image) => finish_bytes(&request, image.bytes, image.content_type),
                Err(error) => finish_error(
                    &request,
                    gio::IOErrorEnum::NotFound,
                    &format!("读取图片失败：{error}"),
                ),
            },
        );
    });
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
            inner.toast(&i18n::text(UiMessage::PreviewDiagnosticsExpiredToast));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigationDisposition {
    Allow,
    External,
    Block,
}

fn classify_navigation(uri: &str, user_gesture: bool) -> NavigationDisposition {
    if (!user_gesture && (uri == "about:blank" || uri.starts_with("cloudstack://preview/current/")))
        || uri.starts_with("cloudstack://preview/current/#")
    {
        NavigationDisposition::Allow
    } else if user_gesture
        && (uri.starts_with("https://") || uri.starts_with("http://") || uri.starts_with("mailto:"))
    {
        NavigationDisposition::External
    } else {
        NavigationDisposition::Block
    }
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
            match classify_navigation(uri, action.is_user_gesture()) {
                NavigationDisposition::Allow => false,
                NavigationDisposition::External => {
                    decision.ignore();
                    if let Err(error) =
                        gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>)
                    {
                        if let Some(inner) = weak.upgrade() {
                            log::warn!("打开外部链接失败：{error}");
                            inner.toast(&i18n::text(UiMessage::PreviewExternalLinkFailedToast));
                        }
                    }
                    true
                }
                NavigationDisposition::Block => {
                    decision.ignore();
                    if let Some(inner) = weak.upgrade() {
                        inner.toast(&i18n::text(UiMessage::PreviewBlockedNavigationToast));
                    }
                    true
                }
            }
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
        inner.toast(&i18n::text(UiMessage::PreviewLoadFailedToast));
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shell_localizes_placeholder_and_language_tag() {
        let shell = preview_shell();
        assert!(!shell.contains("__CLOUDSTACK_LOCALE__"));
        assert!(!shell.contains("__CLOUDSTACK_PREVIEW_PLACEHOLDER__"));
        assert!(shell.contains("preview-root"));
    }

    #[test]
    fn navigation_rejects_legacy_and_dangerous_schemes() {
        assert_eq!(
            classify_navigation("cloudstack://preview/current/#part", true),
            NavigationDisposition::Allow
        );
        for uri in [
            "blog-editor://preview/current/#part",
            "javascript:alert(1)",
            "data:text/html,unsafe",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                classify_navigation(uri, true),
                NavigationDisposition::Block,
                "{uri} must be blocked"
            );
        }
    }
}
