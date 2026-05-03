// Persistent JSON-RPC client for the `pi` agent CLI. Spawns `pi --mode rpc`
// once, keeps it alive across turns, and demultiplexes stdout into command
// responses (correlated via `id`) and streaming events (forwarded to the
// frontend as `chat-stream` / `pi-thinking` / `pi-tool`).
//
// Ported 1:1 from mila (D:\LLM\github\melchinger\mila\src-tauri\src\llm\pi_rpc.rs).
// Only `StreamChunk` import path and crate refs were adapted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

use crate::llm::StreamChunk;
use crate::state::PiConfig;

const DEFAULT_SESSION_FILE: &str = "pi_session.jsonl";

/// Iterator over every balanced top-level `{...}` block in `raw`.
/// Skips from one closing brace to the next opening brace, so we find
/// *all* candidate objects, not just the first. gemma sometimes emits
/// a thinking-style `{"analysis":"..."}` before the actual
/// `{"rules":[...]}`; the caller inspects each candidate to pick the
/// one that matches the expected schema.
///
/// Naive brace counter: doesn't respect string escapes, so `"{"` inside
/// a JSON string could confuse it. In practice pi-generated JSON is
/// flat enough that this works; the follow-up `serde_json::from_str`
/// guard catches the rare false positive.
fn iter_balanced_json<'a>(raw: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let bytes = raw.as_bytes();
    let mut cursor = 0usize;
    std::iter::from_fn(move || {
        while cursor < bytes.len() {
            let start = bytes[cursor..].iter().position(|&b| b == b'{')? + cursor;
            let mut depth: i32 = 0;
            let mut i = start;
            while i < bytes.len() {
                match bytes[i] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            let slice = &raw[start..=i];
                            cursor = i + 1;
                            return Some(slice);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            // Unbalanced — buffer is still growing. Bail; next poll will retry.
            return None;
        }
        None
    })
}

pub fn resolve_session_path(cfg: &PiConfig) -> Option<PathBuf> {
    let home = dirs::home_dir();
    let dir = if !cfg.session_dir.is_empty() {
        PathBuf::from(&cfg.session_dir)
    } else {
        home?.join(".crystalmail")
    };
    let _ = std::fs::create_dir_all(&dir);

    let file_name = if cfg.session_file.is_empty() {
        DEFAULT_SESSION_FILE
    } else {
        cfg.session_file.as_str()
    };
    let candidate = PathBuf::from(file_name);
    Some(if candidate.is_absolute() {
        candidate
    } else {
        dir.join(file_name)
    })
}

pub fn fingerprint(cfg: &PiConfig) -> String {
    serde_json::to_string(cfg).unwrap_or_default()
}

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;
type TurnSlot = Arc<Mutex<Option<oneshot::Sender<Result<(), String>>>>>;
/// Accumulates `text_delta` content while a prompt turn is active. Used by
/// `prompt_collect` callers that need the full agent response instead of
/// (or in addition to) the streaming deltas. The reader loop always appends
/// to it; callers who don't care simply ignore the buffer.
type CollectBuf = Arc<Mutex<String>>;

pub struct PiRpc {
    #[allow(dead_code)]
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: PendingMap,
    turn_done: TurnSlot,
    collected: CollectBuf,
    /// Serializes concurrent prompt calls. pi-rpc is a single-turn model
    /// — we can't interleave a "spam analysis" prompt with an ongoing
    /// chat-terminal prompt without scrambling their streams. This mutex
    /// gates entry into `prompt`/`prompt_collect` so the second caller
    /// waits for the first to finish.
    turn_gate: Mutex<()>,
    next_id: AtomicU64,
    fingerprint: String,
}

impl PiRpc {
    pub async fn spawn(app: AppHandle, cfg: &PiConfig) -> Result<Arc<Self>, String> {
        let fp = fingerprint(cfg);
        let session_path = resolve_session_path(cfg);

        let mut args: Vec<String> = vec![
            "--mode".into(),
            "rpc".into(),
            "--provider".into(),
            cfg.provider.clone(),
            "--model".into(),
            cfg.model.clone(),
        ];
        if let Some(p) = &session_path {
            args.push("--session".into());
            args.push(p.to_string_lossy().into_owned());
        }
        if !cfg.tools.is_empty() {
            args.push("--tools".into());
            args.push(cfg.tools.clone());
        }
        if !cfg.thinking.is_empty() && cfg.thinking != "off" {
            args.push("--thinking".into());
            args.push(cfg.thinking.clone());
        }
        args.extend(cfg.extra_args.iter().cloned());

        info!("pi rpc spawn: {} {:?}", cfg.bin_path, args);

        let mut child = Command::new(&cfg.bin_path)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                format!(
                    "pi (RPC) konnte nicht gestartet werden: {e}\n\
                     Binary-Pfad in Settings → Pi prüfen."
                )
            })?;

        let stdin = child.stdin.take().ok_or("pi stdin nicht verfügbar")?;
        let stdout = child.stdout.take().ok_or("pi stdout nicht verfügbar")?;
        let stderr = child.stderr.take().ok_or("pi stderr nicht verfügbar")?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let turn_done: TurnSlot = Arc::new(Mutex::new(None));
        let collected: CollectBuf = Arc::new(Mutex::new(String::new()));
        let show_thinking = cfg.show_thinking;

        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let t = line.trim();
                        if !t.is_empty() {
                            warn!("pi rpc stderr: {t}");
                        }
                    }
                }
            }
        });

        let app_reader = app.clone();
        let pending_reader = pending.clone();
        let turn_reader = turn_done.clone();
        let collected_reader = collected.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut text_bytes: usize = 0;
            let mut thinking_bytes: usize = 0;
            let mut tool_calls: u32 = 0;
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        info!("pi rpc stdout closed");
                        break;
                    }
                    Err(e) => {
                        warn!("pi rpc stdout read error: {e}");
                        break;
                    }
                    Ok(_) => {}
                }
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if trimmed.is_empty() {
                    continue;
                }

                let msg: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        debug!("pi rpc non-JSON: {trimmed}");
                        continue;
                    }
                };
                let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

                if msg_type == "response" {
                    if let Some(id) = msg.get("id").and_then(|v| v.as_str()).map(str::to_string) {
                        if let Some(tx) = pending_reader.lock().await.remove(&id) {
                            let _ = tx.send(msg);
                        }
                    }
                    continue;
                }

                match msg_type {
                    "message_update" => {
                        let ev = &msg["assistantMessageEvent"];
                        let ev_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match ev_type {
                            "text_delta" => {
                                if let Some(delta) = ev.get("delta").and_then(|v| v.as_str()) {
                                    if !delta.is_empty() {
                                        text_bytes += delta.len();
                                        let _ = app_reader.emit(
                                            "chat-stream",
                                            StreamChunk {
                                                content: delta.to_string(),
                                                done: false,
                                            },
                                        );
                                        // Also fan the delta into the
                                        // shared collect buffer so non-
                                        // streaming callers (spam
                                        // analysis) can grab the full
                                        // text after agent_end.
                                        let mut buf = collected_reader.lock().await;
                                        buf.push_str(delta);
                                    }
                                }
                            }
                            "thinking_delta" => {
                                if let Some(delta) = ev.get("delta").and_then(|v| v.as_str()) {
                                    thinking_bytes += delta.len();
                                    if show_thinking && !delta.is_empty() {
                                        let _ = app_reader.emit("pi-thinking", delta.to_string());
                                    }
                                }
                            }
                            "toolcall_end" => {
                                tool_calls += 1;
                            }
                            _ => {}
                        }
                    }
                    "tool_execution_start" => {
                        if let Some(name) = msg.get("toolName").and_then(|v| v.as_str()) {
                            let _ = app_reader.emit("pi-tool", format!("▶ {name}"));
                        }
                    }
                    "agent_end" => {
                        info!(
                            "pi rpc turn done: text={}B thinking={}B tool_calls={}",
                            text_bytes, thinking_bytes, tool_calls
                        );
                        text_bytes = 0;
                        thinking_bytes = 0;
                        tool_calls = 0;
                        let _ = app_reader.emit(
                            "chat-stream",
                            StreamChunk {
                                content: String::new(),
                                done: true,
                            },
                        );
                        if let Some(tx) = turn_reader.lock().await.take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    "extension_error" => {
                        warn!("pi rpc extension_error: {msg}");
                    }
                    "auto_retry_start" | "auto_retry_end" => {
                        // These are the prime suspect for "Dauerschleife":
                        // pi's agent framework decides the turn isn't done
                        // and retries. Info-level so it shows up in the
                        // default log without needing RUST_LOG=debug.
                        info!("pi rpc {msg_type}: {msg}");
                    }
                    _ => {
                        debug!("pi rpc event: {msg_type}");
                    }
                }
            }

            let _ = app_reader.emit(
                "chat-stream",
                StreamChunk {
                    content: String::new(),
                    done: true,
                },
            );
            if let Some(tx) = turn_reader.lock().await.take() {
                let _ = tx.send(Err("pi rpc-Prozess wurde beendet".into()));
            }
        });

        Ok(Arc::new(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            turn_done,
            collected,
            turn_gate: Mutex::new(()),
            next_id: AtomicU64::new(1),
            fingerprint: fp,
        }))
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    async fn send_command(&self, mut cmd: Value) -> Result<Value, String> {
        let id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        if let Some(obj) = cmd.as_object_mut() {
            obj.insert("id".to_string(), Value::String(id.clone()));
        }
        let mut serialized = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;
        serialized.push('\n');

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(serialized.as_bytes())
                .await
                .map_err(|e| format!("pi rpc write: {e}"))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("pi rpc flush: {e}"))?;
        }

        rx.await
            .map_err(|_| "pi rpc-Prozess antwortet nicht (Kanal geschlossen)".to_string())
    }

    /// Submit a user prompt and wait until the agent finishes the turn
    /// (i.e. `agent_end` arrives or the process dies). Streams the
    /// response via the `chat-stream` event as before.
    pub async fn prompt(&self, message: String) -> Result<(), String> {
        // Single-turn gate: pi-rpc can't interleave two concurrent
        // prompts without scrambling their streams. Second caller waits.
        let _gate = self.turn_gate.lock().await;
        // Reset the collect buffer even though no-one will read it here —
        // keeps state consistent for the next `prompt_collect` call.
        self.collected.lock().await.clear();
        self.submit_and_wait(message).await
    }

    /// Submit a prompt and return the full assistant text once the turn
    /// finishes. Stream events still fire (so a visible chat terminal
    /// updates in parallel) — the collect buffer is a side-channel.
    pub async fn prompt_collect(&self, message: String) -> Result<String, String> {
        let _gate = self.turn_gate.lock().await;
        self.collected.lock().await.clear();
        self.submit_and_wait(message).await?;
        Ok(self.collected.lock().await.clone())
    }

    /// Submit a prompt and return as soon as the collect buffer contains
    /// a balanced top-level JSON object — whichever happens first,
    /// `agent_end` or JSON-complete. Purpose-built for the spam-analysis
    /// flow where pi's own "am I done" signal is unreliable: gemma in
    /// agent mode tends to regenerate the same response two or three
    /// times before emitting `agent_end`, burning minutes each round
    /// even though the useful JSON arrived after the first pass.
    ///
    /// Caller is expected to drop `Self` right after — we leave the
    /// pending turn/token-stream in place intentionally, and rely on
    /// `kill_on_drop` at the child-process boundary to stop pi dead.
    pub async fn prompt_collect_json(&self, message: String) -> Result<String, String> {
        // Backwards-compatible wrapper: the spam-analysis schema
        // always returns a top-level `rules` array, so we keep
        // hard-coding that key for legacy call sites. New callers
        // should use `prompt_collect_until_key` directly.
        self.prompt_collect_until_key(message, "rules").await
    }

    /// Like `prompt_collect_json` but parametrised on the top-level
    /// JSON key the schema promises to produce (`rules` for spam,
    /// `predicates` for workflow-rule training, …). Without this
    /// parameter, the detector would match a predicate-schema
    /// response only after its 120-second timeout — appearing as a
    /// hung dialog to the user.
    pub async fn prompt_collect_until_key(
        &self,
        message: String,
        sentinel_key: &'static str,
    ) -> Result<String, String> {
        let _gate = self.turn_gate.lock().await;
        self.collected.lock().await.clear();

        let (done_tx, done_rx) = oneshot::channel();
        *self.turn_done.lock().await = Some(done_tx);

        let resp = self
            .send_command(json!({ "type": "prompt", "message": message }))
            .await?;
        if !resp
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let _ = self.turn_done.lock().await.take();
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("prompt abgelehnt");
            return Err(format!("pi rpc: {err}"));
        }

        // Poller: wakes every 200 ms, walks every balanced `{...}` in
        // the collected buffer looking for one that parses AND
        // carries `sentinel_key` as a top-level field. Stricter than
        // "first valid JSON" so a gemma preamble like
        // `{"analysis":"..."}` doesn't trigger an early return on
        // the wrong object. The sentinel value is accepted as array
        // or object — the caller decides what the key's payload
        // looks like; we just need to know the key is there.
        let collected_poll = self.collected.clone();
        let json_detector = async move {
            let mut last_size = 0usize;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let snapshot = {
                    let buf = collected_poll.lock().await;
                    buf.clone()
                };
                if snapshot.len() != last_size {
                    debug!(
                        "pi rpc json_detector[{sentinel_key}]: collected={} bytes",
                        snapshot.len()
                    );
                    last_size = snapshot.len();
                }
                let mut found_size: Option<usize> = None;
                for obj in iter_balanced_json(&snapshot) {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(obj) else {
                        continue;
                    };
                    if value.get(sentinel_key).is_some() {
                        found_size = Some(obj.len());
                        break;
                    }
                }
                if let Some(size) = found_size {
                    info!(
                        "pi rpc json_detector[{sentinel_key}]: matching object found ({size} bytes)"
                    );
                    return snapshot;
                }
            }
        };

        tokio::select! {
            result = done_rx => {
                // Agent signalled completion first — use whatever's in the buffer.
                result.map_err(|_| "pi rpc turn-done Kanal geschlossen".to_string())??;
                Ok(self.collected.lock().await.clone())
            }
            buf = json_detector => {
                info!("pi rpc: early-exit on balanced JSON (skipped pi's own turn-end)");
                // A late agent_end would try to fire on a dropped sender and panic;
                // take the slot so it's a no-op.
                let _ = self.turn_done.lock().await.take();
                Ok(buf)
            }
        }
    }

    /// Forcibly terminate the child pi process. Called by the cancel
    /// path of `suggest_spam_rules` when the user hits the Abort button
    /// in the learn-dialog; collapsing the child closes stdin/stdout
    /// which in turn breaks the reader loop and unblocks any pending
    /// `done_rx`. Idempotent — multiple calls after the first are
    /// cheap no-ops.
    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }

    /// Current snapshot of the collected text buffer. Used by the
    /// timeout fallback so we can show the user whatever pi did manage
    /// to produce even when the structured-JSON detector didn't match.
    pub async fn collected_snapshot(&self) -> String {
        self.collected.lock().await.clone()
    }

    async fn submit_and_wait(&self, message: String) -> Result<(), String> {
        let (done_tx, done_rx) = oneshot::channel();
        *self.turn_done.lock().await = Some(done_tx);

        let resp = self
            .send_command(json!({ "type": "prompt", "message": message }))
            .await?;

        if !resp
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let _ = self.turn_done.lock().await.take();
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("prompt abgelehnt");
            return Err(format!("pi rpc: {err}"));
        }

        done_rx
            .await
            .map_err(|_| "pi rpc turn-done Kanal geschlossen".to_string())?
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────
//
// `iter_balanced_json` ist die Stelle, an der pi-Output (Text-Stream mit
// teils mehreren JSON-Objekten am Stück, gemischt mit Klartext) in
// einzelne JSON-Kandidaten zerteilt wird. Streaming-Output kann eine
// Mail-Filter-Antwort als zwei aufeinanderfolgende Objekte ausspielen
// (`{"analysis":"..."}` als Thinking + `{"rules":[...]}` als Ergebnis),
// und der Caller will über alle iterieren. Eine gut-getestete Iterator-
// Logik hier spart später Stunden Debugging-Zeit, wenn eine pi-Variante
// das Format umstrukturiert.
//
// Bekannte Einschränkung (im doc-comment dokumentiert): naiver Brace-
// Counter, ignoriert String-Escapes. Tests auf den dokumentierten
// Fehlerfall machen wir bewusst nicht — `serde_json::from_str` beim
// Caller kümmert sich darum.
#[cfg(test)]
mod tests {
    use super::*;

    fn collect(raw: &str) -> Vec<&str> {
        iter_balanced_json(raw).collect()
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(collect("").is_empty());
    }

    #[test]
    fn whitespace_only_yields_nothing() {
        assert!(collect("   \n\t  ").is_empty());
    }

    #[test]
    fn no_braces_yields_nothing() {
        assert!(collect("kein json hier, nur Text").is_empty());
    }

    #[test]
    fn single_flat_object() {
        let raw = r#"{"a":1}"#;
        assert_eq!(collect(raw), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn surrounding_text_is_ignored() {
        // pi gibt manchmal "Hier ist die Antwort: {...}" aus.
        let raw = r#"Hier ist die Antwort: {"a":1} — fertig."#;
        assert_eq!(collect(raw), vec![r#"{"a":1}"#]);
    }

    /// Der Hauptgrund für den Iterator: gemma's "Thinking + Antwort"-
    /// Doppel-JSON-Pattern.
    #[test]
    fn two_consecutive_objects() {
        let raw = r#"{"analysis":"das sieht spammy aus"}{"rules":[]}"#;
        let out = collect(raw);
        assert_eq!(
            out,
            vec![
                r#"{"analysis":"das sieht spammy aus"}"#,
                r#"{"rules":[]}"#,
            ]
        );
    }

    #[test]
    fn objects_separated_by_text() {
        let raw = r#"first {"a":1} middle text {"b":2} trailing"#;
        let out = collect(raw);
        assert_eq!(out, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    /// Verschachtelte Objekte (Sub-Objects) müssen als ein einziger Top-
    /// Level-Block kommen, nicht in zwei zerlegt werden.
    #[test]
    fn nested_objects_treated_as_one_top_level() {
        let raw = r#"{"outer":{"inner":1}}"#;
        assert_eq!(collect(raw), vec![r#"{"outer":{"inner":1}}"#]);
    }

    #[test]
    fn deeply_nested_balances_correctly() {
        let raw = r#"{"a":{"b":{"c":{"d":1}}}}"#;
        let out = collect(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], raw);
    }

    /// Ein noch wachsender Buffer (Stream noch nicht fertig) liefert
    /// keine Ergebnisse — der Caller versucht's bei der nächsten Token-
    /// Gruppe nochmal.
    #[test]
    fn unbalanced_open_yields_nothing() {
        let raw = r#"{"a":1"#;
        assert!(collect(raw).is_empty());
    }

    /// Ein vollständig schließendes plus ein offenes Objekt: wir geben
    /// das vollständige aus und stoppen am noch offenen — beim nächsten
    /// Buffer-Wachstum probiert der Caller erneut.
    #[test]
    fn complete_then_unbalanced() {
        let raw = r#"{"a":1}{"b":2"#;
        assert_eq!(collect(raw), vec![r#"{"a":1}"#]);
    }

    /// Whitespace + Newlines zwischen Objekten passieren regelmäßig im
    /// Streaming-Output (LLM emittet Token mit Leerzeichen/Newlines an
    /// Block-Grenzen). Müssen ignoriert werden.
    #[test]
    fn whitespace_between_objects_ignored() {
        let raw = "{\"a\":1}\n  \n{\"b\":2}";
        let out = collect(raw);
        assert_eq!(out, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    /// Realistisches pi-Output-Beispiel: kurzer Reasoning-Vorlauf, dann
    /// das eigentliche `rules`-Objekt.
    #[test]
    fn realistic_pi_output() {
        let raw = r#"
            Ich analysiere die 5 markierten Mails.
            {"analysis":"alle haben '***SPAM***' im Betreff"}
            Hier die Regel-Vorschläge:
            {"rules":[{"pattern_type":"subject_contains","pattern":"***SPAM***"}]}
        "#;
        let out = collect(raw);
        assert_eq!(out.len(), 2, "must yield both analysis + rules blocks");
        assert!(out[0].contains("analysis"));
        assert!(out[1].contains("rules"));
        // Jeder Kandidat soll für sich allein parsbar sein — das ist
        // die downstream-Erwartung.
        for s in &out {
            serde_json::from_str::<serde_json::Value>(s)
                .expect("each candidate must be valid JSON");
        }
    }

    /// Empty object — degenerate but valid input from a pedantic LLM.
    #[test]
    fn empty_object() {
        let raw = "{}";
        assert_eq!(collect(raw), vec!["{}"]);
    }

    /// Multiple empty objects in a row — exercises the "advance cursor
    /// past a finished object" path with minimal content.
    #[test]
    fn multiple_empty_objects() {
        let raw = "{}{}{}";
        assert_eq!(collect(raw), vec!["{}", "{}", "{}"]);
    }
}
