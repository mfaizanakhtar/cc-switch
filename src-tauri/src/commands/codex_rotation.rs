//! Codex account rotation commands.

use std::time::{SystemTime, UNIX_EPOCH};

use tauri::AppHandle;

use crate::services::codex_account_rotation::{
    self, RotationStatus,
};
use crate::settings;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[tauri::command]
pub fn get_codex_rotation_status() -> RotationStatus {
    codex_account_rotation::status()
}

#[tauri::command]
pub async fn run_codex_rotation_now(app: AppHandle) -> Result<RotationStatus, String> {
    codex_account_rotation::run_once(&app).await?;
    Ok(codex_account_rotation::status())
}

#[tauri::command]
pub async fn set_codex_rotation_paused(
    app: AppHandle,
    paused: bool,
) -> Result<RotationStatus, String> {
    let mut current = settings::get_settings();
    current.codex_account_rotation.manual_paused = paused;
    settings::update_settings(current).map_err(|error| error.to_string())?;
    codex_account_rotation::run_once(&app).await?;
    Ok(codex_account_rotation::status())
}

#[tauri::command]
pub async fn set_codex_rotation_serving_account(
    app: AppHandle,
    account_id: Option<String>,
) -> Result<RotationStatus, String> {
    let current = settings::get_settings();
    if let Some(id) = account_id.as_deref() {
        if !current
            .codex_account_rotation
            .accounts
            .iter()
            .any(|account| account.account_id == id)
        {
            return Err("That account is not in the rotation pool".to_string());
        }
    }
    let until = now_millis()
        + (current.codex_account_rotation.manual_override_seconds as i64) * 1000;
    codex_account_rotation::set_manual_account(account_id, until);
    codex_account_rotation::run_once(&app).await?;
    Ok(codex_account_rotation::status())
}
