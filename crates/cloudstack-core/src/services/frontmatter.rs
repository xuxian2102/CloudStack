pub mod value;

use std::path::Path;
use std::str::FromStr;

use chrono::Local;
use yaml_edit::{Document, Sequence, SequenceBuilder};

use crate::error::AppError;
use crate::model::FieldSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Text(String),
    Boolean(bool),
    Tags(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldState {
    pub name: String,
    pub value: FieldValue,
}

pub fn read_fields(raw: &str, fields: &[FieldSpec]) -> Result<Vec<FieldState>, AppError> {
    let document = parse_mapping(raw)?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| AppError::Config("Frontmatter 顶层必须是 YAML 对象".into()))?;

    Ok(fields
        .iter()
        .map(|field| {
            let node = mapping.get(field.name.as_str());
            let value = match field.field_type.as_str() {
                "boolean" => FieldValue::Boolean(
                    node.and_then(|node| node.as_scalar()?.as_bool())
                        .unwrap_or(false),
                ),
                "tags" => FieldValue::Tags(
                    node.and_then(|node| node.as_sequence().cloned())
                        .map(|sequence| {
                            (0..sequence.len())
                                .filter_map(|index| {
                                    sequence
                                        .get(index)?
                                        .as_scalar()
                                        .map(|scalar| scalar.as_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                _ => FieldValue::Text(
                    node.and_then(|node| {
                        node.as_scalar()
                            .map(|scalar| scalar.as_string())
                            .or_else(|| Some(node.to_string()))
                    })
                    .unwrap_or_default(),
                ),
            };
            FieldState {
                name: field.name.clone(),
                value,
            }
        })
        .collect())
}

pub fn set_field(raw: &str, name: &str, value: FieldValue) -> Result<String, AppError> {
    let document = parse_mapping(raw)?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| AppError::Config("Frontmatter 顶层必须是 YAML 对象".into()))?;
    // yaml-edit intentionally models comments before/after the root mapping as document trivia.
    // Mapping edits preserve entry trivia; retain the outer trivia explicitly as well.
    let range = mapping.byte_range();
    let leading = raw.get(..range.start as usize).unwrap_or_default();
    let trailing = raw.get(range.end as usize..).unwrap_or_default();
    match value {
        FieldValue::Text(value) => mapping.set(name, value),
        FieldValue::Boolean(value) => mapping.set(name, value),
        FieldValue::Tags(values) => mapping.set(name, yaml_sequence(&values)),
    }
    Ok(format!("{leading}{mapping}{trailing}"))
}

/// 按项目字段配置生成新文章的初始 Frontmatter。未知字段类型仍按字符串处理；
/// 未配置任何可初始化字段时返回 None。
pub fn initial_for_post(fields: &[FieldSpec], post_id: &str) -> Option<String> {
    let document = Document::new_mapping();
    let mapping = document.as_mapping()?;
    let stem = Path::new(post_id)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(post_id);

    for field in fields {
        if field.field_type == "string" && field.name == "title" {
            mapping.set(field.name.as_str(), stem);
        } else if let Some(default) = &field.default {
            set_json_default(&mapping, field, default);
        } else if field.field_type == "date" {
            mapping.set(
                field.name.as_str(),
                Local::now().date_naive().format("%Y-%m-%d").to_string(),
            );
        } else if field.required {
            match field.field_type.as_str() {
                "boolean" => mapping.set(field.name.as_str(), false),
                "tags" => mapping.set(field.name.as_str(), yaml_sequence(&[])),
                _ => mapping.set(field.name.as_str(), ""),
            }
        }
    }

    (!mapping.is_empty()).then(|| document.to_string())
}

fn parse_mapping(raw: &str) -> Result<Document, AppError> {
    Document::from_str(raw)
        .map_err(|error| AppError::Config(format!("Frontmatter YAML 无法解析：{error}")))
}

fn set_json_default(mapping: &yaml_edit::Mapping, field: &FieldSpec, default: &serde_json::Value) {
    match field.field_type.as_str() {
        "boolean" => mapping.set(field.name.as_str(), default.as_bool().unwrap_or(false)),
        "tags" => {
            let values = default
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            mapping.set(field.name.as_str(), yaml_sequence(&values));
        }
        _ => mapping.set(
            field.name.as_str(),
            default
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| default.to_string()),
        ),
    }
}

fn yaml_sequence(values: &[String]) -> Sequence {
    if values.is_empty() {
        return Document::from_str("[]")
            .expect("static empty sequence is valid YAML")
            .as_sequence()
            .expect("static empty sequence parses as a sequence");
    }
    values
        .iter()
        .fold(SequenceBuilder::new(), |builder, value| {
            builder.item(value.as_str())
        })
        .build_document()
        .as_sequence()
        .expect("SequenceBuilder always creates a sequence")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &str, field_type: &str) -> FieldSpec {
        FieldSpec {
            name: name.into(),
            field_type: field_type.into(),
            required: false,
            default: None,
        }
    }

    #[test]
    fn reads_supported_field_types() {
        let fields = [
            spec("title", "string"),
            spec("date", "date"),
            spec("draft", "boolean"),
            spec("tags", "tags"),
        ];
        let states = read_fields(
            "title: 'Hello'\ndate: 2026-08-05\ndraft: true\ntags: [rust, GTK]\n",
            &fields,
        )
        .unwrap();

        assert_eq!(states[0].value, FieldValue::Text("Hello".into()));
        assert_eq!(states[1].value, FieldValue::Text("2026-08-05".into()));
        assert_eq!(states[2].value, FieldValue::Boolean(true));
        assert_eq!(
            states[3].value,
            FieldValue::Tags(vec!["rust".into(), "GTK".into()])
        );
    }

    #[test]
    fn surgical_edit_preserves_unrelated_yaml() {
        let raw = "# keep this comment\ntitle: 'Old title' # inline\ncustom: { untouched: true }\n";
        let changed = set_field(raw, "title", FieldValue::Text("New title".into())).unwrap();

        assert!(changed.contains("# keep this comment"), "{changed:?}");
        assert!(changed.contains("# inline"));
        assert!(changed.contains("custom: { untouched: true }"));
        assert!(changed.contains("title: New title"));
    }

    #[test]
    fn initializes_title_defaults_dates_and_required_values() {
        let mut title = spec("title", "string");
        title.required = true;
        let mut draft = spec("draft", "boolean");
        draft.default = Some(json!(false));
        let mut tags = spec("tags", "tags");
        tags.required = true;
        let date = spec("pubDate", "date");

        let raw = initial_for_post(&[title, date, draft, tags], "nested/my-post.md").unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(value["title"], "my-post");
        assert_eq!(value["draft"], false);
        assert!(
            value["tags"].as_sequence().is_some_and(Vec::is_empty),
            "{raw:?}"
        );
        assert_eq!(value["pubDate"].as_str().unwrap().len(), 10);
    }
}
