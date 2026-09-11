mod integrations;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use tauri_plugin_notification::NotificationExt;

const PORT: u16 = 7878;
const WORKING_STALE_MS: u64 = 10 * 60 * 1000;
const QUIET_STALE_MS: u64 = 5 * 60 * 1000;
const ZCODE_POLL_MS: u64 = 250;
const ZCODE_LOG_POLL_MS: u64 = 120;

#[derive(Clone, Serialize)]
struct SessionInfo {
    id: String,
    source: String,
    name: String,
    state: String,
    detail: String,
    updated_ms: u64,
    created_ms: u64,
    needs_ack: bool,
    #[serde(skip_serializing)]
    activity_id: Option<String>,
    #[serde(skip_serializing)]
    monitor_waiting: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MutedSession {
    activity_id: Option<String>,
    muted_ms: u64,
}

#[derive(Default)]
struct Store {
    sessions: HashMap<String, SessionInfo>,
    muted: HashMap<String, MutedSession>,
}

impl Store {
    fn aggregate(&self) -> &'static str {
        for priority in ["waiting", "error", "done"] {
            if self
                .sessions
                .values()
                .any(|session| session.needs_ack && session.state == priority)
            {
                return priority;
            }
        }
        if self.has_working() {
            "working"
        } else if self.sessions.is_empty() {
            "sleeping"
        } else {
            "idle"
        }
    }

    fn has_working(&self) -> bool {
        self.sessions
            .values()
            .any(|session| session.state == "working")
    }

    fn pending_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| session.needs_ack)
            .count()
    }

    fn payload(&self) -> serde_json::Value {
        let mut sessions: Vec<SessionInfo> = self.sessions.values().cloned().collect();
        sessions.sort_by_key(|session| session.created_ms);
        serde_json::json!({
            "aggregate": self.aggregate(),
            "sessions": sessions,
            "has_working": self.has_working(),
            "pending_count": self.pending_count(),
        })
    }
}

#[derive(Deserialize)]
struct IncomingEvent {
    source: Option<String>,
    kind: Option<String>,
    hook_event_name: Option<String>,
    session_id: Option<String>,
    session_name: Option<String>,
    message: Option<String>,
    prompt: Option<String>,
    cwd: Option<String>,
    tool_name: Option<String>,
    notification_type: Option<String>,
    transcript_path: Option<String>,
    activity_id: Option<String>,
}

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<Store>>,
    app: AppHandle,
}

#[derive(Clone)]
struct CodexTurn {
    turn_id: String,
    thread_id: String,
    name: String,
    started_ms: u64,
    waiting_detail: Option<String>,
}

#[derive(Clone)]
struct ZcodeTurn {
    turn_id: String,
    session_id: String,
    name: String,
    started_ms: u64,
    waiting_detail: Option<String>,
}

#[derive(Clone)]
struct ZcodeCompletedTurn {
    turn_id: String,
    session_id: String,
    name: String,
    started_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ZcodeLogEvent {
    kind: String,
    session_id: String,
    turn_id: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn muted_sessions_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|directory| directory.join("muted-sessions.json"))
}

fn load_muted_sessions(app: &AppHandle) -> HashMap<String, MutedSession> {
    let Some(path) = muted_sessions_path(app) else {
        return HashMap::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn persist_muted_sessions(app: &AppHandle, muted: &HashMap<String, MutedSession>) {
    let Some(path) = muted_sessions_path(app) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("failed to create mute state directory: {error}");
            return;
        }
    }
    match serde_json::to_string_pretty(muted) {
        Ok(serialized) => {
            if let Err(error) = std::fs::write(path, format!("{serialized}\n")) {
                eprintln!("failed to persist muted sessions: {error}");
            }
        }
        Err(error) => eprintln!("failed to serialize muted sessions: {error}"),
    }
}

fn clean_session_name(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect()
}

fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_millis(250))?;
    Ok(connection)
}

fn opencode_db_path() -> Option<PathBuf> {
    let data_root = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| integrations::user_home().map(|home| home.join(".local").join("share")))?;
    let path = data_root.join("opencode").join("opencode.db");
    path.exists().then_some(path)
}

fn read_opencode_session_name(session_id: &str) -> Option<String> {
    let connection = open_read_only(&opencode_db_path()?).ok()?;
    let title: Option<String> = connection
        .query_row(
            "SELECT title FROM session WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .ok()?;
    title
        .map(|value| clean_session_name(&value))
        .filter(|value| !value.is_empty())
}

fn read_claude_session_name(transcript_path: &str) -> Option<String> {
    let projects_root = integrations::user_home()?
        .join(".claude")
        .join("projects")
        .canonicalize()
        .ok()?;
    let transcript = PathBuf::from(transcript_path).canonicalize().ok()?;
    if !transcript.starts_with(projects_root)
        || transcript.extension().and_then(|value| value.to_str()) != Some("jsonl")
    {
        return None;
    }

    let reader = BufReader::new(std::fs::File::open(transcript).ok()?);
    let mut title = None;
    for line in reader.lines().map_while(Result::ok) {
        if !line.contains("\"custom-title\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value["type"] == "custom-title" {
            if let Some(raw) = value["customTitle"].as_str() {
                let candidate = clean_session_name(raw);
                if !candidate.is_empty() {
                    title = Some(candidate);
                }
            }
        }
    }
    title
}

fn resolve_session_name(
    event: &IncomingEvent,
    source: &str,
    kind: &str,
    previous: Option<&SessionInfo>,
) -> String {
    if let Some(name) = event.session_name.as_deref() {
        let name = clean_session_name(name);
        if !name.is_empty() {
            return name;
        }
    }

    let previous_name = previous
        .map(|session| session.name.as_str())
        .unwrap_or_default();
    let should_refresh = previous_name.is_empty() || needs_attention(kind) || kind == "idle";

    if source == "opencode" && should_refresh {
        if let Some(name) =
            read_opencode_session_name(event.session_id.as_deref().unwrap_or("opencode-default"))
        {
            return name;
        }
    }

    if source == "claude" && should_refresh {
        if let Some(name) = event
            .transcript_path
            .as_deref()
            .and_then(read_claude_session_name)
        {
            return name;
        }
    }

    if !previous_name.is_empty() {
        return previous_name.to_string();
    }

    event
        .prompt
        .as_deref()
        .map(clean_session_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_default()
}

fn is_question_tool(tool_name: Option<&str>) -> bool {
    let Some(tool_name) = tool_name else {
        return false;
    };
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "askuserquestion"
            | "ask_user_question"
            | "request_user_input"
            | "requestuserinput"
            | "elicitation"
    )
}

fn normalize_kind(event: &IncomingEvent) -> Option<String> {
    let raw = event
        .kind
        .clone()
        .or_else(|| event.hook_event_name.clone())?;
    let mapped = match raw.as_str() {
        "SessionStart" => "idle",
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "ElicitationResult"
        | "user_activity" => "working",
        "PreToolUse" if is_question_tool(event.tool_name.as_deref()) => "waiting",
        "PreToolUse" => "working",
        "PermissionRequest" | "Elicitation" => "waiting",
        "Notification"
            if matches!(
                event.notification_type.as_deref(),
                Some("auth_success" | "elicitation_complete" | "elicitation_response")
            ) =>
        {
            "working"
        }
        "Notification"
            if matches!(
                event.notification_type.as_deref(),
                Some("permission_prompt" | "elicitation_dialog")
            ) =>
        {
            "waiting"
        }
        "Notification" => return None,
        "Stop" => "done",
        "StopFailure" => "error",
        "SessionEnd" => "session_end",
        "working" | "idle" | "done" | "error" | "waiting" | "session_end" => return Some(raw),
        _ => return None,
    };
    Some(mapped.to_string())
}

fn is_user_activity(event: &IncomingEvent) -> bool {
    event.kind.as_deref() == Some("user_activity")
        || event.hook_event_name.as_deref() == Some("UserPromptSubmit")
}

fn event_detail(event: &IncomingEvent, kind: &str) -> String {
    let detail = if let Some(message) = event.message.as_ref().filter(|message| !message.is_empty())
    {
        message.clone()
    } else if kind == "waiting" {
        match (event.hook_event_name.as_deref(), event.tool_name.as_deref()) {
            (Some("PreToolUse"), tool_name) if is_question_tool(tool_name) => {
                "Agent 正在等你回答".to_string()
            }
            (Some("PermissionRequest"), _) => "需要你确认权限".to_string(),
            (Some("Elicitation"), _) => "需要你完成交互".to_string(),
            _ => "等待你的输入".to_string(),
        }
    } else {
        event
            .tool_name
            .clone()
            .or_else(|| event.cwd.clone())
            .unwrap_or_default()
    };
    detail.chars().take(80).collect()
}

fn needs_attention(kind: &str) -> bool {
    matches!(kind, "done" | "waiting" | "error")
}

fn source_label(source: &str) -> &str {
    match source {
        "opencode" => "OpenCode",
        "codex" => "Codex",
        "zcode" => "ZCode",
        _ => "Claude Code",
    }
}

fn emit_payload(app: &AppHandle, payload: &serde_json::Value) {
    let _ = app.emit("pet-state", payload.clone());
}

fn show_attention_notification(app: &AppHandle, source: &str, kind: &str, detail: &str) {
    let label = source_label(source);
    let body = if detail.is_empty() {
        label.to_string()
    } else {
        format!("{label} · {detail}")
    };
    let builder = app.notification().builder();
    let _ = match kind {
        "waiting" => builder.title("小狗在等你 🐾").body(body).show(),
        "error" => builder.title("小狗：需要检查 😵").body(body).show(),
        _ => builder.title("小狗：干完啦！🎉").body(body).show(),
    };
}

async fn handle_event(
    State(state): State<AppState>,
    Json(event): Json<IncomingEvent>,
) -> StatusCode {
    let source = event.source.clone().unwrap_or_else(|| "claude".to_string());
    let Some(kind) = normalize_kind(&event) else {
        return StatusCode::OK;
    };
    let session_id = event
        .session_id
        .clone()
        .unwrap_or_else(|| format!("{source}-default"));
    let key = format!("{source}:{session_id}");
    let muted_snapshot = {
        let mut store = state.store.lock().unwrap();
        if store.muted.contains_key(&key) {
            if is_user_activity(&event) {
                store.muted.remove(&key);
                Some(store.muted.clone())
            } else {
                return StatusCode::OK;
            }
        } else {
            None
        }
    };
    if let Some(muted) = muted_snapshot {
        persist_muted_sessions(&state.app, &muted);
    }
    let incoming_detail = event_detail(&event, &kind);
    let now = now_ms();

    let previous_snapshot = state.store.lock().unwrap().sessions.get(&key).cloned();
    let resolved_name = resolve_session_name(&event, &source, &kind, previous_snapshot.as_ref());

    let mut store = state.store.lock().unwrap();
    let previous = store.sessions.get(&key).cloned();
    let activity_id = event.activity_id.clone().or_else(|| {
        previous
            .as_ref()
            .and_then(|session| session.activity_id.clone())
    });
    let name = if resolved_name.is_empty() {
        previous
            .as_ref()
            .map(|session| session.name.clone())
            .unwrap_or_default()
    } else {
        resolved_name
    };
    let mut became_attention = false;

    if kind == "session_end" {
        if !previous.as_ref().is_some_and(|session| session.needs_ack) {
            store.sessions.remove(&key);
        }
    } else {
        let created_ms = match &previous {
            Some(session) if kind == "working" && session.state != "working" => now,
            Some(session) => session.created_ms,
            None => now,
        };
        let detail = if incoming_detail.is_empty() {
            previous
                .as_ref()
                .map(|session| session.detail.clone())
                .unwrap_or_default()
        } else {
            incoming_detail
        };
        let attention = needs_attention(&kind);
        became_attention = attention
            && previous
                .as_ref()
                .is_none_or(|session| !session.needs_ack || session.state != kind);

        store.sessions.insert(
            key,
            SessionInfo {
                id: session_id.clone(),
                source: source.clone(),
                name,
                state: kind.clone(),
                detail,
                updated_ms: now,
                created_ms,
                needs_ack: attention,
                activity_id,
                monitor_waiting: false,
            },
        );
    }

    let payload = store.payload();
    drop(store);
    emit_payload(&state.app, &payload);

    if became_attention {
        let detail = payload["sessions"]
            .as_array()
            .and_then(|sessions| {
                sessions
                    .iter()
                    .find(|session| session["source"] == source && session["id"] == session_id)
            })
            .and_then(|session| session["detail"].as_str())
            .unwrap_or_default();
        show_attention_notification(&state.app, &source, &kind, detail);
    }

    StatusCode::OK
}

#[tauri::command]
fn get_state(store: tauri::State<'_, Arc<Mutex<Store>>>) -> serde_json::Value {
    store.lock().unwrap().payload()
}

#[tauri::command]
fn acknowledge_done(
    app: AppHandle,
    store: tauri::State<'_, Arc<Mutex<Store>>>,
) -> serde_json::Value {
    let mut store = store.lock().unwrap();
    let now = now_ms();
    for session in store.sessions.values_mut() {
        if session.needs_ack && session.state == "done" {
            session.needs_ack = false;
            session.updated_ms = now;
        }
    }
    let payload = store.payload();
    drop(store);
    emit_payload(&app, &payload);
    payload
}

#[tauri::command]
fn dismiss_session(
    app: AppHandle,
    store: tauri::State<'_, Arc<Mutex<Store>>>,
    source: String,
    session_id: String,
) -> serde_json::Value {
    let mut store = store.lock().unwrap();
    store.sessions.remove(&format!("{source}:{session_id}"));
    let payload = store.payload();
    drop(store);
    emit_payload(&app, &payload);
    payload
}

#[tauri::command]
fn mute_session(
    app: AppHandle,
    store: tauri::State<'_, Arc<Mutex<Store>>>,
    source: String,
    session_id: String,
) -> serde_json::Value {
    let key = format!("{source}:{session_id}");
    let mut store = store.lock().unwrap();
    let activity_id = store
        .sessions
        .get(&key)
        .and_then(|session| session.activity_id.clone());
    store.sessions.remove(&key);
    store.muted.insert(
        key,
        MutedSession {
            activity_id,
            muted_ms: now_ms(),
        },
    );
    let muted = store.muted.clone();
    let payload = store.payload();
    drop(store);
    persist_muted_sessions(&app, &muted);
    emit_payload(&app, &payload);
    payload
}

#[tauri::command]
fn focus_session_window(source: String) -> bool {
    focus_window_for_source(&source)
}

fn codex_history_path() -> Option<PathBuf> {
    let root = integrations::codex_home()?;
    let path = root.join("thread_history_1.sqlite");
    path.exists().then_some(path)
}

fn codex_state_path() -> Option<PathBuf> {
    let root = integrations::codex_home()?;
    let path = root.join("state_5.sqlite");
    path.exists().then_some(path)
}

fn read_codex_waiting_detail(rollout_path: &Path, active_turn_id: &str) -> Option<String> {
    let reader = BufReader::new(std::fs::File::open(rollout_path).ok()?);
    let mut current_turn = String::new();
    let mut pending: Vec<(String, String)> = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let payload = &value["payload"];
        if value["type"] == "turn_context" {
            current_turn = payload["turn_id"].as_str().unwrap_or_default().to_string();
            continue;
        }
        if current_turn != active_turn_id || value["type"] != "response_item" {
            continue;
        }

        match payload["type"].as_str().unwrap_or_default() {
            "function_call" | "custom_tool_call" => {
                let name = payload["name"].as_str().unwrap_or_default();
                let detail = match name {
                    "request_user_input" => Some("Agent 正在等你回答"),
                    "request_permissions" => Some("需要你确认权限"),
                    _ => None,
                };
                let call_id = payload["call_id"]
                    .as_str()
                    .or_else(|| payload["id"].as_str())
                    .unwrap_or_default();
                if let (Some(detail), false) = (detail, call_id.is_empty()) {
                    pending.retain(|(id, _)| id != call_id);
                    pending.push((call_id.to_string(), detail.to_string()));
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = payload["call_id"]
                    .as_str()
                    .or_else(|| payload["id"].as_str())
                    .unwrap_or_default();
                pending.retain(|(id, _)| id != call_id);
            }
            _ => {}
        }
    }

    pending.last().map(|(_, detail)| detail.clone())
}

fn read_active_codex_turns(
    history_path: &Path,
    state_path: Option<&Path>,
) -> rusqlite::Result<Vec<CodexTurn>> {
    let connection = open_read_only(history_path)?;
    let mut statement = connection.prepare(
        "SELECT turn_id, thread_id, started_at
         FROM thread_turns
         WHERE status = 'inProgress'",
    )?;
    let mut turns = statement
        .query_map([], |row| {
            let started_at: Option<i64> = row.get(2)?;
            Ok(CodexTurn {
                turn_id: row.get(0)?,
                thread_id: row.get(1)?,
                name: String::new(),
                started_ms: started_at.unwrap_or_default().max(0) as u64 * 1000,
                waiting_detail: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if let Some(state_path) = state_path {
        if let Ok(state) = open_read_only(state_path) {
            if let Ok(mut title_query) = state.prepare(
                "SELECT COALESCE(
                    NULLIF(name, ''),
                    NULLIF(title, ''),
                    NULLIF(preview, ''),
                    NULLIF(first_user_message, ''),
                    ''
                 ), rollout_path FROM threads WHERE id = ?1",
            ) {
                for turn in &mut turns {
                    let thread: Option<(String, String)> = title_query
                        .query_row([&turn.thread_id], |row| Ok((row.get(0)?, row.get(1)?)))
                        .optional()
                        .unwrap_or(None);
                    if let Some((raw_name, rollout_path)) = thread {
                        turn.name = clean_session_name(&raw_name);
                        turn.waiting_detail =
                            read_codex_waiting_detail(Path::new(&rollout_path), &turn.turn_id);
                    }
                }
            }
        }
    }
    Ok(turns)
}

fn sync_codex_turns(
    store: &mut Store,
    previous: &mut HashMap<String, CodexTurn>,
    current: Vec<CodexTurn>,
) -> (bool, usize, usize, bool) {
    let now = now_ms();
    let current_ids: HashSet<&str> = current.iter().map(|turn| turn.turn_id.as_str()).collect();
    let active_threads: HashSet<&str> =
        current.iter().map(|turn| turn.thread_id.as_str()).collect();
    let mut changed = false;
    let mut completed = 0;
    let mut waiting = 0;
    let mut mutes_changed = false;

    for turn in &current {
        let key = format!("codex:{}", turn.thread_id);
        if let Some(muted_activity_id) =
            store.muted.get(&key).map(|muted| muted.activity_id.clone())
        {
            if muted_activity_id
                .as_deref()
                .is_some_and(|id| id != turn.turn_id)
            {
                store.muted.remove(&key);
                mutes_changed = true;
            } else {
                continue;
            }
        }
        let created_ms = if turn.started_ms == 0 {
            now
        } else {
            turn.started_ms
        };
        let hook_waiting = store.sessions.get(&key).is_some_and(|session| {
            session.state == "waiting"
                && session.needs_ack
                && !session.monitor_waiting
                && turn.waiting_detail.is_none()
                && session
                    .activity_id
                    .as_deref()
                    .is_none_or(|activity_id| activity_id == turn.turn_id)
        });
        let target_waiting = turn.waiting_detail.is_some() || hook_waiting;
        let target_state = if target_waiting { "waiting" } else { "working" };
        let target_detail = turn.waiting_detail.as_deref().unwrap_or_else(|| {
            if hook_waiting {
                store
                    .sessions
                    .get(&key)
                    .map(|session| session.detail.as_str())
                    .unwrap_or("等待你的输入")
            } else {
                "Codex 正在运行"
            }
        });
        let became_waiting = target_waiting
            && store
                .sessions
                .get(&key)
                .is_none_or(|session| session.state != "waiting" || !session.needs_ack);
        let needs_update = store.sessions.get(&key).is_none_or(|session| {
            session.state != target_state
                || session.name != turn.name
                || session.detail != target_detail
                || session.created_ms != created_ms
                || session.needs_ack != target_waiting
                || session.activity_id.as_deref() != Some(turn.turn_id.as_str())
                || session.monitor_waiting != turn.waiting_detail.is_some()
        });
        if needs_update {
            store.sessions.insert(
                key,
                SessionInfo {
                    id: turn.thread_id.clone(),
                    source: "codex".to_string(),
                    name: turn.name.clone(),
                    state: target_state.to_string(),
                    detail: target_detail.to_string(),
                    updated_ms: now,
                    created_ms,
                    needs_ack: target_waiting,
                    activity_id: Some(turn.turn_id.clone()),
                    monitor_waiting: turn.waiting_detail.is_some(),
                },
            );
            changed = true;
        } else if let Some(session) = store.sessions.get_mut(&key) {
            session.updated_ms = now;
        }
        if became_waiting {
            waiting += 1;
        }
    }

    for turn in previous.values() {
        if current_ids.contains(turn.turn_id.as_str())
            || active_threads.contains(turn.thread_id.as_str())
        {
            continue;
        }
        let key = format!("codex:{}", turn.thread_id);
        if store.muted.contains_key(&key) {
            continue;
        }
        if let Some(session) = store.sessions.get_mut(&key) {
            if matches!(session.state.as_str(), "working" | "waiting") {
                session.state = "done".to_string();
                session.detail = "任务已完成".to_string();
                session.updated_ms = now;
                session.needs_ack = true;
                changed = true;
                completed += 1;
            }
        }
    }

    previous.clear();
    previous.extend(current.into_iter().map(|turn| (turn.turn_id.clone(), turn)));
    (changed, completed, waiting, mutes_changed)
}

async fn run_codex_monitor(app: AppHandle, store: Arc<Mutex<Store>>) {
    let Some(path) = codex_history_path() else {
        return;
    };
    let state_path = codex_state_path();
    let mut previous = HashMap::new();

    loop {
        let query_path = path.clone();
        let query_state_path = state_path.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            read_active_codex_turns(&query_path, query_state_path.as_deref())
        })
        .await;
        if let Ok(Ok(current)) = result {
            let mut state = store.lock().unwrap();
            let (changed, completed, waiting, mutes_changed) =
                sync_codex_turns(&mut state, &mut previous, current);
            let payload = changed.then(|| state.payload());
            let muted = mutes_changed.then(|| state.muted.clone());
            drop(state);
            if let Some(muted) = muted {
                persist_muted_sessions(&app, &muted);
            }
            if let Some(payload) = payload {
                emit_payload(&app, &payload);
            }
            if completed > 0 {
                let detail = if completed == 1 {
                    "任务已完成".to_string()
                } else {
                    format!("{completed} 个任务已完成")
                };
                show_attention_notification(&app, "codex", "done", &detail);
            }
            if waiting > 0 {
                let detail = if waiting == 1 {
                    "等待你的输入".to_string()
                } else {
                    format!("{waiting} 个任务在等待输入")
                };
                show_attention_notification(&app, "codex", "waiting", &detail);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
}

fn zcode_db_path() -> Option<PathBuf> {
    let path = integrations::user_home()?
        .join(".zcode")
        .join("cli")
        .join("db")
        .join("db.sqlite");
    path.exists().then_some(path)
}

fn latest_zcode_log_path() -> Option<PathBuf> {
    let directory = integrations::user_home()?
        .join(".zcode")
        .join("cli")
        .join("log");
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("zcode-") || !name.ends_with(".jsonl") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn parse_zcode_log_event(line: &str) -> Option<ZcodeLogEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = match value["event"].as_str()? {
        "turn.started" => "working",
        "turn.completed" => "done",
        "turn.failed" => "error",
        _ => return None,
    };
    let session_id = value["sessionId"]
        .as_str()
        .or_else(|| value["session_id"].as_str())?;
    let turn_id = value["turnId"]
        .as_str()
        .or_else(|| value["turn_id"].as_str())?;
    Some(ZcodeLogEvent {
        kind: kind.to_string(),
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
    })
}

fn read_appended_zcode_log_events(
    path: &Path,
    offset: &mut u64,
) -> std::io::Result<Vec<ZcodeLogEvent>> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    if *offset > length {
        *offset = 0;
    }
    file.seek(SeekFrom::Start(*offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(Vec::new());
    };
    let consumed = last_newline + 1;
    *offset += consumed as u64;
    let text = String::from_utf8_lossy(&bytes[..consumed]);
    Ok(text.lines().filter_map(parse_zcode_log_event).collect())
}

fn read_zcode_session_name(path: &Path, session_id: &str) -> rusqlite::Result<Option<String>> {
    let connection = open_read_only(path)?;
    connection
        .query_row(
            "SELECT COALESCE(title, '') FROM session WHERE id = ?1",
            [session_id],
            |row| {
                let raw: String = row.get(0)?;
                Ok(clean_session_name(&raw))
            },
        )
        .optional()
}

fn sync_zcode_log_event(
    store: &mut Store,
    event: &ZcodeLogEvent,
    resolved_name: Option<&str>,
) -> (bool, Option<&'static str>, bool) {
    let key = format!("zcode:{}", event.session_id);
    let mut mutes_changed = false;
    if let Some(muted_activity_id) = store.muted.get(&key).map(|muted| muted.activity_id.clone()) {
        if muted_activity_id.as_deref() == Some(event.turn_id.as_str()) {
            return (false, None, false);
        }
        store.muted.remove(&key);
        mutes_changed = true;
    }

    let now = now_ms();
    let previous = store.sessions.get(&key).cloned();
    let name = resolved_name
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| previous.as_ref().map(|session| session.name.clone()))
        .unwrap_or_else(|| "ZCode 桌面端会话".to_string());
    let (detail, needs_ack) = match event.kind.as_str() {
        "working" => ("ZCode 正在运行", false),
        "error" => ("任务运行失败", true),
        _ => ("任务已完成", true),
    };
    let already_current = previous.as_ref().is_some_and(|session| {
        session.state == event.kind
            && session.needs_ack == needs_ack
            && session.activity_id.as_deref() == Some(event.turn_id.as_str())
            && session.name == name
    });
    if already_current {
        return (false, None, mutes_changed);
    }

    let created_ms = previous
        .as_ref()
        .filter(|session| session.activity_id.as_deref() == Some(event.turn_id.as_str()))
        .map(|session| session.created_ms)
        .unwrap_or(now);
    store.sessions.insert(
        key,
        SessionInfo {
            id: event.session_id.clone(),
            source: "zcode".to_string(),
            name,
            state: event.kind.clone(),
            detail: detail.to_string(),
            updated_ms: now,
            created_ms,
            needs_ack,
            activity_id: Some(event.turn_id.clone()),
            monitor_waiting: false,
        },
    );
    let attention = needs_ack.then_some(if event.kind == "error" {
        "error"
    } else {
        "done"
    });
    (true, attention, mutes_changed)
}

fn read_active_zcode_turns(path: &Path) -> rusqlite::Result<Vec<ZcodeTurn>> {
    let connection = open_read_only(path)?;
    let mut statement = connection.prepare(
        "SELECT t.turn_id,
                t.session_id,
                s.title,
                t.started_at,
                CASE
                    WHEN EXISTS (
                        SELECT 1 FROM tool_usage u
                        WHERE u.session_id = t.session_id
                          AND u.turn_id = t.turn_id
                          AND u.status = 'running'
                          AND LOWER(REPLACE(u.tool_name, '_', '')) IN (
                              'askuserquestion', 'requestuserinput', 'elicitation'
                          )
                    ) THEN 'Agent 正在等你回答'
                    WHEN EXISTS (
                        SELECT 1 FROM tool_usage u
                        WHERE u.session_id = t.session_id
                          AND u.turn_id = t.turn_id
                          AND u.status = 'running'
                          AND COALESCE(NULLIF(LOWER(u.approval_status), ''), 'none') NOT IN (
                              'none', 'allow', 'allowed', 'approved', 'auto', 'not_required'
                          )
                    ) THEN '需要你确认权限'
                    ELSE NULL
                END
         FROM turn_usage t
         JOIN session s ON s.id = t.session_id
         WHERE t.status = 'running'",
    )?;
    let turns = statement
        .query_map([], |row| {
            let started_at: i64 = row.get(3)?;
            let raw_name: String = row.get(2)?;
            Ok(ZcodeTurn {
                turn_id: row.get(0)?,
                session_id: row.get(1)?,
                name: clean_session_name(&raw_name),
                started_ms: started_at.max(0) as u64,
                waiting_detail: row.get(4)?,
            })
        })?
        .collect();
    turns
}

fn read_recent_completed_zcode_turns(path: &Path) -> rusqlite::Result<Vec<ZcodeCompletedTurn>> {
    let connection = open_read_only(path)?;
    let mut statement = connection.prepare(
        "SELECT t.turn_id,
                t.session_id,
                s.title,
                t.started_at
         FROM turn_usage t
         JOIN session s ON s.id = t.session_id
         WHERE t.status = 'completed'
         ORDER BY COALESCE(t.completed_at, t.started_at) DESC
         LIMIT 64",
    )?;
    let turns = statement
        .query_map([], |row| {
            let started_at: i64 = row.get(3)?;
            let raw_name: String = row.get(2)?;
            Ok(ZcodeCompletedTurn {
                turn_id: row.get(0)?,
                session_id: row.get(1)?,
                name: clean_session_name(&raw_name),
                started_ms: started_at.max(0) as u64,
            })
        })?
        .collect();
    turns
}

fn sync_zcode_completed_turns(
    store: &mut Store,
    known: &mut HashSet<String>,
    current: Vec<ZcodeCompletedTurn>,
    baseline_only: bool,
) -> (bool, usize, bool) {
    let recent_ids: HashSet<String> = current.iter().map(|turn| turn.turn_id.clone()).collect();
    if baseline_only {
        *known = recent_ids;
        return (false, 0, false);
    }

    let now = now_ms();
    let mut changed = false;
    let mut completed = 0;
    let mut mutes_changed = false;

    // The database returns newest-first. Apply oldest-first so a burst of short
    // turns leaves the session pointing at its most recent completion.
    for turn in current
        .iter()
        .rev()
        .filter(|turn| !known.contains(&turn.turn_id))
    {
        let key = format!("zcode:{}", turn.session_id);
        if let Some(muted_activity_id) =
            store.muted.get(&key).map(|muted| muted.activity_id.clone())
        {
            if muted_activity_id.as_deref() == Some(turn.turn_id.as_str()) {
                continue;
            }
            store.muted.remove(&key);
            mutes_changed = true;
        }

        let already_done = store.sessions.get(&key).is_some_and(|session| {
            session.state == "done"
                && session.needs_ack
                && session.activity_id.as_deref() == Some(turn.turn_id.as_str())
        });
        if already_done {
            continue;
        }

        store.sessions.insert(
            key,
            SessionInfo {
                id: turn.session_id.clone(),
                source: "zcode".to_string(),
                name: turn.name.clone(),
                state: "done".to_string(),
                detail: "任务已完成".to_string(),
                updated_ms: now,
                created_ms: if turn.started_ms == 0 {
                    now
                } else {
                    turn.started_ms
                },
                needs_ack: true,
                activity_id: Some(turn.turn_id.clone()),
                monitor_waiting: false,
            },
        );
        changed = true;
        completed += 1;
    }

    *known = recent_ids;
    (changed, completed, mutes_changed)
}

async fn run_zcode_log_monitor(app: AppHandle, store: Arc<Mutex<Store>>) {
    let mut current_path: Option<PathBuf> = None;
    let mut offset = 0_u64;
    let mut initialized = false;

    loop {
        let Some(path) = latest_zcode_log_path() else {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        };
        let path_changed = current_path.as_ref() != Some(&path);
        if path_changed {
            current_path = Some(path.clone());
            offset = 0;
        }

        let start_offset = offset;
        let read_path = path.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut next_offset = start_offset;
            let events = read_appended_zcode_log_events(&read_path, &mut next_offset)?;
            Ok::<_, std::io::Error>((events, next_offset))
        })
        .await;

        if let Ok(Ok((events, next_offset))) = result {
            offset = next_offset;
            let baseline = !initialized;
            initialized = true;
            let events = if baseline {
                let mut latest_by_session = HashMap::new();
                for event in events {
                    latest_by_session.insert(event.session_id.clone(), event);
                }
                latest_by_session
                    .into_values()
                    .filter(|event| event.kind == "working")
                    .collect::<Vec<_>>()
            } else {
                events
            };

            if !events.is_empty() {
                let db_path = zcode_db_path();
                let named_events = events
                    .into_iter()
                    .map(|event| {
                        let name = db_path.as_ref().and_then(|path| {
                            read_zcode_session_name(path, &event.session_id)
                                .ok()
                                .flatten()
                        });
                        (event, name)
                    })
                    .collect::<Vec<_>>();

                let mut state = store.lock().unwrap();
                let mut changed = false;
                let mut mutes_changed = false;
                let mut attentions = Vec::new();
                for (event, name) in &named_events {
                    let (event_changed, attention, event_mutes_changed) =
                        sync_zcode_log_event(&mut state, event, name.as_deref());
                    changed |= event_changed;
                    mutes_changed |= event_mutes_changed;
                    if let Some(kind) = attention {
                        attentions.push(kind);
                    }
                }
                let payload = changed.then(|| state.payload());
                let muted = mutes_changed.then(|| state.muted.clone());
                drop(state);

                if let Some(muted) = muted {
                    persist_muted_sessions(&app, &muted);
                }
                if let Some(payload) = payload {
                    emit_payload(&app, &payload);
                }
                for kind in attentions {
                    let detail = if kind == "error" {
                        "任务运行失败"
                    } else {
                        "任务已完成"
                    };
                    show_attention_notification(&app, "zcode", kind, detail);
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(ZCODE_LOG_POLL_MS)).await;
    }
}

fn sync_zcode_turns(
    store: &mut Store,
    previous: &mut HashMap<String, ZcodeTurn>,
    current: Vec<ZcodeTurn>,
) -> (bool, usize, usize, bool) {
    let now = now_ms();
    let current_ids: HashSet<&str> = current.iter().map(|turn| turn.turn_id.as_str()).collect();
    let active_sessions: HashSet<&str> = current
        .iter()
        .map(|turn| turn.session_id.as_str())
        .collect();
    let mut changed = false;
    let mut completed = 0;
    let mut waiting = 0;
    let mut mutes_changed = false;

    for turn in &current {
        let key = format!("zcode:{}", turn.session_id);
        if let Some(muted_activity_id) =
            store.muted.get(&key).map(|muted| muted.activity_id.clone())
        {
            if muted_activity_id
                .as_deref()
                .is_some_and(|id| id != turn.turn_id)
            {
                store.muted.remove(&key);
                mutes_changed = true;
            } else {
                continue;
            }
        }

        let created_ms = if turn.started_ms == 0 {
            now
        } else {
            turn.started_ms
        };
        let hook_waiting = store.sessions.get(&key).is_some_and(|session| {
            session.state == "waiting"
                && session.needs_ack
                && !session.monitor_waiting
                && turn.waiting_detail.is_none()
                && session
                    .activity_id
                    .as_deref()
                    .is_none_or(|activity_id| activity_id == turn.turn_id)
        });
        let target_waiting = turn.waiting_detail.is_some() || hook_waiting;
        let target_state = if target_waiting { "waiting" } else { "working" };
        let target_detail = turn.waiting_detail.as_deref().unwrap_or_else(|| {
            if hook_waiting {
                store
                    .sessions
                    .get(&key)
                    .map(|session| session.detail.as_str())
                    .unwrap_or("等待你的输入")
            } else {
                "ZCode 正在运行"
            }
        });
        let became_waiting = target_waiting
            && store
                .sessions
                .get(&key)
                .is_none_or(|session| session.state != "waiting" || !session.needs_ack);
        let needs_update = store.sessions.get(&key).is_none_or(|session| {
            session.state != target_state
                || session.name != turn.name
                || session.detail != target_detail
                || session.created_ms != created_ms
                || session.needs_ack != target_waiting
                || session.activity_id.as_deref() != Some(turn.turn_id.as_str())
                || session.monitor_waiting != turn.waiting_detail.is_some()
        });
        if needs_update {
            store.sessions.insert(
                key,
                SessionInfo {
                    id: turn.session_id.clone(),
                    source: "zcode".to_string(),
                    name: turn.name.clone(),
                    state: target_state.to_string(),
                    detail: target_detail.to_string(),
                    updated_ms: now,
                    created_ms,
                    needs_ack: target_waiting,
                    activity_id: Some(turn.turn_id.clone()),
                    monitor_waiting: turn.waiting_detail.is_some(),
                },
            );
            changed = true;
        } else if let Some(session) = store.sessions.get_mut(&key) {
            session.updated_ms = now;
        }
        if became_waiting {
            waiting += 1;
        }
    }

    for turn in previous.values() {
        if current_ids.contains(turn.turn_id.as_str())
            || active_sessions.contains(turn.session_id.as_str())
        {
            continue;
        }
        let key = format!("zcode:{}", turn.session_id);
        if store.muted.contains_key(&key) {
            continue;
        }
        if let Some(session) = store.sessions.get_mut(&key) {
            if matches!(session.state.as_str(), "working" | "waiting") {
                session.state = "done".to_string();
                session.detail = "任务已完成".to_string();
                session.updated_ms = now;
                session.needs_ack = true;
                changed = true;
                completed += 1;
            }
        }
    }

    previous.clear();
    previous.extend(current.into_iter().map(|turn| (turn.turn_id.clone(), turn)));
    (changed, completed, waiting, mutes_changed)
}

async fn run_zcode_monitor(app: AppHandle, store: Arc<Mutex<Store>>) {
    let mut previous = HashMap::new();
    let mut known_completed = HashSet::new();
    let mut completed_baselined = false;
    loop {
        let (active_result, completed_result) = if let Some(path) = zcode_db_path() {
            let active_path = path.clone();
            let active =
                tauri::async_runtime::spawn_blocking(move || read_active_zcode_turns(&active_path));
            let completed = tauri::async_runtime::spawn_blocking(move || {
                read_recent_completed_zcode_turns(&path)
            });
            tokio::join!(active, completed)
        } else {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            continue;
        };

        if let (Ok(Ok(current)), Ok(Ok(recent_completed))) = (active_result, completed_result) {
            let mut state = store.lock().unwrap();
            let (active_changed, active_completed, waiting, active_mutes_changed) =
                sync_zcode_turns(&mut state, &mut previous, current);
            let (short_changed, short_completed, short_mutes_changed) = sync_zcode_completed_turns(
                &mut state,
                &mut known_completed,
                recent_completed,
                !completed_baselined,
            );
            completed_baselined = true;
            let changed = active_changed || short_changed;
            let completed = active_completed + short_completed;
            let mutes_changed = active_mutes_changed || short_mutes_changed;
            let payload = changed.then(|| state.payload());
            let muted = mutes_changed.then(|| state.muted.clone());
            drop(state);
            if let Some(muted) = muted {
                persist_muted_sessions(&app, &muted);
            }
            if let Some(payload) = payload {
                emit_payload(&app, &payload);
            }
            if completed > 0 {
                let detail = if completed == 1 {
                    "任务已完成".to_string()
                } else {
                    format!("{completed} 个任务已完成")
                };
                show_attention_notification(&app, "zcode", "done", &detail);
            }
            if waiting > 0 {
                let detail = if waiting == 1 {
                    "等待你的输入".to_string()
                } else {
                    format!("{waiting} 个任务在等待输入")
                };
                show_attention_notification(&app, "zcode", "waiting", &detail);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(ZCODE_POLL_MS)).await;
    }
}

#[cfg(target_os = "windows")]
fn focus_window_for_source(source: &str) -> bool {
    windows_focus::focus(source)
}

#[cfg(not(target_os = "windows"))]
fn focus_window_for_source(_source: &str) -> bool {
    false
}

#[cfg(target_os = "windows")]
mod windows_focus {
    use std::collections::HashMap;
    use std::ffi::c_void;

    type Bool = i32;
    type Dword = u32;
    type Handle = *mut c_void;
    type Hwnd = *mut c_void;
    type Lparam = isize;

    const PROCESS_QUERY_LIMITED_INFORMATION: Dword = 0x1000;
    const SW_RESTORE: i32 = 9;
    const TH32CS_SNAPPROCESS: Dword = 0x0000_0002;
    const MAX_PATH: usize = 260;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(
            callback: Option<unsafe extern "system" fn(Hwnd, Lparam) -> Bool>,
            data: Lparam,
        ) -> Bool;
        fn IsWindowVisible(window: Hwnd) -> Bool;
        fn IsIconic(window: Hwnd) -> Bool;
        fn GetWindowTextLengthW(window: Hwnd) -> i32;
        fn GetWindowTextW(window: Hwnd, text: *mut u16, max_count: i32) -> i32;
        fn GetWindowThreadProcessId(window: Hwnd, process_id: *mut Dword) -> Dword;
        fn ShowWindow(window: Hwnd, command: i32) -> Bool;
        fn BringWindowToTop(window: Hwnd) -> Bool;
        fn SetForegroundWindow(window: Hwnd) -> Bool;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: Dword, inherit_handle: Bool, process_id: Dword) -> Handle;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: Dword,
            filename: *mut u16,
            size: *mut Dword,
        ) -> Bool;
        fn CloseHandle(object: Handle) -> Bool;
        fn CreateToolhelp32Snapshot(flags: Dword, process_id: Dword) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> Bool;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> Bool;
    }

    #[repr(C)]
    struct ProcessEntry32W {
        size: Dword,
        usage: Dword,
        process_id: Dword,
        default_heap_id: usize,
        module_id: Dword,
        threads: Dword,
        parent_process_id: Dword,
        priority_class_base: i32,
        flags: Dword,
        exe_file: [u16; MAX_PATH],
    }

    struct SearchContext {
        source: String,
        best_window: Hwnd,
        best_score: i32,
        processes: HashMap<Dword, (Dword, String)>,
    }

    fn process_name_matches(source: &str, process_name: &str) -> bool {
        let process_name = process_name.to_lowercase();
        match source {
            "opencode" => process_name == "opencode.exe" || process_name == "opencode",
            "claude" => process_name == "claude.exe" || process_name == "claude",
            "codex" => process_name == "codex.exe" || process_name == "codex-code-mode-host.exe",
            "zcode" => process_name == "zcode.exe" || process_name == "zcode",
            _ => false,
        }
    }

    fn has_matching_descendant(
        source: &str,
        root_process_id: Dword,
        processes: &HashMap<Dword, (Dword, String)>,
    ) -> bool {
        processes.iter().any(|(&process_id, (_, name))| {
            if !process_name_matches(source, name) {
                return false;
            }
            let mut cursor = process_id;
            for _ in 0..32 {
                if cursor == root_process_id {
                    return true;
                }
                let Some((parent, _)) = processes.get(&cursor) else {
                    return false;
                };
                if *parent == 0 || *parent == cursor {
                    return false;
                }
                cursor = *parent;
            }
            false
        })
    }

    unsafe fn process_snapshot() -> HashMap<Dword, (Dword, String)> {
        let mut processes = HashMap::new();
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot as isize == -1 {
            return processes;
        }
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.size = std::mem::size_of::<ProcessEntry32W>() as Dword;
        let mut available = Process32FirstW(snapshot, &mut entry) != 0;
        while available {
            let name_length = entry
                .exe_file
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.exe_file.len());
            processes.insert(
                entry.process_id,
                (
                    entry.parent_process_id,
                    String::from_utf16_lossy(&entry.exe_file[..name_length]),
                ),
            );
            available = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        processes
    }

    fn match_score(source: &str, title: &str, path: &str) -> i32 {
        let title = title.to_lowercase();
        let path = path.to_lowercase();
        match source {
            "codex" => {
                if path.contains("openai.codex_") {
                    120
                } else if title.contains("codex") {
                    100
                } else {
                    0
                }
            }
            "opencode" => {
                if path.ends_with("opencode.exe") {
                    120
                } else if title.contains("opencode") {
                    100
                } else {
                    0
                }
            }
            "claude" => {
                if path.ends_with("claude.exe") {
                    120
                } else if title.contains("claude") {
                    100
                } else {
                    0
                }
            }
            "zcode" => {
                if path.ends_with("zcode.exe") {
                    120
                } else if title.contains("zcode") {
                    100
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    unsafe fn window_title(window: Hwnd) -> String {
        let length = GetWindowTextLengthW(window);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0_u16; length as usize + 1];
        let copied = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
        String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
    }

    unsafe fn process_path(process_id: Dword) -> String {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if process.is_null() {
            return String::new();
        }
        let mut buffer = vec![0_u16; 1024];
        let mut length = buffer.len() as Dword;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        if ok == 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buffer[..length as usize])
        }
    }

    unsafe extern "system" fn inspect_window(window: Hwnd, data: Lparam) -> Bool {
        if IsWindowVisible(window) == 0 {
            return 1;
        }
        let context = &mut *(data as *mut SearchContext);
        let title = window_title(window);
        let mut process_id = 0;
        GetWindowThreadProcessId(window, &mut process_id);
        let path = process_path(process_id);
        let direct_score = match_score(&context.source, &title, &path);
        let score = if direct_score > 0 {
            direct_score
        } else if has_matching_descendant(&context.source, process_id, &context.processes) {
            90
        } else {
            0
        };
        if score > context.best_score {
            context.best_score = score;
            context.best_window = window;
        }
        1
    }

    pub fn focus(source: &str) -> bool {
        let mut context = SearchContext {
            source: source.to_string(),
            best_window: std::ptr::null_mut(),
            best_score: 0,
            processes: unsafe { process_snapshot() },
        };
        unsafe {
            EnumWindows(
                Some(inspect_window),
                (&mut context as *mut SearchContext) as Lparam,
            );
            if context.best_window.is_null() {
                return false;
            }
            if IsIconic(context.best_window) != 0 {
                ShowWindow(context.best_window, SW_RESTORE);
            }
            let raised = BringWindowToTop(context.best_window) != 0;
            let focused = SetForegroundWindow(context.best_window) != 0;
            raised || focused
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{has_matching_descendant, match_score};
        use std::collections::HashMap;

        #[test]
        fn identifies_codex_package_without_matching_plain_chatgpt_titles() {
            assert_eq!(
                match_score(
                    "codex",
                    "ChatGPT",
                    r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0\app\ChatGPT.exe"
                ),
                120
            );
            assert_eq!(
                match_score("codex", "ChatGPT", r"C:\Program Files\ChatGPT\ChatGPT.exe"),
                0
            );
        }

        #[test]
        fn identifies_zcode_desktop_window() {
            assert_eq!(
                match_score(
                    "zcode",
                    "Project - ZCode",
                    r"C:\Users\Someone\Apps\ZCode\ZCode.exe"
                ),
                120
            );
            assert_eq!(
                match_score("zcode", "Project", r"C:\Windows\notepad.exe"),
                0
            );
        }

        #[test]
        fn finds_a_cli_nested_under_its_terminal_process() {
            let processes = HashMap::from([
                (10, (1, "WindowsTerminal.exe".to_string())),
                (20, (10, "conhost.exe".to_string())),
                (30, (20, "claude.exe".to_string())),
            ]);
            assert!(has_matching_descendant("claude", 10, &processes));
            assert!(!has_matching_descendant("opencode", 10, &processes));
        }
    }
}

fn position_window(app: &tauri::App) {
    if let Some(window) = app.get_webview_window("pet") {
        if let Ok(Some(monitor)) = window.current_monitor() {
            let monitor_size = monitor.size();
            let monitor_position = monitor.position();
            let Ok(window_size) = window.outer_size() else {
                return;
            };
            let scale = monitor.scale_factor();
            let margin = (14.0 * scale) as i32;
            let taskbar = (52.0 * scale) as i32;
            let x =
                monitor_position.x + monitor_size.width as i32 - window_size.width as i32 - margin;
            let y = monitor_position.y + monitor_size.height as i32
                - window_size.height as i32
                - taskbar
                - margin;
            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    // 版本行：enabled=false 显示为灰色且不可点击，仅供查看；
    // 版本号编译期取自 Cargo.toml（五处版本同步的其中一处）
    let version = MenuItem::with_id(
        app,
        "version",
        format!("小狗桌宠 v{}", env!("CARGO_PKG_VERSION")),
        false,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(app, "show", "显示 / 隐藏", true, None::<&str>)?;
    let autostart_enabled = {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().is_enabled().unwrap_or(false)
    };
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "开机自启",
        true,
        autostart_enabled,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&version, &show, &autostart, &quit])?;

    TrayIconBuilder::with_id("pet-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("小狗桌宠")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("pet") {
                    if window.is_visible().unwrap_or(true) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
            "autostart" => {
                use tauri_plugin_autostart::ManagerExt;
                let manager = app.autolaunch();
                let result = if manager.is_enabled().unwrap_or(false) {
                    manager.disable()
                } else {
                    manager.enable()
                };
                if let Err(error) = result {
                    eprintln!("autostart toggle failed: {error}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn hook_bridge_payload(
    mode: &str,
    requested_source: &str,
    input: &serde_json::Value,
) -> Option<Vec<u8>> {
    let source = match requested_source {
        "zcode" => "zcode",
        _ => "codex",
    };
    let kind = match mode {
        "waiting" | "working" => Some(mode),
        "auto" if source == "zcode" => None,
        _ => return None,
    };
    let session_id = input["session_id"]
        .as_str()
        .or_else(|| input["sessionId"].as_str())?;
    let event_name = input["hook_event_name"]
        .as_str()
        .or_else(|| input["hookEventName"].as_str())
        .unwrap_or_default();
    let tool_name = input["tool_name"]
        .as_str()
        .or_else(|| input["toolName"].as_str())
        .unwrap_or_default();
    let message = if source == "zcode" {
        None
    } else if kind == Some("working") {
        Some("Codex 已收到你的输入")
    } else if event_name == "PermissionRequest" || tool_name == "request_permissions" {
        Some("需要你确认权限")
    } else if tool_name == "request_user_input" {
        Some("Agent 正在等你回答")
    } else if tool_name.starts_with("mcp__") {
        Some("MCP 工具正在等待确认")
    } else {
        Some("等待你的输入")
    };
    serde_json::to_vec(&serde_json::json!({
        "source": source,
        "kind": kind,
        "session_id": session_id,
        "message": message,
        "prompt": input["prompt"],
        "cwd": input["cwd"],
        "tool_name": tool_name,
        "hook_event_name": event_name,
        "session_name": input["session_name"].as_str().or_else(|| input["sessionName"].as_str()),
        "transcript_path": input["transcript_path"].as_str().or_else(|| input["transcriptPath"].as_str()),
        "activity_id": input["turn_id"]
            .as_str()
            .or_else(|| input["turnId"].as_str())
            .or_else(|| input["tool_use_id"].as_str())
            .or_else(|| input["toolCallId"].as_str()),
    }))
    .ok()
}

fn decode_hook_input(bytes: &[u8]) -> String {
    let is_utf16_le = bytes.starts_with(&[0xff, 0xfe])
        || (bytes.len() >= 4
            && bytes.len().is_multiple_of(2)
            && bytes
                .iter()
                .skip(1)
                .step_by(2)
                .filter(|byte| **byte == 0)
                .count()
                >= bytes.len() / 8);
    if is_utf16_le {
        let start = usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2;
        let (pairs, _) = bytes[start..].as_chunks::<2>();
        let units = pairs
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }

    let is_utf16_be = bytes.starts_with(&[0xfe, 0xff]);
    if is_utf16_be {
        let (pairs, _) = bytes[2..].as_chunks::<2>();
        let units = pairs
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }

    String::from_utf8_lossy(bytes).into_owned()
}

pub fn run_hook_bridge_if_requested() -> bool {
    let mut args = std::env::args();
    let _ = args.next();
    if args.next().as_deref() != Some("--pet-hook") {
        return false;
    }
    let mode = args.next().unwrap_or_default();
    let source = args.next().unwrap_or_else(|| "codex".to_string());
    let mut bytes = Vec::new();
    let _ = std::io::stdin().take(1024 * 1024).read_to_end(&mut bytes);
    let raw = decode_hook_input(&bytes);
    let Some(body) = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|input| hook_bridge_payload(&mode, &source, &input))
    else {
        return true;
    };

    if let Ok(mut stream) = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], PORT)),
        Duration::from_millis(700),
    ) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(700)));
        let request = format!(
            "POST /event HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(request.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Write);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(700)));
        let mut response = [0_u8; 256];
        let _ = stream.read(&mut response);
    }
    true
}

pub fn run() {
    let store = Arc::new(Mutex::new(Store::default()));

    tauri::Builder::default()
        // 必须是第一个注册的插件（官方要求）。第二个实例启动时自动退出，
        // 并把这里当作「用户想让小狗现身」的信号：若被托盘隐藏则恢复显示。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("pet") {
                if !window.is_visible().unwrap_or(false) {
                    let _ = window.show();
                }
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(store.clone())
        .setup(move |app| {
            store.lock().unwrap().muted = load_muted_sessions(app.handle());
            match integrations::install_for_current_user() {
                Ok(report) if report != integrations::InstallReport::default() => {
                    eprintln!("installed monitoring integrations: {report:?}");
                }
                Ok(_) => {}
                Err(error) => eprintln!("failed to install monitoring integrations: {error}"),
            }
            position_window(app);
            build_tray(app)?;

            let codex_app = app.handle().clone();
            let codex_store = store.clone();
            tauri::async_runtime::spawn(run_codex_monitor(codex_app, codex_store));

            let zcode_app = app.handle().clone();
            let zcode_store = store.clone();
            tauri::async_runtime::spawn(run_zcode_monitor(zcode_app, zcode_store));

            let zcode_log_app = app.handle().clone();
            let zcode_log_store = store.clone();
            tauri::async_runtime::spawn(run_zcode_log_monitor(zcode_log_app, zcode_log_store));

            let cleanup_app = app.handle().clone();
            let cleanup_store = store.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    let now = now_ms();
                    let mut state = cleanup_store.lock().unwrap();
                    let before = state.sessions.len();
                    state.sessions.retain(|_, session| {
                        if session.needs_ack {
                            return true;
                        }
                        let max_age = if session.state == "working" {
                            WORKING_STALE_MS
                        } else {
                            QUIET_STALE_MS
                        };
                        now.saturating_sub(session.updated_ms) < max_age
                    });
                    if state.sessions.len() == before {
                        continue;
                    }
                    let payload = state.payload();
                    drop(state);
                    emit_payload(&cleanup_app, &payload);
                }
            });

            let state = AppState {
                store,
                app: app.handle().clone(),
            };
            tauri::async_runtime::spawn(async move {
                let router = Router::new()
                    .route("/event", post(handle_event))
                    .with_state(state);
                match tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await {
                    Ok(listener) => {
                        if let Err(error) = axum::serve(listener, router).await {
                            eprintln!("event server error: {error}");
                        }
                    }
                    Err(error) => eprintln!("failed to bind 127.0.0.1:{PORT}: {error}"),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            acknowledge_done,
            dismiss_session,
            mute_session,
            focus_session_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_session_stays_pending_until_acknowledged() {
        let mut store = Store::default();
        store.sessions.insert(
            "codex:test".into(),
            SessionInfo {
                id: "test".into(),
                source: "codex".into(),
                name: "测试会话".into(),
                state: "done".into(),
                detail: String::new(),
                updated_ms: 1,
                created_ms: 1,
                needs_ack: true,
                activity_id: Some("turn-test".into()),
                monitor_waiting: false,
            },
        );
        assert_eq!(store.pending_count(), 1);
        assert_eq!(store.aggregate(), "done");
        store.sessions.get_mut("codex:test").unwrap().needs_ack = false;
        assert_eq!(store.pending_count(), 0);
        assert_eq!(store.aggregate(), "idle");
    }

    #[test]
    fn session_names_are_compact_and_single_line() {
        assert_eq!(
            clean_session_name("  修复\n  多会话   面板  "),
            "修复 多会话 面板"
        );
        assert_eq!(clean_session_name(&"a".repeat(60)).chars().count(), 48);
    }

    #[test]
    fn claude_question_and_elicitation_events_wait_for_input() {
        let event = IncomingEvent {
            source: Some("claude".into()),
            kind: None,
            hook_event_name: Some("PreToolUse".into()),
            session_id: Some("test".into()),
            session_name: None,
            message: None,
            prompt: None,
            cwd: None,
            tool_name: Some("AskUserQuestion".into()),
            notification_type: None,
            transcript_path: None,
            activity_id: None,
        };
        assert_eq!(normalize_kind(&event).as_deref(), Some("waiting"));
        assert_eq!(event_detail(&event, "waiting"), "Agent 正在等你回答");
    }

    #[test]
    fn claude_idle_prompt_does_not_create_a_false_waiting_state() {
        let event = IncomingEvent {
            source: Some("claude".into()),
            kind: None,
            hook_event_name: Some("Notification".into()),
            session_id: Some("test".into()),
            session_name: None,
            message: Some("Claude is waiting for your input".into()),
            prompt: None,
            cwd: None,
            tool_name: None,
            notification_type: Some("idle_prompt".into()),
            transcript_path: None,
            activity_id: None,
        };
        assert_eq!(normalize_kind(&event), None);
    }

    #[test]
    fn codex_hook_bridge_preserves_session_and_wait_reason() {
        let input = serde_json::json!({
            "session_id": "thread-123",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash"
        });
        let body = hook_bridge_payload("waiting", "codex", &input).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["source"], "codex");
        assert_eq!(payload["kind"], "waiting");
        assert_eq!(payload["session_id"], "thread-123");
        assert_eq!(payload["message"], "需要你确认权限");
    }

    #[test]
    fn codex_hook_bridge_carries_the_turn_marker() {
        let input = serde_json::json!({
            "session_id": "thread-123",
            "turn_id": "turn-456",
            "hook_event_name": "PreToolUse",
            "tool_name": "request_user_input"
        });
        let body = hook_bridge_payload("waiting", "codex", &input).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["activity_id"], "turn-456");
    }

    #[test]
    fn codex_hook_bridge_accepts_windows_utf16_input() {
        let raw = r#"{"session_id":"thread-utf16","hook_event_name":"PreToolUse","tool_name":"request_user_input"}"#;
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(
            raw.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let input: serde_json::Value = serde_json::from_str(&decode_hook_input(&bytes)).unwrap();
        let body = hook_bridge_payload("waiting", "codex", &input).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["session_id"], "thread-utf16");
        assert_eq!(payload["message"], "Agent 正在等你回答");
    }

    #[test]
    fn zcode_hook_bridge_keeps_native_event_fields_for_state_mapping() {
        let input = serde_json::json!({
            "session_id": "sess-zcode",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "给桌宠增加 ZCode 支持",
            "cwd": r"C:\work\pet"
        });
        let body = hook_bridge_payload("auto", "zcode", &input).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["source"], "zcode");
        assert!(payload["kind"].is_null());
        assert_eq!(payload["hook_event_name"], "UserPromptSubmit");
        assert_eq!(payload["prompt"], "给桌宠增加 ZCode 支持");
    }

    #[test]
    fn zcode_hook_bridge_accepts_desktop_camel_case_fields() {
        let input = serde_json::json!({
            "sessionId": "sess-desktop",
            "turnId": "turn-desktop",
            "hookEventName": "UserPromptSubmit",
            "prompt": "桌面端任务",
            "cwd": r"C:\work\desktop"
        });
        let body = hook_bridge_payload("auto", "zcode", &input).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["session_id"], "sess-desktop");
        assert_eq!(payload["activity_id"], "turn-desktop");
        assert_eq!(payload["hook_event_name"], "UserPromptSubmit");
    }

    #[test]
    fn zcode_question_and_failed_tool_states_are_mapped_without_finishing_early() {
        let question = IncomingEvent {
            source: Some("zcode".into()),
            kind: None,
            hook_event_name: Some("PreToolUse".into()),
            session_id: Some("sess-zcode".into()),
            session_name: None,
            message: None,
            prompt: None,
            cwd: None,
            tool_name: Some("request_user_input".into()),
            notification_type: None,
            transcript_path: None,
            activity_id: None,
        };
        assert_eq!(normalize_kind(&question).as_deref(), Some("waiting"));

        let failed_tool = IncomingEvent {
            hook_event_name: Some("PostToolUseFailure".into()),
            tool_name: Some("Bash".into()),
            ..question
        };
        assert_eq!(normalize_kind(&failed_tool).as_deref(), Some("working"));
    }

    #[test]
    fn zcode_desktop_log_events_drive_working_and_done() {
        let started = parse_zcode_log_event(
            r#"{"event":"turn.started","sessionId":"sess-1","turnId":"turn-1"}"#,
        )
        .unwrap();
        assert_eq!(started.kind, "working");

        let mut store = Store::default();
        let (changed, attention, _) =
            sync_zcode_log_event(&mut store, &started, Some("桌面端会话"));
        assert!(changed);
        assert_eq!(attention, None);
        assert_eq!(store.sessions["zcode:sess-1"].state, "working");

        let completed = parse_zcode_log_event(
            r#"{"event":"turn.completed","sessionId":"sess-1","turnId":"turn-1"}"#,
        )
        .unwrap();
        let (changed, attention, _) =
            sync_zcode_log_event(&mut store, &completed, Some("桌面端会话"));
        assert!(changed);
        assert_eq!(attention, Some("done"));
        assert_eq!(store.sessions["zcode:sess-1"].state, "done");
        assert!(store.sessions["zcode:sess-1"].needs_ack);
    }

    #[test]
    fn zcode_desktop_log_tail_preserves_an_incomplete_json_line() {
        use std::io::Write as _;

        let path = std::env::temp_dir().join(format!(
            "golden-pet-zcode-log-{}-{}.jsonl",
            std::process::id(),
            now_ms()
        ));
        std::fs::write(
            &path,
            concat!(
                "{\"event\":\"turn.started\",\"sessionId\":\"sess-1\",\"turnId\":\"turn-1\"}\n",
                "{\"event\":\"turn.completed\",\"sessionId\":\"sess-1\""
            ),
        )
        .unwrap();

        let mut offset = 0;
        let first = read_appended_zcode_log_events(&path, &mut offset).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, "working");

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b",\"turnId\":\"turn-1\"}\n").unwrap();
        drop(file);

        let second = read_appended_zcode_log_events(&path, &mut offset).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].kind, "done");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reads_active_zcode_turn_and_pending_question_from_database() {
        let path = std::env::temp_dir().join(format!(
            "golden-pet-zcode-monitor-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL);
                 CREATE TABLE turn_usage (
                    session_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    started_at INTEGER NOT NULL
                 );
                 CREATE TABLE tool_usage (
                    session_id TEXT NOT NULL,
                    turn_id TEXT,
                    tool_name TEXT NOT NULL,
                    approval_status TEXT,
                    status TEXT NOT NULL
                 );
                 INSERT INTO session VALUES ('sess-1', '修复桌宠识别');
                 INSERT INTO turn_usage VALUES ('sess-1', 'turn-1', 'running', 1234);
                 INSERT INTO tool_usage VALUES (
                    'sess-1', 'turn-1', 'AskUserQuestion', 'none', 'running'
                 );",
            )
            .unwrap();
        drop(connection);

        let turns = read_active_zcode_turns(&path).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].session_id, "sess-1");
        assert_eq!(turns[0].name, "修复桌宠识别");
        assert_eq!(turns[0].started_ms, 1234);
        assert_eq!(
            turns[0].waiting_detail.as_deref(),
            Some("Agent 正在等你回答")
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn zcode_database_monitor_tracks_running_then_completed() {
        let mut store = Store::default();
        let mut previous = HashMap::new();
        let turn = ZcodeTurn {
            turn_id: "turn-1".into(),
            session_id: "sess-1".into(),
            name: "测试 ZCode".into(),
            started_ms: 1234,
            waiting_detail: None,
        };

        let (changed, completed, waiting, _) =
            sync_zcode_turns(&mut store, &mut previous, vec![turn]);
        assert!(changed);
        assert_eq!(completed, 0);
        assert_eq!(waiting, 0);
        assert_eq!(store.sessions["zcode:sess-1"].state, "working");

        let (changed, completed, waiting, _) =
            sync_zcode_turns(&mut store, &mut previous, Vec::new());
        assert!(changed);
        assert_eq!(completed, 1);
        assert_eq!(waiting, 0);
        assert_eq!(store.sessions["zcode:sess-1"].state, "done");
        assert!(store.sessions["zcode:sess-1"].needs_ack);
    }

    #[test]
    fn zcode_database_monitor_recovers_a_short_turn_between_polls() {
        let mut store = Store::default();
        let mut known = HashSet::new();
        let old_turn = ZcodeCompletedTurn {
            turn_id: "turn-old".into(),
            session_id: "sess-old".into(),
            name: "旧任务".into(),
            started_ms: 1000,
        };

        let result =
            sync_zcode_completed_turns(&mut store, &mut known, vec![old_turn.clone()], true);
        assert_eq!(result, (false, 0, false));
        assert!(store.sessions.is_empty());

        let short_turn = ZcodeCompletedTurn {
            turn_id: "turn-short".into(),
            session_id: "sess-short".into(),
            name: "快速回复".into(),
            started_ms: 2000,
        };
        let (changed, completed, _) =
            sync_zcode_completed_turns(&mut store, &mut known, vec![short_turn, old_turn], false);

        assert!(changed);
        assert_eq!(completed, 1);
        let session = &store.sessions["zcode:sess-short"];
        assert_eq!(session.state, "done");
        assert_eq!(session.activity_id.as_deref(), Some("turn-short"));
        assert!(session.needs_ack);
    }

    #[test]
    fn codex_rollout_question_is_waiting_until_answered() {
        let path = std::env::temp_dir().join(format!(
            "golden-pet-codex-waiting-{}-{}.jsonl",
            std::process::id(),
            now_ms()
        ));
        let turn = serde_json::json!({
            "type": "turn_context",
            "payload": { "turn_id": "turn-1" }
        });
        let question = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "request_user_input",
                "call_id": "call-1"
            }
        });
        std::fs::write(&path, format!("{turn}\n{question}\n")).unwrap();
        assert_eq!(
            read_codex_waiting_detail(&path, "turn-1").as_deref(),
            Some("Agent 正在等你回答")
        );

        let answer = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "answered"
            }
        });
        std::fs::write(&path, format!("{turn}\n{question}\n{answer}\n")).unwrap();
        assert_eq!(read_codex_waiting_detail(&path, "turn-1"), None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn codex_monitor_returns_to_working_after_waiting_is_answered() {
        let mut store = Store::default();
        let mut previous = HashMap::new();
        let waiting_turn = CodexTurn {
            turn_id: "turn-1".into(),
            thread_id: "thread-1".into(),
            name: "继续运行测试".into(),
            started_ms: 1,
            waiting_detail: Some("Agent 正在等你回答".into()),
        };

        sync_codex_turns(&mut store, &mut previous, vec![waiting_turn.clone()]);
        assert_eq!(store.sessions["codex:thread-1"].state, "waiting");

        sync_codex_turns(
            &mut store,
            &mut previous,
            vec![CodexTurn {
                waiting_detail: None,
                ..waiting_turn
            }],
        );
        assert_eq!(store.sessions["codex:thread-1"].state, "working");
        assert!(!store.sessions["codex:thread-1"].needs_ack);
    }

    #[test]
    fn zcode_monitor_returns_to_working_after_waiting_is_answered() {
        let mut store = Store::default();
        let mut previous = HashMap::new();
        let waiting_turn = ZcodeTurn {
            turn_id: "turn-1".into(),
            session_id: "session-1".into(),
            name: "继续运行测试".into(),
            started_ms: 1,
            waiting_detail: Some("Agent 正在等你回答".into()),
        };

        sync_zcode_turns(&mut store, &mut previous, vec![waiting_turn.clone()]);
        assert_eq!(store.sessions["zcode:session-1"].state, "waiting");

        sync_zcode_turns(
            &mut store,
            &mut previous,
            vec![ZcodeTurn {
                waiting_detail: None,
                ..waiting_turn
            }],
        );
        assert_eq!(store.sessions["zcode:session-1"].state, "working");
        assert!(!store.sessions["zcode:session-1"].needs_ack);
    }

    #[test]
    fn muted_codex_session_stays_hidden_until_a_new_turn() {
        let mut store = Store::default();
        store.muted.insert(
            "codex:thread-1".into(),
            MutedSession {
                activity_id: Some("turn-1".into()),
                muted_ms: 1,
            },
        );
        let mut previous = HashMap::new();
        let first = CodexTurn {
            turn_id: "turn-1".into(),
            thread_id: "thread-1".into(),
            name: "静音测试".into(),
            started_ms: 1,
            waiting_detail: Some("Agent 正在等你回答".into()),
        };
        let (changed, completed, waiting, mutes_changed) =
            sync_codex_turns(&mut store, &mut previous, vec![first]);
        assert!(!changed);
        assert_eq!(completed, 0);
        assert_eq!(waiting, 0);
        assert!(!mutes_changed);
        assert!(store.sessions.is_empty());
        assert!(store.muted.contains_key("codex:thread-1"));

        let second = CodexTurn {
            turn_id: "turn-2".into(),
            thread_id: "thread-1".into(),
            name: "静音测试".into(),
            started_ms: 2,
            waiting_detail: None,
        };
        let (changed, completed, waiting, mutes_changed) =
            sync_codex_turns(&mut store, &mut previous, vec![second]);
        assert!(changed);
        assert_eq!(completed, 0);
        assert_eq!(waiting, 0);
        assert!(mutes_changed);
        assert!(!store.muted.contains_key("codex:thread-1"));
        assert_eq!(store.sessions["codex:thread-1"].state, "working");
        assert_eq!(
            store.sessions["codex:thread-1"].activity_id.as_deref(),
            Some("turn-2")
        );
    }
}
