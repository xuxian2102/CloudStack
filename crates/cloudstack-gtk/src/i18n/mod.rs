mod message;

use std::env;
use std::sync::OnceLock;

use fluent_templates::{static_loader, LanguageIdentifier, Loader};
use unic_langid::langid;

pub(crate) use message::UiMessage;

pub(crate) const FALLBACK_LOCALE: &str = "en-US";

static_loader! {
    static LOCALES = {
        locales: "./locales",
        // `static_loader!` requires a literal; keep this aligned with `FALLBACK_LOCALE`.
        fallback_language: "en-US",
    };
}

static CURRENT_LOCALE: OnceLock<LanguageIdentifier> = OnceLock::new();

/// 在应用启动时调用一次。第一版语言在进程启动时确定，修改语言后重启生效。
pub(crate) fn initialize() {
    let _ = current_locale();
}

pub(crate) fn current_locale() -> &'static LanguageIdentifier {
    CURRENT_LOCALE.get_or_init(detect_system_locale)
}

pub(crate) fn text(message: UiMessage) -> String {
    let id = message.id();
    match message.args() {
        Some(args) => LOCALES.lookup_with_args(current_locale(), id, &args),
        None => LOCALES.lookup(current_locale(), id),
    }
}

fn detect_system_locale() -> LanguageIdentifier {
    for variable in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Some(value) = env::var_os(variable) else {
            continue;
        };
        if let Some(locale) = locale_for_preference(&value.to_string_lossy()) {
            return locale;
        }
    }
    fallback_locale()
}

fn locale_for_preference(raw: &str) -> Option<LanguageIdentifier> {
    raw.split(':').find_map(locale_for)
}

fn locale_for(raw: &str) -> Option<LanguageIdentifier> {
    let normalized = raw
        .split(['.', '@'])
        .next()
        .unwrap_or(raw)
        .replace('_', "-");
    let language = normalized.split('-').next()?.to_ascii_lowercase();
    match language.as_str() {
        "zh" => Some(langid!("zh-CN")),
        "en" => Some(langid!("en-US")),
        _ => None,
    }
}

fn fallback_locale() -> LanguageIdentifier {
    FALLBACK_LOCALE
        .parse()
        .expect("FALLBACK_LOCALE must be a valid language identifier")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    use fluent_bundle::FluentResource;
    use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};

    const ZH_CATALOG: &str = include_str!("../../locales/zh-CN/main.ftl");
    const EN_CATALOG: &str = include_str!("../../locales/en-US/main.ftl");

    #[test]
    fn selects_chinese_for_zh_locale() {
        assert_eq!(locale_for("zh_CN.UTF-8"), Some(langid!("zh-CN")));
        assert_eq!(locale_for("zh-TW"), Some(langid!("zh-CN")));
    }

    #[test]
    fn selects_english_for_en_locale() {
        assert_eq!(locale_for("en_AU.UTF-8"), Some(langid!("en-US")));
        assert_eq!(locale_for("en"), Some(langid!("en-US")));
    }

    #[test]
    fn skips_unsupported_language_preferences() {
        assert_eq!(
            locale_for_preference("fr_FR:zh_CN:en_US"),
            Some(langid!("zh-CN"))
        );
        assert_eq!(
            locale_for_preference("C:POSIX:en_AU.UTF-8"),
            Some(langid!("en-US"))
        );
    }

    #[test]
    fn unsupported_locale_uses_explicit_fallback() {
        assert_eq!(locale_for("fr_FR.UTF-8"), None);
        assert_eq!(fallback_locale(), langid!("en-US"));
    }

    #[test]
    fn formats_save_before_action_with_count() {
        let args = UiMessage::GitSaveBeforeAction { count: 2 }.args().unwrap();
        let english = LOCALES.lookup_with_args(&langid!("en-US"), "git-save-before-action", &args);
        let chinese = LOCALES.lookup_with_args(&langid!("zh-CN"), "git-save-before-action", &args);
        assert_eq!(strip_bidi_isolates(&english), "Save 2 articles first");
        assert_eq!(strip_bidi_isolates(&chinese), "先保存 2 篇");
    }

    fn strip_bidi_isolates(text: &str) -> String {
        text.chars()
            .filter(|character| !matches!(character, '\u{2068}' | '\u{2069}'))
            .collect()
    }

    #[test]
    fn catalogs_parse_without_errors() {
        FluentResource::try_new(ZH_CATALOG.to_owned()).expect("zh-CN catalog must parse");
        FluentResource::try_new(EN_CATALOG.to_owned()).expect("en-US catalog must parse");
    }

    #[test]
    fn all_locale_catalogs_have_matching_keys() {
        let zh = catalog_messages(ZH_CATALOG);
        let en = catalog_messages(EN_CATALOG);
        assert_eq!(zh.keys, en.keys);
        assert_eq!(zh.variables, en.variables);
        assert_eq!(
            zh.all_keys.len(),
            zh.unique_keys.len(),
            "zh-CN contains duplicate keys"
        );
        assert_eq!(
            en.all_keys.len(),
            en.unique_keys.len(),
            "en-US contains duplicate keys"
        );
    }

    #[test]
    fn all_ui_messages_have_catalog_entries() {
        let zh = catalog_messages(ZH_CATALOG).unique_keys;
        let en = catalog_messages(EN_CATALOG).unique_keys;
        for message in message::message_samples() {
            assert!(
                zh.contains(message.id()),
                "missing zh-CN key {}",
                message.id()
            );
            assert!(
                en.contains(message.id()),
                "missing en-US key {}",
                message.id()
            );
        }
    }

    #[test]
    fn all_ui_messages_render_without_unknown_key_errors() {
        for message in message::message_samples() {
            let zh = match message.args() {
                Some(args) => LOCALES.lookup_with_args(&langid!("zh-CN"), message.id(), &args),
                None => LOCALES.lookup(&langid!("zh-CN"), message.id()),
            };
            let en = match message.args() {
                Some(args) => LOCALES.lookup_with_args(&langid!("en-US"), message.id(), &args),
                None => LOCALES.lookup(&langid!("en-US"), message.id()),
            };
            assert!(
                !zh.starts_with("Unknown localization key:"),
                "{}",
                message.id()
            );
            assert!(
                !en.starts_with("Unknown localization key:"),
                "{}",
                message.id()
            );
        }
    }

    struct CatalogMessages {
        all_keys: Vec<String>,
        keys: BTreeSet<String>,
        unique_keys: BTreeSet<String>,
        variables: BTreeMap<String, BTreeSet<String>>,
    }

    fn catalog_messages(source: &str) -> CatalogMessages {
        let resource = FluentResource::try_new(source.to_owned()).expect("catalog must parse");
        let mut keys = Vec::new();
        let mut variables = BTreeMap::new();
        for entry in resource.entries() {
            let Entry::Message(message) = entry else {
                continue;
            };
            let key = message.id.name.to_owned();
            keys.push(key.clone());
            let mut names = BTreeSet::new();
            if let Some(value) = &message.value {
                collect_pattern_variables(value, &mut names);
            }
            for attribute in &message.attributes {
                collect_pattern_variables(&attribute.value, &mut names);
            }
            variables.insert(key, names);
        }
        CatalogMessages {
            all_keys: keys.clone(),
            unique_keys: keys.iter().cloned().collect(),
            keys: keys.into_iter().collect(),
            variables,
        }
    }

    fn collect_pattern_variables(pattern: &Pattern<&str>, names: &mut BTreeSet<String>) {
        for element in &pattern.elements {
            let PatternElement::Placeable { expression } = element else {
                continue;
            };
            collect_expression_variables(expression, names);
        }
    }

    fn collect_expression_variables(expression: &Expression<&str>, names: &mut BTreeSet<String>) {
        match expression {
            Expression::Select { selector, variants } => {
                collect_inline_variables(selector, names);
                for variant in variants {
                    collect_pattern_variables(&variant.value, names);
                }
            }
            Expression::Inline(inline) => collect_inline_variables(inline, names),
        }
    }

    fn collect_inline_variables(expression: &InlineExpression<&str>, names: &mut BTreeSet<String>) {
        match expression {
            InlineExpression::VariableReference { id } => {
                names.insert(id.name.to_owned());
            }
            InlineExpression::FunctionReference { arguments, .. }
            | InlineExpression::TermReference {
                arguments: Some(arguments),
                ..
            } => {
                for positional in &arguments.positional {
                    collect_inline_variables(positional, names);
                }
                for named in &arguments.named {
                    collect_inline_variables(&named.value, names);
                }
            }
            InlineExpression::Placeable { expression } => {
                collect_expression_variables(expression, names);
            }
            _ => {}
        }
    }
}
