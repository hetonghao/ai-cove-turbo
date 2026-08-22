use serde_json::{Map, Value, json};

use super::{CatalogChange, CatalogModel};

pub(super) fn diff_models(before: &Value, after: &Value) -> Vec<CatalogChange> {
    let before_models = models_by_slug(before);
    let after_models = models_by_slug(after);
    let mut changes = Vec::new();
    let mut slugs = std::collections::BTreeSet::new();
    slugs.extend(before_models.keys().copied());
    slugs.extend(after_models.keys().copied());
    for slug in slugs {
        let previous = before_models.get(slug).copied();
        let next = after_models.get(slug).copied();
        let Some(next) = next else {
            changes.push(CatalogChange {
                slug: slug.to_owned(),
                field: "model".to_owned(),
                before: previous
                    .and_then(|model| model.get("display_name"))
                    .and_then(change_value),
                after: None,
            });
            continue;
        };
        let Some(previous) = previous else {
            changes.push(CatalogChange {
                slug: slug.to_owned(),
                field: "model".to_owned(),
                before: None,
                after: next.get("display_name").and_then(change_value),
            });
            continue;
        };
        let mut fields = std::collections::BTreeSet::new();
        if let Some(object) = previous.as_object() {
            fields.extend(object.keys().cloned());
        }
        if let Some(object) = next.as_object() {
            fields.extend(object.keys().cloned());
        }
        for field in fields {
            if field == "slug" {
                continue;
            }
            let before = previous.get(&field);
            let after = next.get(&field);
            if before == after {
                continue;
            }
            changes.push(CatalogChange {
                slug: slug.to_owned(),
                field,
                before: before.and_then(change_value),
                after: after.and_then(change_value),
            });
        }
    }
    changes
}

fn models_by_slug(document: &Value) -> std::collections::BTreeMap<&str, &Value> {
    document
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| Some((model.get("slug")?.as_str()?, model)))
                .collect()
        })
        .unwrap_or_default()
}

fn change_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| serde_json::to_string(value).ok())
}

pub(super) fn models_document(models: &[CatalogModel]) -> Value {
    Value::Object(Map::from_iter([(
        "models".to_owned(),
        Value::Array(
            models
                .iter()
                .map(|model| {
                    json!({
                        "slug": model.slug,
                        "display_name": model.display_name,
                        "description": model.description,
                        "visibility": model.visibility,
                        "priority": model.priority,
                    })
                })
                .collect(),
        ),
    )]))
}
