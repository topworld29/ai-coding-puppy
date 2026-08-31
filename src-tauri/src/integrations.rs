use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

const ENDPOINT: &str = "http://127.0.0.1:7878/event";
const OPENCODE_BRIDGE: &str = include_str!("../resources/opencode-pet-bridge.ts");
const CLAUDE_EVENTS: &[&str] = &[
    "Elicitation",
    "ElicitationResult",
    "Notification",
    "PermissionRequest",
    "PostToolUse",
    "PreToolUse",
    "SessionEnd",
    "SessionStart",
    "Stop",
    "StopFailure",
    "UserPromptSubmit",
];
const ZCODE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub codex_changed: bool,
    pub claude_changed: bool,
    pub opencode_changed: bool,
    pub zcode_changed: bool,
}

pub fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

pub fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| user_home().map(|home| home.join(".codex")))
}

pub fn install_for_current_user() -> io::Result<InstallReport> {
    let home = user_home().ok_or_else(|| {
        io::Error::new(
            ErrorKind::NotFound,
            "USERPROFILE/HOME is unavailable; integrations were not installed",
        )
    })?;
    let codex_root = codex_home().unwrap_or_else(|| home.join(".codex"));
    let executable = std::env::current_exe()?;
    install_at(&home, &codex_root, &executable)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
}

fn read_json_object(path: &Path) -> io::Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| invalid_data(format!("{} is invalid JSON: {error}", path.display())))?;
    if !value.is_object() {
        return Err(invalid_data(format!(
            "{} must contain a JSON object",
            path.display()
        )));
    }
    Ok(value)
}

fn backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("settings.json");
    path.with_file_name(format!("{file_name}.golden-puppy.bak"))
}

fn write_if_changed(path: &Path, contents: &str, backup_existing: bool) -> io::Result<bool> {
    if fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if backup_existing && path.exists() {
        fs::copy(path, backup_path(path))?;
    }
    fs::write(path, contents)?;
    Ok(true)
}

fn write_json_if_changed(path: &Path, value: &Value) -> io::Result<bool> {
    let mut serialized = serde_json::to_string_pretty(value).map_err(|error| {
        invalid_data(format!("failed to serialize {}: {error}", path.display()))
    })?;
    serialized.push('\n');
    write_if_changed(path, &serialized, true)
}

fn quoted_executable(executable: &Path) -> String {
    format!("\"{}\"", executable.to_string_lossy().replace('"', "\\\""))
}

fn codex_command(executable: &Path, mode: &str) -> String {
    format!("{} --pet-hook {mode}", quoted_executable(executable))
}

fn codex_hook(executable: &Path, mode: &str) -> Value {
    let command = codex_command(executable, mode);
    json!({
        "type": "command",
        "command": command,
        "commandWindows": command,
        "async": true,
        "timeout": 3
    })
}

fn is_managed_codex_group(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .or_else(|| hook.get("commandWindows").and_then(Value::as_str))
                    .is_some_and(|command| command.contains("--pet-hook"))
            })
        })
}

fn array_for_event<'a>(
    hooks: &'a mut Map<String, Value>,
    event: &str,
) -> io::Result<&'a mut Vec<Value>> {
    hooks
        .entry(event.to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| invalid_data(format!("hooks.{event} must be an array")))
}

fn install_codex_hooks(path: &Path, executable: &Path) -> io::Result<bool> {
    let mut root = read_json_object(path)?;
    let root_object = root.as_object_mut().expect("validated JSON object");
    let hooks = root_object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid_data("Codex hooks must be a JSON object"))?;

    for entries in hooks.values_mut() {
        if let Some(entries) = entries.as_array_mut() {
            entries.retain(|group| !is_managed_codex_group(group));
        }
    }
    hooks.retain(|_, entries| !entries.as_array().is_some_and(Vec::is_empty));

    array_for_event(hooks, "PreToolUse")?.push(json!({
        "matcher": "^(request_user_input|request_permissions)$",
        "hooks": [codex_hook(executable, "waiting")]
    }));
    array_for_event(hooks, "PermissionRequest")?.push(json!({
        "matcher": ".*",
        "hooks": [codex_hook(executable, "waiting")]
    }));
    array_for_event(hooks, "PostToolUse")?.push(json!({
        "matcher": "^(Bash|apply_patch|mcp__.*|request_user_input|request_permissions)$",
        "hooks": [codex_hook(executable, "working")]
    }));

    write_json_if_changed(path, &root)
}

fn is_managed_claude_hook(hook: &Value) -> bool {
    hook.get("type").and_then(Value::as_str) == Some("http")
        && hook.get("url").and_then(Value::as_str) == Some(ENDPOINT)
}

fn install_claude_hooks(path: &Path) -> io::Result<bool> {
    let mut root = read_json_object(path)?;
    let root_object = root.as_object_mut().expect("validated JSON object");
    let hooks = root_object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid_data("Claude hooks must be a JSON object"))?;

    for groups in hooks.values_mut() {
        if let Some(groups) = groups.as_array_mut() {
            groups.retain_mut(|group| {
                let Some(actions) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                    return true;
                };
                let previous_len = actions.len();
                actions.retain(|action| !is_managed_claude_hook(action));
                previous_len == actions.len() || !actions.is_empty()
            });
        }
    }
    hooks.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));

    for event in CLAUDE_EVENTS {
        array_for_event(hooks, event)?.push(json!({
            "hooks": [{
                "type": "http",
                "url": ENDPOINT,
                "timeout": 3
            }]
        }));
    }

    write_json_if_changed(path, &root)
}

fn zcode_hook(executable: &Path) -> Value {
    json!({
        "type": "process",
        "command": executable.to_string_lossy(),
        "args": ["--pet-hook", "auto", "zcode"],
        "enabled": true,
        "timeoutMs": 3000
    })
}

fn is_managed_zcode_group(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                let args = hook.get("args").and_then(Value::as_array);
                hook.get("type").and_then(Value::as_str) == Some("process")
                    && args.is_some_and(|args| {
                        args.iter().any(|arg| arg.as_str() == Some("--pet-hook"))
                            && args.iter().any(|arg| arg.as_str() == Some("zcode"))
                    })
            })
        })
}

fn install_zcode_hooks(path: &Path, executable: &Path) -> io::Result<bool> {
    let mut root = read_json_object(path)?;
    let root_object = root.as_object_mut().expect("validated JSON object");
    let hooks = root_object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid_data("ZCode hooks must be a JSON object"))?;
    hooks.insert("enabled".to_string(), Value::Bool(true));
    let events = hooks
        .entry("events")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid_data("ZCode hooks.events must be a JSON object"))?;

    for groups in events.values_mut() {
        if let Some(groups) = groups.as_array_mut() {
            groups.retain(|group| !is_managed_zcode_group(group));
        }
    }
    events.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));

    for event in ZCODE_EVENTS {
        array_for_event(events, event)?.push(json!({
            "hooks": [zcode_hook(executable)]
        }));
    }

    write_json_if_changed(path, &root)
}

fn install_at(home: &Path, codex_root: &Path, executable: &Path) -> io::Result<InstallReport> {
    let codex_changed = install_codex_hooks(&codex_root.join("hooks.json"), executable)?;
    let claude_changed = install_claude_hooks(&home.join(".claude").join("settings.json"))?;
    let opencode_changed = write_if_changed(
        &home
            .join(".config")
            .join("opencode")
            .join("plugins")
            .join("golden-pet")
            .join("pet-bridge.ts"),
        OPENCODE_BRIDGE,
        false,
    )?;
    let zcode_changed = install_zcode_hooks(
        &home.join(".zcode").join("cli").join("config.json"),
        executable,
    )?;

    Ok(InstallReport {
        codex_changed,
        claude_changed,
        opencode_changed,
        zcode_changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_home(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "golden-puppy-integrations-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn installs_for_another_windows_user_without_overwriting_existing_hooks() {
        let home = temporary_home("portable");
        let codex_root = home.join(".codex");
        fs::create_dir_all(&codex_root).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".zcode").join("cli")).unwrap();
        fs::write(
            codex_root.join("hooks.json"),
            r#"{"custom":"keep","hooks":{"PreToolUse":[{"matcher":"Shell","hooks":[{"type":"command","command":"custom-hook"}]}]}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"theme":"keep","hooks":{"Stop":[{"matcher":"custom","hooks":[{"type":"command","command":"custom-hook"}]}]}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".zcode").join("cli").join("config.json"),
            r#"{"mcp":{"servers":{"keep":{"type":"stdio","command":"custom-mcp"}}},"hooks":{"enabled":false,"events":{"Stop":[{"hooks":[{"type":"process","command":"custom-zcode-hook"}]}]}}}"#,
        )
        .unwrap();
        let executable =
            PathBuf::from(r"C:\Users\Another Person\AppData\Local\小金毛桌宠\golden-puppy-pet.exe");

        let first = install_at(&home, &codex_root, &executable).unwrap();
        assert_eq!(
            first,
            InstallReport {
                codex_changed: true,
                claude_changed: true,
                opencode_changed: true,
                zcode_changed: true,
            }
        );
        let second = install_at(&home, &codex_root, &executable).unwrap();
        assert_eq!(second, InstallReport::default());

        let codex_raw = fs::read_to_string(codex_root.join("hooks.json")).unwrap();
        assert!(codex_raw.contains("custom-hook"));
        assert!(codex_raw.contains(r"C:\\Users\\Another Person"));
        // 写入的必须是传入的 exe 路径，而不是开发机 current_exe() 的真实用户目录
        let all_user_paths = codex_raw.matches(r"C:\\Users\\").count();
        let fictional_paths = codex_raw.matches(r"C:\\Users\\Another Person").count();
        assert_eq!(all_user_paths, fictional_paths);
        assert_eq!(codex_raw.matches("--pet-hook waiting").count(), 4);
        assert_eq!(codex_raw.matches("--pet-hook working").count(), 2);

        let claude_raw = fs::read_to_string(home.join(".claude").join("settings.json")).unwrap();
        assert!(claude_raw.contains("custom-hook"));
        assert!(claude_raw.contains("\"theme\": \"keep\""));
        assert_eq!(claude_raw.matches(ENDPOINT).count(), CLAUDE_EVENTS.len());

        let bridge = fs::read_to_string(
            home.join(".config")
                .join("opencode")
                .join("plugins")
                .join("golden-pet")
                .join("pet-bridge.ts"),
        )
        .unwrap();
        assert_eq!(bridge, OPENCODE_BRIDGE);

        let zcode_raw =
            fs::read_to_string(home.join(".zcode").join("cli").join("config.json")).unwrap();
        assert!(zcode_raw.contains("custom-mcp"));
        assert!(zcode_raw.contains("custom-zcode-hook"));
        assert!(zcode_raw.contains("\"enabled\": true"));
        assert_eq!(zcode_raw.matches("--pet-hook").count(), ZCODE_EVENTS.len());
        assert_eq!(zcode_raw.matches("\"zcode\"").count(), ZCODE_EVENTS.len());

        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn replaces_only_previous_golden_pet_entries() {
        let home = temporary_home("upgrade");
        let codex_root = home.join(".codex");
        fs::create_dir_all(&codex_root).unwrap();
        fs::write(
            codex_root.join("hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"old","hooks":[{"type":"command","command":"\"C:\\Users\\Old\\golden-puppy-pet.exe\" --pet-hook waiting"}]}]}}"#,
        )
        .unwrap();
        let executable = PathBuf::from(r"D:\Apps\Golden Pet\golden-puppy-pet.exe");
        install_at(&home, &codex_root, &executable).unwrap();
        let raw = fs::read_to_string(codex_root.join("hooks.json")).unwrap();
        assert!(!raw.contains(r"C:\\Users\\Old"));
        assert!(raw.contains(r"D:\\Apps\\Golden Pet"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn refuses_to_overwrite_an_invalid_existing_config() {
        let home = temporary_home("invalid");
        let codex_root = home.join(".codex");
        fs::create_dir_all(&codex_root).unwrap();
        let path = codex_root.join("hooks.json");
        fs::write(&path, "{ definitely not json").unwrap();

        let error = install_at(
            &home,
            &codex_root,
            Path::new(r"C:\Portable\golden-puppy-pet.exe"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(path).unwrap(), "{ definitely not json");
        fs::remove_dir_all(home).unwrap();
    }
}
