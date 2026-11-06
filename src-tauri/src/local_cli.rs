//! A small, local command channel for the running desktop app.
//!
//! The CLI never opens the credential store itself. The GUI process owns the
//! existing account, quota, and provider switch paths.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::Manager;

use crate::app_config::AppType;
use crate::services::ProviderService;
use crate::store::AppState;

fn socket_path() -> PathBuf {
    crate::config::get_app_config_dir().join("cli.sock")
}

fn response(app: &tauri::AppHandle, request: &Value) -> Result<Value, String> {
    let state = app.state::<AppState>();
    match request.get("op").and_then(Value::as_str) {
        Some("accounts") => {
            let status = tauri::async_runtime::block_on(state.codex_oauth_manager.get_status());
            Ok(
                json!({"accounts": status.accounts.iter().map(|account| json!({
                "id": account.id,
                "login": account.login,
                "reauthRequired": account.reauth_required,
                "default": status.default_account_id.as_deref() == Some(account.id.as_str())
            })).collect::<Vec<_>>() }),
            )
        }
        Some("providers") => {
            let providers = state
                .db
                .get_all_providers("codex")
                .map_err(|e| e.to_string())?;
            let current = state
                .db
                .get_current_provider("codex")
                .map_err(|e| e.to_string())?;
            Ok(
                json!({"providers": providers.values().map(|provider| json!({
                "id": provider.id,
                "name": provider.name,
                "accountId": provider.meta.as_ref().and_then(|meta| meta.managed_account_id_for("codex_oauth")),
                "active": current.as_deref() == Some(provider.id.as_str())
            })).collect::<Vec<_>>() }),
            )
        }
        Some("use") => {
            let id = request
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .ok_or_else(|| "A provider ID is required".to_string())?;
            ProviderService::switch(state.inner(), AppType::Codex, id)
                .map_err(|e| e.to_string())?;
            Ok(json!({"selected": id}))
        }
        Some("limits") => {
            let status = tauri::async_runtime::block_on(state.codex_oauth_manager.get_status());
            let mut accounts = Vec::with_capacity(status.accounts.len());
            for account in status.accounts {
                let result =
                    tauri::async_runtime::block_on(crate::commands::query_codex_oauth_quota_for(
                        &state.codex_oauth_manager,
                        &account.id,
                    ));
                let limits = match result {
                    Ok(quota) => json!({
                        "available": quota.success,
                        "windows": quota.tiers.iter().map(|tier| json!({
                            "name": tier.name,
                            "usedPercent": tier.utilization,
                            "resetsAt": tier.resets_at
                        })).collect::<Vec<_>>()
                    }),
                    Err(_) => json!({"available": false, "windows": []}),
                };
                accounts.push(json!({"id": account.id, "login": account.login, "limits": limits}));
            }
            Ok(json!({"accounts": accounts}))
        }
        _ => Err("Unsupported command".to_string()),
    }
}

pub fn start(app: tauri::AppHandle) {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!("CLI socket directory unavailable: {error}");
            return;
        }
        if let Err(error) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        {
            log::warn!("CLI socket directory permission error: {error}");
            return;
        }
    }
    // The single-instance GUI owns this path. A stale socket is removable only
    // after checking that no process accepts a connection on it.
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            log::warn!("CLI socket already active");
            return;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!("CLI socket unavailable: {error}");
            return;
        }
    };
    if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        log::warn!("CLI socket permission error: {error}");
        let _ = std::fs::remove_file(&path);
        return;
    }
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
            let mut line = String::new();
            let result =
                if BufReader::new(&stream).read_line(&mut line).is_ok() && line.len() < 4096 {
                    serde_json::from_str::<Value>(&line)
                        .map_err(|_| "Invalid command".to_string())
                        .and_then(|request| response(&app, &request))
                } else {
                    Err("Invalid command".to_string())
                };
            let _ = writeln!(
                stream,
                "{}",
                json!({"ok": result.is_ok(), "result": result.as_ref().ok(), "error": result.err()})
            );
        }
    });
}

pub fn run(args: &[String]) -> i32 {
    let request = match args {
        [command] if command == "accounts" || command == "providers" || command == "limits" => {
            json!({"op": command})
        }
        [command, id] if command == "use" => json!({"op": "use", "id": id}),
        _ => {
            eprintln!("Usage: cc-switch codex-cli accounts|providers|limits|use <provider-id>");
            return 2;
        }
    };
    let mut stream = match UnixStream::connect(socket_path()) {
        Ok(stream) => stream,
        Err(_) => {
            eprintln!("CC Switch desktop app is not running in this user session");
            return 1;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(40)));
    if writeln!(stream, "{request}").is_err() {
        eprintln!("Could not send CLI command");
        return 1;
    }
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() {
        eprintln!("CC Switch did not answer");
        return 1;
    }
    let Ok(reply) = serde_json::from_str::<Value>(&line) else {
        eprintln!("Invalid CC Switch reply");
        return 1;
    };
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        println!("{}", reply["result"]);
        0
    } else {
        eprintln!(
            "{}",
            reply["error"]
                .as_str()
                .unwrap_or("CC Switch command failed")
        );
        1
    }
}
