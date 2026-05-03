// Python argparse analyzer. Ported from the `launchpad` sibling project
// (see `D:\LLM\github\melchinger\launchpad\src-tauri\src\main.rs`).
//
// Scans a `.py` file for `parser.add_argument(...)` call sites and
// extracts each declared parameter — kind (positional/option/flag),
// value type, choices, default, required, help. Designed to drive a
// guided editor in Settings: the user picks a script, the analyzer
// surfaces its CLI surface, and the workflow author then binds each
// parameter to a fixed value or one of our template variables
// (`$csv`, `$subject`, …).
//
// The analysis is deliberately heuristic, not AST-based. Regex +
// balanced-paren scanning catches 95 % of typical scripts; the
// remaining 5 % (dynamic argument construction, decorated argparse
// wrappers, inherited parsers) the user edits manually in the UI.
//
// Scope differences from launchpad:
//   * We emit our own `ScriptParam` type (the one we store in
//     workflows), pre-populated with `source = Fixed` using the
//     script's default. The editor rebinds to `Template` where that
//     makes sense.
//   * Keys, labels and choices are produced the same way launchpad
//     does — the UX parity keeps users' muscle memory intact.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::domain::workflow::{
    ParamSource, ParameterKind, ScriptParam, ValueType,
};

/// Analyse a Python file and return the parameter list the script
/// declares. Empty result = `parser.add_argument(...)` not found
/// (script uses a different framework, no CLI, or too dynamic to
/// parse). The caller surfaces that as "no parameters detected —
/// add them manually" in the UI.
pub fn analyze(path: &Path) -> Result<Vec<ScriptParam>, String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("Lesen {}: {e}", path.display()))?;
    let constants = collect_python_constants(&source);
    let calls = extract_add_argument_calls(&source);
    let params: Vec<ScriptParam> = calls
        .into_iter()
        .enumerate()
        .filter_map(|(order, call)| parse_argument_call(&call, order as u32, &constants))
        .collect();
    Ok(params)
}

// ─── internals ────────────────────────────────────────────────────────

fn extract_add_argument_calls(source: &str) -> Vec<String> {
    let marker = ".add_argument(";
    let mut results = Vec::new();
    let bytes = source.as_bytes();
    let mut start_at = 0;

    while let Some(relative_idx) = source[start_at..].find(marker) {
        let open_idx = start_at + relative_idx + marker.len() - 1;
        let mut idx = open_idx + 1;
        let mut depth = 1usize;
        let mut in_string = false;
        let mut string_char = b'"';
        let mut escaped = false;

        while idx < bytes.len() {
            let current = bytes[idx];
            if in_string {
                if escaped {
                    escaped = false;
                } else if current == b'\\' {
                    escaped = true;
                } else if current == string_char {
                    in_string = false;
                }
            } else if current == b'\'' || current == b'"' {
                in_string = true;
                string_char = current;
            } else if current == b'(' {
                depth += 1;
            } else if current == b')' {
                depth -= 1;
                if depth == 0 {
                    results.push(source[open_idx + 1..idx].to_string());
                    start_at = idx + 1;
                    break;
                }
            }
            idx += 1;
        }

        if idx >= bytes.len() {
            break;
        }
    }

    results
}

fn parse_argument_call(
    call: &str,
    order: u32,
    constants: &HashMap<String, String>,
) -> Option<ScriptParam> {
    let parts = split_top_level(call, ',');
    if parts.is_empty() {
        return None;
    }

    let mut positionals: Vec<String> = Vec::new();
    let mut kwargs: HashMap<String, String> = HashMap::new();

    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(eq_idx) = top_level_char_index(trimmed, '=') {
            let key = trimmed[..eq_idx].trim().to_string();
            let value = trimmed[eq_idx + 1..].trim().to_string();
            kwargs.insert(key, value);
        } else {
            positionals.push(trimmed.to_string());
        }
    }

    let literal_args: Vec<String> = positionals
        .iter()
        .filter_map(|item| parse_python_string_literal(item))
        .collect();
    if literal_args.is_empty() {
        return None;
    }

    let option_names: Vec<String> = literal_args
        .iter()
        .filter(|item| item.starts_with('-'))
        .cloned()
        .collect();
    let positional_name: Option<String> = literal_args
        .iter()
        .find(|item| !item.starts_with('-'))
        .cloned();

    let cli_name = option_names
        .iter()
        .find(|name| name.starts_with("--"))
        .cloned()
        .or_else(|| option_names.first().cloned())
        .or(positional_name.clone())?;
    let key = positional_name
        .clone()
        .unwrap_or_else(|| cli_name.trim_start_matches('-').to_string())
        .replace('-', "_");

    let action = kwargs
        .get("action")
        .and_then(|v| parse_python_string_literal(v));
    let choices: Vec<String> = kwargs
        .get("choices")
        .map(|v| parse_python_collection_of_literals(v))
        .unwrap_or_default();
    let default_value = kwargs
        .get("default")
        .and_then(|v| resolve_default_value(v, constants));
    let help_text = kwargs
        .get("help")
        .and_then(|v| parse_python_string_literal(v));
    let required = kwargs
        .get("required")
        .and_then(|v| parse_python_bool(v))
        .unwrap_or_else(|| positional_name.is_some());

    let kind = if matches!(action.as_deref(), Some("store_true" | "store_false")) {
        ParameterKind::Flag
    } else if positional_name.is_some() && option_names.is_empty() {
        ParameterKind::Positional
    } else {
        ParameterKind::Option
    };

    let value_type = if !choices.is_empty() {
        ValueType::Choice
    } else if matches!(action.as_deref(), Some("store_true" | "store_false")) {
        ValueType::Boolean
    } else if let Some(type_name) = kwargs.get("type") {
        match type_name.trim().trim_matches('\'').trim_matches('"') {
            "int" | "float" => ValueType::Number,
            "bool" => ValueType::Boolean,
            "Path" | "pathlib.Path" => ValueType::Path,
            _ => infer_value_type(&key, &cli_name),
        }
    } else {
        infer_value_type(&key, &cli_name)
    };

    // Source default: fixed value pre-populated from argparse default
    // if we have one, otherwise empty-fixed so the editor highlights
    // it as "needs attention" for a required param. The editor can
    // then let the user switch to `Template` where semantics fit.
    let source = ParamSource::Fixed {
        value: default_value.clone().unwrap_or_default(),
    };

    let label = humanize_label(&key);

    Some(ScriptParam {
        key,
        cli_name,
        kind,
        label,
        value_type,
        choices,
        help_text,
        required,
        default_value,
        source,
        order,
        enabled: true,
    })
}

fn infer_value_type(key: &str, cli_name: &str) -> ValueType {
    let lowered = format!("{} {}", key.to_lowercase(), cli_name.to_lowercase());
    if lowered.contains("path") || lowered.contains("file") || lowered.contains("dir") {
        ValueType::Path
    } else {
        ValueType::String
    }
}

fn humanize_label(value: &str) -> String {
    value
        .split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut in_string = false;
    let mut string_char = '\0';
    let mut escaped = false;

    for ch in input.chars() {
        if in_string {
            current.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == string_char {
                in_string = false;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                in_string = true;
                string_char = ch;
                current.push(ch);
            }
            '(' => {
                depth_paren += 1;
                current.push(ch);
            }
            ')' => {
                depth_paren -= 1;
                current.push(ch);
            }
            '[' => {
                depth_bracket += 1;
                current.push(ch);
            }
            ']' => {
                depth_bracket -= 1;
                current.push(ch);
            }
            '{' => {
                depth_brace += 1;
                current.push(ch);
            }
            '}' => {
                depth_brace -= 1;
                current.push(ch);
            }
            _ if ch == delimiter
                && depth_paren == 0
                && depth_bracket == 0
                && depth_brace == 0 =>
            {
                result.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        result.push(current.trim().to_string());
    }
    result
}

fn top_level_char_index(input: &str, needle: char) -> Option<usize> {
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut in_string = false;
    let mut string_char = '\0';
    let mut escaped = false;

    for (index, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == string_char {
                in_string = false;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                in_string = true;
                string_char = ch;
            }
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '[' => depth_bracket += 1,
            ']' => depth_bracket -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            _ if ch == needle
                && depth_paren == 0
                && depth_bracket == 0
                && depth_brace == 0 =>
            {
                return Some(index)
            }
            _ => {}
        }
    }
    None
}

fn collect_python_constants(source: &str) -> HashMap<String, String> {
    let mut constants: HashMap<String, String> = HashMap::new();
    // Up to four passes so a chain like `A=B`, `B=C`, `C="x"` resolves
    // regardless of declaration order.
    for _ in 0..4 {
        let mut changed = false;
        for line in source.lines() {
            if line.trim().is_empty()
                || line.starts_with(' ')
                || line.starts_with('\t')
            {
                continue;
            }
            let Some(eq_idx) = line.find('=') else {
                continue;
            };
            let name = line[..eq_idx].trim();
            if !is_python_identifier(name) {
                continue;
            }
            let value = line[eq_idx + 1..]
                .split('#')
                .next()
                .map(str::trim)
                .unwrap_or_default();
            if value.is_empty() {
                continue;
            }
            if let Some(resolved) = resolve_default_value(value, &constants) {
                let needs_update = constants
                    .get(name)
                    .map(|current| current != &resolved)
                    .unwrap_or(true);
                if needs_update {
                    constants.insert(name.to_string(), resolved);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    constants
}

fn resolve_default_value(raw: &str, constants: &HashMap<String, String>) -> Option<String> {
    normalize_literal_value(raw)
        .or_else(|| parse_os_getenv_default(raw, constants))
        .or_else(|| constants.get(raw.trim()).cloned())
}

fn parse_os_getenv_default(
    raw: &str,
    constants: &HashMap<String, String>,
) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("os.getenv(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed["os.getenv(".len()..trimmed.len() - 1];
    let parts = split_top_level(inner, ',');
    let env_name = parts.first().and_then(|p| parse_python_string_literal(p))?;
    std::env::var(&env_name)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| constants.get(&env_name).cloned())
        .or_else(|| {
            parts
                .get(1)
                .and_then(|fallback| resolve_default_value(fallback, constants))
        })
}

fn is_python_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_python_string_literal(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    // Skip string prefixes (r, b, u, f, rb, br, …).
    let mut start = 0usize;
    while start < bytes.len() && bytes[start].is_ascii_alphabetic() {
        start += 1;
    }
    if bytes.len().saturating_sub(start) < 2 {
        return None;
    }
    let quote = bytes[start];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    if *bytes.last()? != quote {
        return None;
    }
    let content = &trimmed[start + 1..trimmed.len() - 1];
    Some(
        content
            .replace("\\\\", "\\")
            .replace("\\\"", "\"")
            .replace("\\'", "'")
            .replace("\\n", "\n")
            .replace("\\t", "\t"),
    )
}

fn parse_python_collection_of_literals(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if !((trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('(') && trimmed.ends_with(')')))
    {
        return Vec::new();
    }
    split_top_level(&trimmed[1..trimmed.len() - 1], ',')
        .into_iter()
        .filter_map(|e| normalize_literal_value(&e))
        .collect()
}

fn normalize_literal_value(raw: &str) -> Option<String> {
    parse_python_string_literal(raw)
        .or_else(|| parse_python_bool(raw).map(|v| v.to_string()))
        .or_else(|| parse_python_number(raw))
}

fn parse_python_bool(raw: &str) -> Option<bool> {
    match raw.trim() {
        "True" => Some(true),
        "False" => Some(false),
        _ => None,
    }
}

fn parse_python_number(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.parse::<i64>().is_ok() || t.parse::<f64>().is_ok() {
        Some(t.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::workflow::{ParameterKind, ValueType};

    #[test]
    fn parses_argparse_options() {
        let source = r#"
parser.add_argument("input", help="File")
parser.add_argument("--count", type=int, default=3)
parser.add_argument("--mode", choices=["fast", "slow"], required=True)
parser.add_argument("--verbose", action="store_true")
"#;
        let constants = collect_python_constants(source);
        let calls = extract_add_argument_calls(source);
        let params: Vec<ScriptParam> = calls
            .into_iter()
            .enumerate()
            .filter_map(|(order, call)| {
                parse_argument_call(&call, order as u32, &constants)
            })
            .collect();
        assert_eq!(params.len(), 4);
        assert!(matches!(params[0].kind, ParameterKind::Positional));
        assert!(matches!(params[1].value_type, ValueType::Number));
        assert!(matches!(params[2].value_type, ValueType::Choice));
        assert!(matches!(params[3].kind, ParameterKind::Flag));
    }

    #[test]
    fn resolves_constant_chain() {
        let source = r#"
A = "first"
B = A
C = B

parser.add_argument("--x", default=C)
"#;
        let constants = collect_python_constants(source);
        let calls = extract_add_argument_calls(source);
        let params: Vec<ScriptParam> = calls
            .into_iter()
            .enumerate()
            .filter_map(|(order, call)| {
                parse_argument_call(&call, order as u32, &constants)
            })
            .collect();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].default_value.as_deref(), Some("first"));
    }
}
