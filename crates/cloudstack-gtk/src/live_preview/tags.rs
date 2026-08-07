//! CloudStack-owned semantic `TextTag` registry for Live Preview styling.
//! Theme-neutral V1 styling per `docs/LIVE_PREVIEW_SPIKES.md` §4/§9:
//! priorities are computed above whatever GtkSourceView's own syntax
//! highlighting has already installed into the buffer's tag table, tag
//! names are stable `cloudstack-live-*` identifiers, and this registry only
//! ever removes the tags it owns — never `remove_all_tags`, which would
//! also strip GtkSourceView's Markdown syntax highlighting and any active
//! `SearchContext` match styling sharing the same tag table.

use gtk::prelude::*;

use super::analysis::StyleKind;

/// Six heading scales, largest first (`Heading(1)` is the biggest), plus
/// one tag per remaining `StyleKind`. Deliberately no fixed link foreground
/// color and no fixed link/code colors beyond a neutral, low-alpha
/// background — V1 is about proving semantic rendering works at all, not
/// designing the final theme; a fixed blue link color would immediately
/// raise light/dark/theme-token questions this phase doesn't need to
/// answer yet.
pub struct LivePreviewTags {
    heading: [gtk::TextTag; 6],
    strong: gtk::TextTag,
    emphasis: gtk::TextTag,
    strikethrough: gtk::TextTag,
    inline_code: gtk::TextTag,
    code_block: gtk::TextTag,
    block_quote: gtk::TextTag,
    link: gtk::TextTag,
}

const HEADING_SCALES: [f64; 6] = [1.65, 1.45, 1.30, 1.18, 1.10, 1.05];

/// Neutral, low-alpha gray — visible against both light and dark schemes
/// without committing to either. The editor view already sets
/// `.monospace(true)` globally, so `InlineCode`/`CodeBlock` need this
/// background to read as distinct at all; a `family` override alone would
/// be invisible.
const CODE_BACKGROUND: &str = "#80808026";

impl LivePreviewTags {
    /// Installs all CloudStack semantic tags into `buffer`'s tag table,
    /// with priorities starting immediately above whatever is already
    /// registered there. A naive 0-based priority would rank CloudStack's
    /// tags *below* GtkSourceView's own Markdown syntax tags (added via
    /// `set_language`) instead of above them — confirmed as a real failure
    /// mode in the Phase 11.2 spike, not a theoretical concern.
    pub fn install(buffer: &sourceview::Buffer) -> Self {
        let table = buffer.tag_table();
        let base_priority = table.size();

        let heading = std::array::from_fn(|index| {
            let tag = gtk::TextTag::new(Some(&format!("cloudstack-live-heading-{}", index + 1)));
            tag.set_scale(HEADING_SCALES[index]);
            tag.set_weight(700);
            tag.set_pixels_above_lines(4);
            tag.set_pixels_below_lines(4);
            table.add(&tag);
            tag
        });

        let strong = gtk::TextTag::new(Some("cloudstack-live-strong"));
        strong.set_weight(700);
        table.add(&strong);

        let emphasis = gtk::TextTag::new(Some("cloudstack-live-emphasis"));
        emphasis.set_style(gtk::pango::Style::Italic);
        table.add(&emphasis);

        let strikethrough = gtk::TextTag::new(Some("cloudstack-live-strikethrough"));
        strikethrough.set_strikethrough(true);
        table.add(&strikethrough);

        let inline_code = gtk::TextTag::new(Some("cloudstack-live-inline-code"));
        inline_code.set_background(Some(CODE_BACKGROUND));
        table.add(&inline_code);

        let code_block = gtk::TextTag::new(Some("cloudstack-live-code-block"));
        code_block.set_background(Some(CODE_BACKGROUND));
        table.add(&code_block);

        let block_quote = gtk::TextTag::new(Some("cloudstack-live-block-quote"));
        block_quote.set_style(gtk::pango::Style::Italic);
        table.add(&block_quote);

        let link = gtk::TextTag::new(Some("cloudstack-live-link"));
        link.set_underline(gtk::pango::Underline::Single);
        table.add(&link);

        let tags = Self {
            heading,
            strong,
            emphasis,
            strikethrough,
            inline_code,
            code_block,
            block_quote,
            link,
        };
        for (index, tag) in tags.all().into_iter().enumerate() {
            tag.set_priority(base_priority + i32::try_from(index).expect("small fixed list"));
        }
        tags
    }

    /// The single mapping point from analysis output to a concrete tag.
    /// Every `StyleKind` V1 knows about has one; an out-of-range heading
    /// level (which `analysis::atx_heading_level` never actually produces,
    /// but this function doesn't trust that from the outside) returns
    /// `None` rather than panicking — fail-visible: an unmappable span is
    /// simply not styled, never a crash.
    pub fn tag_for(&self, kind: StyleKind) -> Option<&gtk::TextTag> {
        match kind {
            StyleKind::Heading(level) => {
                let index = usize::from(level.checked_sub(1)?);
                self.heading.get(index)
            }
            StyleKind::Strong => Some(&self.strong),
            StyleKind::Emphasis => Some(&self.emphasis),
            StyleKind::Strikethrough => Some(&self.strikethrough),
            StyleKind::InlineCode => Some(&self.inline_code),
            StyleKind::CodeBlock => Some(&self.code_block),
            StyleKind::BlockQuote => Some(&self.block_quote),
            StyleKind::Link => Some(&self.link),
        }
    }

    fn all(&self) -> [&gtk::TextTag; 13] {
        [
            &self.heading[0],
            &self.heading[1],
            &self.heading[2],
            &self.heading[3],
            &self.heading[4],
            &self.heading[5],
            &self.strong,
            &self.emphasis,
            &self.strikethrough,
            &self.inline_code,
            &self.code_block,
            &self.block_quote,
            &self.link,
        ]
    }

    /// Removes only the tags this registry owns, over the whole buffer.
    /// Never `remove_all_tags` — see the module doc comment.
    pub fn clear(&self, buffer: &sourceview::Buffer) {
        let start = buffer.start_iter();
        let end = buffer.end_iter();
        for tag in self.all() {
            buffer.remove_tag(tag, &start, &end);
        }
    }
}
