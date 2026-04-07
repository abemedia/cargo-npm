use std::collections::HashMap;

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

/// Substitutes `{key}` and `{env.VAR}` placeholders in a template string.
///
/// For each `{...}` token the placeholder name is looked up in `vars`; if found,
/// its mapped value is inserted. If the placeholder starts with `env.`, the
/// remaining suffix is read from the process environment and inserted. Returns
/// an error if a placeholder references an unknown key, references an unset
/// environment variable, or if a `{` has no matching `}`.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
///
/// let mut vars = HashMap::new();
/// vars.insert("name", "Alice");
/// let rendered = render("hello {name}, home={env.HOME}", &vars).unwrap();
/// assert!(rendered.starts_with("hello Alice, home="));
/// ```
pub fn render(template: &str, vars: &HashMap<&str, &str>) -> Result<String> {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let end = rest
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("unclosed '{{' in template {template:?}"))?;
        let var = &rest[..end];
        rest = &rest[end + 1..];
        if let Some(val) = vars.get(var) {
            result.push_str(val);
        } else if let Some(var_name) = var.strip_prefix("env.") {
            let var_value = std::env::var(var_name).with_context(|| {
                format!(
                    "template variable {{env.{var_name}}} references unset environment variable"
                )
            })?;
            result.push_str(&var_value);
        } else {
            anyhow::bail!("unknown template variable {{{var}}} in {template:?}");
        }
    }
    result.push_str(rest);
    Ok(result)
}

/// Recursively substitutes template placeholders in all string values of a JSON value.
///
/// This function walks the provided JSON value and replaces `{key}` placeholders from `vars`
/// and `{env.VAR}` placeholders from the environment in every string it encounters.
/// Non-string JSON values (numbers, booleans, null) are returned unchanged.
///
/// # Parameters
///
/// - `value`: Any value convertible into `serde_json::Value` to be processed.
/// - `vars`: Mapping of placeholder names to replacement strings used for `{key}` substitutions.
///
/// # Returns
///
/// A `serde_json::Value` equivalent to the input with all string placeholders substituted.
///
/// # Errors
///
/// Returns an error if any string contains an unknown placeholder, references an unset environment
/// variable, or contains an unclosed `{` brace.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use serde_json::json;
///
/// let mut vars = HashMap::new();
/// vars.insert("name", "Alice");
///
/// let input = json!({
///     "greeting": "Hello, {name}!",
///     "nested": ["literal", "{name}"],
///     "count": 3
/// });
///
/// let rendered = template::render_json(input, &vars).unwrap();
/// assert_eq!(rendered["greeting"], "Hello, Alice!");
/// assert_eq!(rendered["nested"][1], "Alice");
/// assert_eq!(rendered["count"], 3);
/// ```
pub fn render_json(value: impl Into<Value>, vars: &HashMap<&str, &str>) -> Result<Value> {
    match value.into() {
        Value::String(s) => Ok(Value::String(render(&s, vars)?)),
        Value::Object(m) => {
            let rendered = m
                .into_iter()
                .map(|(k, v)| render_json(v, vars).map(|v| (k, v)))
                .collect::<Result<Map<_, _>>>()?;
            Ok(Value::Object(rendered))
        }
        Value::Array(arr) => arr
            .into_iter()
            .map(|v| render_json(v, vars))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{render, render_json};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn substitutes_known_vars() {
        let vars = HashMap::from([("foo", "bar"), ("baz", "qux")]);
        assert_eq!(render("{foo} and {baz}", &vars).unwrap(), "bar and qux");
    }

    #[test]
    fn passes_through_literal_text() {
        let vars = HashMap::new();
        assert_eq!(render("no vars here", &vars).unwrap(), "no vars here");
    }

    #[test]
    fn substitutes_env_var() {
        // Use CARGO, which cargo always sets when running tests - avoids set_var races.
        let cargo = std::env::var("CARGO").expect("CARGO env var not set");
        let vars = HashMap::new();
        assert_eq!(render("{env.CARGO}", &vars).unwrap(), cargo);
    }

    #[test]
    fn errors_on_unknown_var() {
        let vars = HashMap::new();
        assert!(render("{unknown}", &vars).is_err());
    }

    #[test]
    fn errors_on_unclosed_brace() {
        let vars = HashMap::new();
        assert!(render("hello {world", &vars).is_err());
    }

    #[test]
    fn render_json_substitutes_all_value_types() {
        let input = json!({
            "str": "hello {x}",
            "arr": ["{x}", "literal", "{x}!"],
            "obj": { "nested": "{x}", "other": "{x}-end" }
        });
        let vars = HashMap::from([("x", "world")]);
        let result = render_json(input, &vars).unwrap();

        assert_eq!(
            result,
            json!({
                "str": "hello world",
                "arr": ["world", "literal", "world!"],
                "obj": { "nested": "world", "other": "world-end" }
            })
        );
    }

    #[test]
    fn errors_on_unset_env_var() {
        let vars = HashMap::new();
        assert!(render("{env.CARGO_NPM_DEFINITELY_NOT_SET_XYZ}", &vars).is_err());
    }
}
