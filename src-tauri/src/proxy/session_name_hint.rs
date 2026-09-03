use serde_json::Value;

pub(super) const MAX_CHARS: usize = 30;

pub(super) fn from_value(value: &Value) -> Option<String> {
    let input = value.get("input")?;
    match input {
        Value::String(text) => normalize(text),
        Value::Array(items) => items.iter().find_map(user_message_text),
        Value::Object(_) => user_message_text(input),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn user_message_text(value: &Value) -> Option<String> {
    if value.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    value
        .get("content")
        .or_else(|| value.get("text"))
        .and_then(content_text)
}

fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => normalize(text),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(text_part)
                .collect::<Vec<_>>()
                .join(" ");
            normalize(&text)
        }
        Value::Object(_) => text_part(value),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn text_part(value: &Value) -> Option<String> {
    value
        .get("text")
        .and_then(Value::as_str)
        .and_then(normalize)
}

fn normalize(text: &str) -> Option<String> {
    let mut result = String::new();
    let mut count = 0;
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space |= !result.is_empty();
            continue;
        }
        if count >= MAX_CHARS {
            return Some(format!("{result}…"));
        }
        if pending_space && count < MAX_CHARS - 1 {
            result.push(' ');
            count += 1;
            pending_space = false;
        }
        result.push(character);
        count += 1;
    }
    (!result.is_empty()).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::{MAX_CHARS, from_value};

    #[test]
    fn ignores_non_user_input_and_normalizes_text_parts() {
        let value = serde_json::json!({
            "input": [
                {"role": "system", "content": "ignore"},
                {"role": "user", "content": [
                    {"type": "input_text", "text": "第一段"},
                    {"type": "input_text", "text": "第二段"}
                ]}
            ]
        });

        assert_eq!(from_value(&value).as_deref(), Some("第一段 第二段"));
    }

    #[test]
    fn truncates_by_unicode_characters() {
        let value = serde_json::json!({
            "input": "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十终"
        });

        let name = from_value(&value).expect("temporary name");
        assert_eq!(name.chars().count(), MAX_CHARS + 1);
        assert!(name.ends_with('…'));
    }
}
