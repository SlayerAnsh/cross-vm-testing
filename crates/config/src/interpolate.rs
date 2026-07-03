//! Stage 2 of the loader pipeline: environment variable interpolation over a raw
//! [`toml::Value`] document, before merge or typed deserialization.
//!
//! Syntax, applied only to string scalars:
//! - `${VAR}` is replaced by `vars(VAR)`; if unset, this is a hard error naming the variable
//!   and the TOML path (e.g. `chain[1].rpc_url`), never the surrounding value.
//! - `${VAR:-default}` falls back to the literal `default` text when `VAR` is unset.
//! - `$${` escapes to a literal `${` (no interpolation of what follows the escaped brace).
//! - A `$` not immediately followed by `{` (and not part of a `$${` escape) is a literal `$`.
//!
//! Values may carry RPC secrets, so no error path in this module ever includes a resolved or
//! literal value; only variable names and TOML paths are named.

use crate::value::{Doc, DocMap};
use crate::ConfigError;

/// Walks every string value in `value` (recursively through tables and arrays) and
/// interpolates it in place using `vars`.
///
/// Errors name the offending variable and its TOML path (e.g. `chain[1].rpc_url`); the
/// string's contents are never echoed, since they may carry RPC secrets.
///
/// Public entry point retained for the TOML value type; the loader itself calls the
/// format-agnostic `interpolate_doc` worker so JSON input is interpolated identically.
pub fn interpolate_value(
    value: &mut toml::Value,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    interpolate_doc(value, vars)
}

/// Format-agnostic interpolation entry point: runs over any [`Doc`] value ([`toml::Value`] or
/// [`serde_json::Value`]) with byte-identical `${VAR}` / `${VAR:-default}` / `$${` semantics.
pub(crate) fn interpolate_doc<V: Doc>(
    value: &mut V,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    interpolate_at(value, vars, "")
}

/// Recursive worker for [`interpolate_doc`]; `path` is the dotted/indexed document path to
/// `value` so far (empty string at the document root).
fn interpolate_at<V: Doc>(
    value: &mut V,
    vars: &dyn Fn(&str) -> Option<String>,
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(s) = value.as_str_mut() {
        let interpolated = interpolate_str(s, vars, path)?;
        *s = interpolated;
        return Ok(());
    }
    if let Some(items) = value.as_array_mut() {
        for (i, item) in items.iter_mut().enumerate() {
            let child_path = format!("{path}[{i}]");
            interpolate_at(item, vars, &child_path)?;
        }
        return Ok(());
    }
    if let Some(table) = value.as_object_mut() {
        for (k, v) in table.iter_mut() {
            let child_path = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            interpolate_at(v, vars, &child_path)?;
        }
        return Ok(());
    }
    // Booleans, integers, floats, and datetimes cannot carry `${...}` syntax.
    Ok(())
}

/// Interpolates a single string value, scanning character by character so `$${` escapes and
/// `${VAR:-default}` fallbacks are handled without regex.
fn interpolate_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
    path: &str,
) -> Result<String, ConfigError> {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '$' {
            result.push(chars[i]);
            i += 1;
            continue;
        }

        // `$${` escapes to a literal `${`; nothing after it is treated as an expression.
        if i + 2 < chars.len() && chars[i + 1] == '$' && chars[i + 2] == '{' {
            result.push('$');
            result.push('{');
            i += 3;
            continue;
        }

        // `${...}`: find the matching close brace, then split on the first `:-`.
        if i + 1 < chars.len() && chars[i + 1] == '{' {
            let start = i + 2;
            let mut j = start;
            let mut found_close = false;
            while j < chars.len() {
                if chars[j] == '}' {
                    found_close = true;
                    break;
                }
                j += 1;
            }
            if !found_close {
                return Err(ConfigError::Interpolation {
                    path: path.to_string(),
                    message: "unterminated `${` (missing closing `}`)".to_string(),
                });
            }

            let content: String = chars[start..j].iter().collect();
            let (var_name, default_val) = match content.find(":-") {
                Some(idx) => (
                    content[..idx].to_string(),
                    Some(content[idx + 2..].to_string()),
                ),
                None => (content, None),
            };

            if var_name.is_empty() {
                return Err(ConfigError::Interpolation {
                    path: path.to_string(),
                    message: "empty variable name in `${}`".to_string(),
                });
            }

            match vars(&var_name) {
                Some(val) => result.push_str(&val),
                None => match default_val {
                    Some(def) => result.push_str(&def),
                    None => {
                        return Err(ConfigError::MissingVar {
                            var: var_name,
                            path: path.to_string(),
                        })
                    }
                },
            }

            i = j + 1;
            continue;
        }

        // A lone `$` not starting `${` or `$${`.
        result.push('$');
        i += 1;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_vars(_: &str) -> Option<String> {
        None
    }

    fn map_vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn missing_var_errors_naming_var_and_path_not_value() {
        let mut value: toml::Value = toml::from_str(
            r#"
            [[chain]]
            label = "eth"
            rpc_url = "${MISSING_VAR}"
            "#,
        )
        .unwrap();

        let err = interpolate_value(&mut value, &no_vars).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("MISSING_VAR"), "message: {message}");
        assert!(message.contains("chain[0].rpc_url"), "message: {message}");
    }

    #[test]
    fn missing_var_error_never_echoes_secret_looking_prefix() {
        let mut value: toml::Value = toml::from_str(
            r#"
            rpc_url = "shhh-secret-token-abc123${MISSING_VAR}"
            "#,
        )
        .unwrap();

        let err = interpolate_value(&mut value, &no_vars).unwrap_err();
        let message = err.to_string();
        assert!(
            !message.contains("shhh-secret-token-abc123"),
            "message must not echo the surrounding value: {message}"
        );
        assert!(message.contains("MISSING_VAR"));
        assert!(message.contains("rpc_url"));
    }

    #[test]
    fn default_fallback_used_when_var_unset() {
        let mut value: toml::Value =
            toml::from_str(r#"rpc_url = "${ETH_RPC:-https://fallback}""#).unwrap();
        interpolate_value(&mut value, &no_vars).unwrap();
        assert_eq!(
            value.get("rpc_url").unwrap().as_str(),
            Some("https://fallback")
        );
    }

    #[test]
    fn default_fallback_overridden_when_var_set() {
        let vars = map_vars(&[("ETH_RPC", "https://real-rpc")]);
        let mut value: toml::Value =
            toml::from_str(r#"rpc_url = "${ETH_RPC:-https://fallback}""#).unwrap();
        interpolate_value(&mut value, &vars).unwrap();
        assert_eq!(
            value.get("rpc_url").unwrap().as_str(),
            Some("https://real-rpc")
        );
    }

    #[test]
    fn escaped_dollar_brace_yields_literal() {
        let mut value: toml::Value = toml::from_str(r#"s = "$${NOT_INTERPOLATED}""#).unwrap();
        interpolate_value(&mut value, &no_vars).unwrap();
        assert_eq!(
            value.get("s").unwrap().as_str(),
            Some("${NOT_INTERPOLATED}")
        );
    }

    #[test]
    fn lone_dollar_is_literal() {
        let mut value: toml::Value = toml::from_str(r#"s = "price is $5""#).unwrap();
        interpolate_value(&mut value, &no_vars).unwrap();
        assert_eq!(value.get("s").unwrap().as_str(), Some("price is $5"));
    }

    #[test]
    fn nested_table_and_array_paths_are_tracked() {
        let mut value: toml::Value = toml::from_str(
            r#"
            [env.params]
            rpc_label = "${TARGET_CHAIN}"
            "#,
        )
        .unwrap();
        let err = interpolate_value(&mut value, &no_vars).unwrap_err();
        assert!(err.to_string().contains("env.params.rpc_label"));
    }

    #[test]
    fn resolved_var_is_substituted_into_string() {
        let vars = map_vars(&[("NAME", "osmosis")]);
        let mut value: toml::Value = toml::from_str(r#"label = "chain-${NAME}""#).unwrap();
        interpolate_value(&mut value, &vars).unwrap();
        assert_eq!(value.get("label").unwrap().as_str(), Some("chain-osmosis"));
    }
}
