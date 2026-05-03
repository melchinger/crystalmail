// Read pi's known-model inventory from disk so Settings can present a
// clickable picker instead of forcing the user to remember exact slugs.
//
// We deliberately do NOT spawn `pi models` — that probes each
// provider's health endpoint in series and routinely blocks for many
// seconds. The JSON file on disk is the same source of truth, minus
// the round-trips.
//
// File location: `~/.pi/agent/models.json`. Shape is tolerant — pi's
// actual format is `{"providers": {"<name>": {"models": [{"id": …}]}}}`
// but we also accept flat arrays, `{models:[…]}` wrappers, and
// provider-grouped string/object maps; see `extract_models_flexible`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModel {
    pub name: String,
    pub provider: String,
    /// Set by pi for the currently-selected default model. Pure display
    /// hint — we don't act on it, but the frontend can highlight the
    /// row so the user sees which model pi itself prefers.
    #[serde(default)]
    pub active: bool,
}

#[tauri::command]
pub async fn list_pi_models(_app: AppHandle) -> Result<Vec<PiModel>, String> {
    let path = models_json_path()
        .ok_or_else(|| "HOME-Verzeichnis nicht auflösbar.".to_string())?;
    match try_models_json_file().await {
        Ok(Some(models)) if !models.is_empty() => Ok(models),
        Ok(Some(_)) => Err(format!(
            "{} existiert, enthielt aber keine erkennbaren Modell-Einträge. \
             Shape-Varianten die wir unterstützen: flaches Array mit \
             {{name,provider}}-Objekten, {{\"models\":[…]}}, oder \
             {{\"providerName\":[…]}}-gruppiert.",
            path.display()
        )),
        Ok(None) => Err(format!(
            "pi's Modell-Inventar nicht gefunden: {}. \
             pi legt die Datei normalerweise bei seinem ersten Start an. \
             Hast du pi schonmal gestartet?",
            path.display()
        )),
        Err(e) => Err(e),
    }
}

fn models_json_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".pi").join("agent").join("models.json"))
}

/// Read and parse the local `~/.pi/models.json` inventory. `Ok(None)`
/// means "file doesn't exist, try something else"; `Ok(Some(vec))` is
/// a successful parse (possibly empty); `Err` surfaces read/JSON errors.
pub async fn try_models_json_file() -> Result<Option<Vec<PiModel>>, String> {
    let Some(path) = models_json_path() else {
        return Ok(None);
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: ungültiges JSON: {e}", path.display()))?;
    Ok(Some(extract_models_flexible(&value)))
}

/// Flexible extractor. Tries multiple shapes in preference order:
///   (1) pi-agent shape `{"providers": {"<name>": {"models": [{"id": …}]}}}`
///   (2) flat array of `{name,provider}` objects
///   (3) `{"models": [...]}` wrapper
///   (4) provider-grouped string/object map
///
/// For (1) we use `id` — that's what pi accepts as the model slug when
/// invoked via `--model`. The optional `name` field in pi's JSON is the
/// human-readable display label (e.g. "MiMo V2 Pro (Xiaomi direct)"),
/// which pi itself wouldn't recognise as a model identifier.
pub fn extract_models_flexible(v: &serde_json::Value) -> Vec<PiModel> {
    // (1) pi-agent shape: {"providers": {"ollama": {"models": [{"id":"gemma4"}]}}}
    if let Some(providers) = v.get("providers").and_then(|x| x.as_object()) {
        let mut out = Vec::new();
        for (provider, block) in providers {
            let Some(models) = block.get("models").and_then(|x| x.as_array()) else {
                continue;
            };
            for item in models {
                // id first (canonical slug for pi --model), fall back
                // to name only if id is missing.
                let slug = item
                    .get("id")
                    .or_else(|| item.get("name"))
                    .and_then(|x| x.as_str());
                let Some(slug) = slug else { continue };
                if slug.is_empty() {
                    continue;
                }
                // `_launch` in pi's schema means "this model is
                // launchable"/enabled, not "currently default". pi keeps
                // its per-run choice elsewhere, so we show no star here
                // — avoids misleading the user that pi would pick this
                // one automatically.
                let active = item
                    .get("active")
                    .or_else(|| item.get("default"))
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                out.push(PiModel {
                    name: slug.to_string(),
                    provider: provider.clone(),
                    active,
                });
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    // (2) Flat array of objects.
    if let Some(arr) = v.as_array() {
        let out: Vec<_> = arr.iter().filter_map(model_from_value).collect();
        if !out.is_empty() {
            return out;
        }
    }
    // (3) Wrapper object with a "models" key.
    if let Some(arr) = v.get("models").and_then(|x| x.as_array()) {
        let out: Vec<_> = arr.iter().filter_map(model_from_value).collect();
        if !out.is_empty() {
            return out;
        }
    }
    // (4) Provider-grouped map: { "ollama": [...], "xiaomi": [...] }.
    if let Some(obj) = v.as_object() {
        let mut out = Vec::new();
        for (provider, inner) in obj {
            // Inner can be an array of strings, an array of objects,
            // or a nested "models" array.
            let list = match inner {
                serde_json::Value::Array(a) => Some(a),
                serde_json::Value::Object(sub) => sub
                    .get("models")
                    .and_then(|x| x.as_array()),
                _ => None,
            };
            let Some(list) = list else { continue };
            for item in list {
                if let Some(name) = item.as_str() {
                    out.push(PiModel {
                        name: name.to_string(),
                        provider: provider.clone(),
                        active: false,
                    });
                } else if let Some(m) = model_from_value(item) {
                    // Prefer the provider from the object itself; fall
                    // back to the outer key.
                    out.push(PiModel {
                        provider: if m.provider.is_empty() {
                            provider.clone()
                        } else {
                            m.provider
                        },
                        ..m
                    });
                }
            }
        }
        return out;
    }
    Vec::new()
}

fn model_from_value(v: &serde_json::Value) -> Option<PiModel> {
    let obj = v.as_object()?;
    // pi might use different field names. Common aliases:
    //   name / id / model → name
    //   provider / backend / source → provider
    let name = obj
        .get("name")
        .or_else(|| obj.get("id"))
        .or_else(|| obj.get("model"))
        .and_then(|x| x.as_str())?
        .to_string();
    if name.is_empty() {
        return None;
    }
    let provider = obj
        .get("provider")
        .or_else(|| obj.get("backend"))
        .or_else(|| obj.get("source"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let active = obj
        .get("active")
        .or_else(|| obj.get("default"))
        .or_else(|| obj.get("current"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    Some(PiModel {
        name,
        provider,
        active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flexible_flat_array() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[{"name":"gemma4","provider":"ollama"}]"#,
        )
        .unwrap();
        let models = extract_models_flexible(&v);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "gemma4");
    }

    #[test]
    fn flexible_wrapper_object() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"models":[{"name":"gemma4","provider":"ollama","active":true}]}"#,
        )
        .unwrap();
        let models = extract_models_flexible(&v);
        assert_eq!(models.len(), 1);
        assert!(models[0].active);
    }

    #[test]
    fn flexible_provider_grouped_strings() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"ollama":["gemma4","qwen:7b"],"xiaomi":["mimo-pro"]}"#,
        )
        .unwrap();
        let mut models = extract_models_flexible(&v);
        models.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(models.len(), 3);
        assert!(models.iter().any(|m| m.name == "gemma4" && m.provider == "ollama"));
        assert!(models.iter().any(|m| m.name == "mimo-pro" && m.provider == "xiaomi"));
    }

    #[test]
    fn flexible_provider_grouped_objects() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"ollama":{"models":[{"name":"gemma4"}]}}"#,
        )
        .unwrap();
        let models = extract_models_flexible(&v);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].provider, "ollama");
    }

    #[test]
    fn tolerates_id_alias_for_name() {
        let v: serde_json::Value =
            serde_json::from_str(r#"[{"id":"claude-haiku","provider":"anthropic"}]"#)
                .unwrap();
        let models = extract_models_flexible(&v);
        assert_eq!(models[0].name, "claude-haiku");
    }

    #[test]
    fn parses_pi_agent_providers_shape() {
        // Exact shape from ~/.pi/agent/models.json on a real install.
        let raw = r#"
        {
          "providers": {
            "ollama": {
              "api": "openai-completions",
              "apiKey": "ollama",
              "baseUrl": "http://127.0.0.1:11434/v1",
              "models": [
                {"_launch": true, "id": "gemma4", "reasoning": true},
                {"_launch": true, "id": "qwen2.5-coder:7b"},
                {"_launch": true, "id": "llama3.1:8b"}
              ]
            },
            "xiaomi": {
              "baseUrl": "https://token-plan-ams.xiaomimimo.com/v1",
              "apiKey": "XIAOMI_API_KEY",
              "models": [
                {"id": "mimo-v2-pro", "name": "MiMo V2 Pro (Xiaomi direct)"},
                {"id": "xiaomi/mimo-v2.5-pro", "name": "MiMo V2.5 Pro (Xiaomi)"}
              ]
            }
          }
        }"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let models = extract_models_flexible(&v);
        assert_eq!(models.len(), 5);
        // Critical: the slug is the `id`, not the human `name`.
        assert!(models.iter().any(|m| m.name == "gemma4" && m.provider == "ollama"));
        assert!(models
            .iter()
            .any(|m| m.name == "qwen2.5-coder:7b" && m.provider == "ollama"));
        assert!(models
            .iter()
            .any(|m| m.name == "xiaomi/mimo-v2.5-pro" && m.provider == "xiaomi"));
        // Nothing should be marked "active" — pi stores its default
        // elsewhere, `_launch` is not a default-marker.
        assert!(models.iter().all(|m| !m.active));
    }
}
