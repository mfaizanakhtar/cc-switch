//! Codex account rotation commands.

use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

use crate::services::codex_account_rotation::{
    self, RotationStatus,
};
use crate::settings;
use crate::store::AppState;

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

#[tauri::command]
pub async fn set_codex_rotation_anchor(
    app: AppHandle,
    provider_id: String,
) -> Result<RotationStatus, String> {
    let state = app.state::<AppState>();
    let providers = state
        .db
        .get_all_providers("codex")
        .map_err(|error| error.to_string())?;
    let anchor = providers
        .get(&provider_id)
        .ok_or_else(|| "Codex provider not found".to_string())?;
    if !crate::proxy::providers::is_codex_official_provider(anchor) {
        return Err("The anchor must be a Codex official account card".to_string());
    }
    if anchor
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
        .is_none()
    {
        return Err("The anchor card must be bound to a managed ChatGPT account".to_string());
    }

    for provider in providers.values() {
        let mut updated = provider.clone();
        let mut meta = updated.meta.clone().unwrap_or_default();
        meta.codex_rotation_pool = Some(updated.id == provider_id);
        updated.meta = Some(meta);
        state
            .db
            .save_provider("codex", &updated)
            .map_err(|error| error.to_string())?;
    }

    let mut current = settings::get_settings();
    current.codex_account_rotation.anchor_provider_id = provider_id;
    settings::update_settings(current).map_err(|error| error.to_string())?;
    codex_account_rotation::run_once(&app).await?;
    Ok(codex_account_rotation::status())
}
