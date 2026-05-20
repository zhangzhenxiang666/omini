use serde_yaml::Mapping;
use serde_yaml::Value;

#[derive(Debug, Clone, Copy)]
pub struct FrontmatterParts<'a> {
    pub frontmatter: &'a str,
    pub body: &'a str,
}

pub fn split(content: &str) -> Result<FrontmatterParts<'_>, String> {
    let content = content
        .strip_prefix("---")
        .ok_or_else(|| "missing frontmatter; expected file to start with '---'".to_string())?;
    let content = content.strip_prefix('\n').unwrap_or(content);
    let Some((frontmatter, body)) = content.split_once("\n---") else {
        return Err("missing closing frontmatter delimiter '---'".to_string());
    };
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok(FrontmatterParts { frontmatter, body })
}

pub fn parse(content: &str) -> Result<(Mapping, &str), String> {
    let parts = split(content)?;
    let frontmatter = parse_yaml(parts.frontmatter)?;
    Ok((frontmatter, parts.body))
}

pub fn parse_yaml(frontmatter: &str) -> Result<Mapping, String> {
    let value: Value =
        serde_yaml::from_str(frontmatter).map_err(|e| format!("invalid frontmatter YAML: {e}"))?;
    match value {
        Value::Mapping(mapping) => Ok(mapping),
        Value::Null => Ok(Mapping::new()),
        _ => Err("frontmatter must be a YAML mapping".to_string()),
    }
}

pub fn get<'a>(raw: &'a Mapping, key: &str) -> Option<&'a Value> {
    raw.get(Value::String(key.to_string()))
}

pub fn get_path<'a>(raw: &'a Mapping, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut value = get(raw, first)?;
    for key in rest {
        let Value::Mapping(mapping) = value else {
            return None;
        };
        value = get(mapping, key)?;
    }
    Some(value)
}

pub fn required_string(raw: &Mapping, key: &str) -> Result<String, String> {
    let value =
        get(raw, key).ok_or_else(|| format!("missing required frontmatter field '{key}'"))?;
    let value = string_value(value, key)?;
    if value.trim().is_empty() {
        return Err(format!("frontmatter field '{key}' must not be empty"));
    }
    Ok(value)
}

pub fn optional_string(raw: &Mapping, key: &str) -> Result<Option<String>, String> {
    get(raw, key)
        .map(|value| string_value(value, key))
        .transpose()
}

pub fn optional_string_list(raw: &Mapping, key: &str) -> Result<Option<Vec<String>>, String> {
    get(raw, key)
        .map(|value| string_list_value(value, key))
        .transpose()
}

pub fn optional_bool_path(raw: &Mapping, path: &[&str]) -> Result<Option<bool>, String> {
    get_path(raw, path)
        .map(|value| bool_value(value, &path.join(".")))
        .transpose()
}

fn string_value(value: &Value, key: &str) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        _ => Err(format!("frontmatter field '{key}' must be a string")),
    }
}

fn string_list_value(value: &Value, key: &str) -> Result<Vec<String>, String> {
    match value {
        Value::String(value) => Ok(value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect()),
        Value::Sequence(values) => values
            .iter()
            .map(|value| string_value(value, key))
            .collect(),
        _ => Err(format!(
            "frontmatter field '{key}' must be a string or string array"
        )),
    }
}

fn bool_value(value: &Value, key: &str) -> Result<bool, String> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::String(value) if value == "true" => Ok(true),
        Value::String(value) if value == "false" => Ok(false),
        _ => Err(format!("frontmatter field '{key}' must be a boolean")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let parts = split("---\nname: demo\n---\nBody\n").unwrap();

        assert_eq!(parts.frontmatter, "name: demo");
        assert_eq!(parts.body, "Body\n");
    }

    #[test]
    fn rejects_missing_opening_delimiter() {
        let err = split("name: demo\n---\nBody").unwrap_err();

        assert!(err.contains("missing frontmatter"));
    }

    #[test]
    fn rejects_missing_closing_delimiter() {
        let err = split("---\nname: demo\nBody").unwrap_err();

        assert!(err.contains("missing closing frontmatter delimiter"));
    }

    #[test]
    fn parses_nested_yaml_metadata() {
        let (raw, body) = parse(
            r#"---
name: demo
description: Demo
metadata:
  inject: false
---
Body
"#,
        )
        .unwrap();

        assert_eq!(required_string(&raw, "name").unwrap(), "demo");
        assert_eq!(
            optional_bool_path(&raw, &["metadata", "inject"]).unwrap(),
            Some(false)
        );
        assert_eq!(body, "Body\n");
    }

    #[test]
    fn parses_string_and_array_lists() {
        let raw = parse_yaml(
            r#"
tools: "Read, Write"
disallow_tools: ["Bash", "Edit"]
"#,
        )
        .unwrap();

        assert_eq!(
            optional_string_list(&raw, "tools").unwrap(),
            Some(vec!["Read".to_string(), "Write".to_string()])
        );
        assert_eq!(
            optional_string_list(&raw, "disallow_tools").unwrap(),
            Some(vec!["Bash".to_string(), "Edit".to_string()])
        );
    }
}
