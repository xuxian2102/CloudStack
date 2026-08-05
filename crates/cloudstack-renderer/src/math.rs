use katex::{render_to_string, KatexContext, Settings, TrustSetting};

/// 隔离 `katex-rs` 的私有适配层，方便以后在不改变公共接口的情况下替换后端。
#[derive(Default)]
pub(super) struct MathRenderer {
    context: KatexContext,
}

impl MathRenderer {
    pub(super) fn render(&self, source: &str, display: bool) -> Result<String, String> {
        let settings = Settings::builder()
            .display_mode(display)
            .throw_on_error(true)
            .trust(TrustSetting::Bool(false))
            .build();
        render_to_string(&self.context, source, &settings).map_err(|error| error.to_string())
    }
}
