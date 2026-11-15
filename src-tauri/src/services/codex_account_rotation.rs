//! Codex account rotation.
//!
//! Codex stays on one stable official "anchor" card. The proxy validates the
//! client's anchor bearer, then swaps the upstream `Authorization` and
//! `chatgpt-account-id` to the selected serving account. Changing the serving
//! account never rewrites `auth.json` and never restarts Codex.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::provider::Provider;
use crate::settings::{CodexAccountRotation, CodexRotationAccount};
use crate::store::AppState;

static SELECTED_ACCOUNT: Mutex<Option<String>> = Mutex::new(None);
static MANUAL_UNTIL: AtomicI64 = AtomicI64::new(0);
static LAST_REQUEST_AT: AtomicI64 = AtomicI64::new(0);
static LAST_HEARTBEAT_AT: AtomicI64 = AtomicI64::new(0);
static ACTIVE_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static STATUS: OnceLock<Mutex<RotationStatus>> = OnceLock::new();
static RUNTIME: OnceLock<Mutex<RotationRuntime>> = OnceLock::new();

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn status_store() -> &'static Mutex<RotationStatus> {
    STATUS.get_or_init(|| Mutex::new(RotationStatus::default()))
}

fn runtime_store() -> &'static Mutex<RotationRuntime> {
    RUNTIME.get_or_init(|| Mutex::new(RotationRuntime::default()))
}

/// The stable Codex card whose proxy requests use the rotation pool.
pub fn is_rotation_provider(provider: &Provider) -> bool {
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.codex_rotation_pool)
        .unwrap_or(false)
}

pub fn selected_account_id() -> Option<String> {
    SELECTED_ACCOUNT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

pub fn set_selected_account_id(account_id: Option<String>) {
    let mut guard = SELECTED_ACCOUNT
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *guard = account_id;
}

/// Manual selection from the UI. The engine leaves it alone for the grace period.
pub fn set_manual_account(account_id: Option<String>, until_millis: i64) {
    set_selected_account_id(account_id);
    MANUAL_UNTIL.store(until_millis, Ordering::SeqCst);
}

pub fn manual_until() -> i64 {
    MANUAL_UNTIL.load(Ordering::SeqCst)
}

pub fn status() -> RotationStatus {
    status_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn update_status(update: impl FnOnce(&mut RotationStatus)) {
    let mut guard = status_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    update(&mut guard);
}

/// Marks a Codex request in flight. The last-request timestamp updates on both
/// edges so a long stream never looks idle.
pub struct ActivityGuard;

impl ActivityGuard {
    pub fn start() -> Self {
        ACTIVE_REQUESTS.fetch_add(1, Ordering::SeqCst);
        LAST_REQUEST_AT.store(now_millis(), Ordering::SeqCst);
        Self
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        LAST_REQUEST_AT.store(now_millis(), Ordering::SeqCst);
        ACTIVE_REQUESTS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[allow(dead_code)]
pub fn is_idle_for(seconds: u64) -> bool {
    if ACTIVE_REQUESTS.load(Ordering::SeqCst) != 0 {
        return false;
    }
    let last = LAST_REQUEST_AT.load(Ordering::SeqCst);
    last > 0 && now_millis().saturating_sub(last) >= (seconds as i64) * 1000
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSnapshot {
    pub five_hour_used: Option<f64>,
    pub five_hour_reset: Option<i64>,
    pub weekly_used: Option<f64>,
    pub weekly_reset: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExhaustedUntil {
    pub five_hour: Option<i64>,
    pub weekly: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RotationRuntime {
    pub exhausted_until: HashMap<String, ExhaustedUntil>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RotationDecision {
    pub target: Option<String>,
    pub paused: bool,
    pub reason: String,
    pub next_reset: Option<i64>,
}

/// Pure policy: choose the highest-priority account whose remaining quota is
/// above both floors. Sticky exhaustion is stored in `runtime`.
pub fn choose_target(
    config: &CodexAccountRotation,
    usage: &HashMap<String, UsageSnapshot>,
    runtime: &mut RotationRuntime,
    now: i64,
) -> RotationDecision {
    let mut accounts: Vec<&CodexRotationAccount> = config.accounts.iter().collect();
    accounts.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });

    let mut next_reset: Option<i64> = None;

    for account in accounts {
        let Some(snapshot) = usage.get(&account.account_id) else {
            continue;
        };
        let mut exhausted = runtime
            .exhausted_until
            .get(&account.account_id)
            .copied()
            .unwrap_or_default();
        let mut blocked = false;

        // 5-hour window.
        if let Some(until) = exhausted.five_hour {
            if now < until {
                blocked = true;
                next_reset = min_reset(next_reset, Some(until));
            } else {
                exhausted.five_hour = None;
            }
        }
        if !blocked {
            match snapshot.five_hour_used {
                Some(used) => {
                    let floor = 100.0 - f64::from(account.five_hour_min_remaining);
                    if used >= floor {
                        let until = snapshot.five_hour_reset.unwrap_or(now + 5 * 3600 * 1000);
                        exhausted.five_hour = Some(until);
                        blocked = true;
                        next_reset = min_reset(next_reset, Some(until));
                    }
                }
                None => blocked = true,
            }
        }

        // Weekly window.
        if !blocked {
            if let Some(until) = exhausted.weekly {
                if now < until {
                    blocked = true;
                    next_reset = min_reset(next_reset, Some(until));
                } else {
                    exhausted.weekly = None;
                }
            }
            if !blocked {
                match snapshot.weekly_used {
                    Some(used) => {
                        let floor = 100.0 - f64::from(account.weekly_min_remaining);
                        if used >= floor {
                            let until = snapshot.weekly_reset.unwrap_or(now + 7 * 24 * 3600 * 1000);
                            exhausted.weekly = Some(until);
                            blocked = true;
                            next_reset = min_reset(next_reset, Some(until));
                        }
                    }
                    None => blocked = true,
                }
            }
        }

        runtime.exhausted_until.insert(account.account_id.clone(), exhausted);
        if !blocked {
            return RotationDecision {
                target: Some(account.account_id.clone()),
                paused: false,
                reason: format!("selected priority {} ({})", account.priority, account.login),
                next_reset,
            };
        }
    }

    RotationDecision {
        target: None,
        paused: true,
        reason: "every account is at or below its remaining-quota floor".to_string(),
        next_reset,
    }
}

fn min_reset(current: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    match (current, candidate) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RotationStatus {
    pub running: bool,
    pub enabled: bool,
    pub auto_paused: bool,
    pub manual_paused: bool,
    pub anchor_provider_id: Option<String>,
    pub anchor_login: Option<String>,
    pub selected_account_id: Option<String>,
    pub selected_login: Option<String>,
    pub next_reset: Option<i64>,
    pub last_decision_at: Option<i64>,
    pub last_switch_at: Option<i64>,
    pub last_heartbeat_at: Option<i64>,
    pub last_error: Option<String>,
    pub accounts: Vec<RotationAccountStatus>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RotationAccountStatus {
    pub account_id: String,
    pub login: String,
    pub priority: u8,
    pub five_hour_used: Option<f64>,
    pub five_hour_reset: Option<i64>,
    pub weekly_used: Option<f64>,
    pub weekly_reset: Option<i64>,
    pub five_hour_min_remaining: u8,
    pub weekly_min_remaining: u8,
    pub eligible: bool,
    pub reauth_required: bool,
    pub provider_id: Option<String>,
}

pub fn spawn(app: tauri::AppHandle) {
    LAST_REQUEST_AT.store(now_millis(), Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        update_status(|status| status.running = true);
        loop {
            let poll_seconds = match run_once(&app).await {
                Ok(seconds) => seconds,
                Err(error) => {
                    log::warn!("[CodexRotation] {error}");
                    update_status(|status| status.last_error = Some(error));
                    30
                }
            };
            tokio::time::sleep(Duration::from_secs(poll_seconds.max(5))).await;
        }
    });
}

pub(crate) async fn run_once(app: &tauri::AppHandle) -> Result<u64, String> {
    let settings = crate::settings::get_settings();
    let config = settings.codex_account_rotation;
    let poll_seconds = config.poll_seconds.max(5);
    let now = now_millis();

    if !config.enabled {
        update_status(|status| {
            status.enabled = false;
            status.auto_paused = false;
            status.manual_paused = config.manual_paused;
            status.last_error = None;
        });
        emit_status(app);
        return Ok(poll_seconds);
    }

    let state = app.state::<AppState>();
    let current_provider = state
        .db
        .get_current_provider("codex")
        .map_err(|error| format!("读取 Codex 当前供应商失败: {error}"))?;
    let providers = state
        .db
        .get_all_providers("codex")
        .map_err(|error| format!("读取 Codex 供应商失败: {error}"))?;
    let anchor = providers
        .get(&config.anchor_provider_id)
        .filter(|provider| is_rotation_provider(provider));
    let Some(anchor) = anchor else {
        return Err("rotation anchor card is missing or not marked as a rotation pool".to_string());
    };
    let anchor_account_id = anchor
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("codex_oauth"));
    let anchor_active = current_provider.as_deref() == Some(config.anchor_provider_id.as_str());
    if !anchor_active {
        update_status(|status| {
            status.enabled = true;
            status.auto_paused = true;
            status.manual_paused = config.manual_paused;
            status.anchor_provider_id = Some(config.anchor_provider_id.clone());
            status.anchor_login = None;
            status.last_error =
                Some("the rotation anchor card is not the active Codex provider".to_string());
            status.last_decision_at = Some(now);
        });
        emit_status(app);
        return Ok(poll_seconds);
    }

    let oauth_status = state
        .codex_oauth_manager
        .get_status()
        .await;
    let oauth_accounts: HashMap<String, (String, bool)> = oauth_status
        .accounts
        .into_iter()
        .map(|account| (account.id, (account.login, account.reauth_required)))
        .collect();

    let anchor_login = anchor_account_id
        .as_ref()
        .and_then(|id| oauth_accounts.get(id).map(|(login, _)| login.clone()));

    let mut usage = HashMap::new();
    let mut account_statuses = Vec::new();
    let mut near_cap = false;
    for account in &config.accounts {
        let (login, reauth_required) = oauth_accounts
            .get(&account.account_id)
            .cloned()
            .unwrap_or_else(|| (account.login.clone(), true));
        let provider_id = exactly_one_provider(&providers, &account.account_id);
        let snapshot = match crate::commands::query_codex_oauth_quota_for(
            &state.codex_oauth_manager,
            &account.account_id,
        )
        .await
        {
            Ok(quota) => snapshot_from_quota(&quota),
            Err(error) => {
                log::warn!(
                    "[CodexRotation] quota unavailable for {}: {error}",
                    account.account_id
                );
                UsageSnapshot::default()
            }
        };
        if let Some(used) = snapshot.five_hour_used {
            if used >= 100.0 - f64::from(account.five_hour_min_remaining) - f64::from(config.near_cap_margin) {
                near_cap = true;
            }
        }
        if let Some(used) = snapshot.weekly_used {
            if used >= 100.0 - f64::from(account.weekly_min_remaining) - f64::from(config.near_cap_margin) {
                near_cap = true;
            }
        }
        let eligible = provider_id.is_some() && !reauth_required;
        account_statuses.push(RotationAccountStatus {
            account_id: account.account_id.clone(),
            login: login.clone(),
            priority: account.priority,
            five_hour_used: snapshot.five_hour_used,
            five_hour_reset: snapshot.five_hour_reset,
            weekly_used: snapshot.weekly_used,
            weekly_reset: snapshot.weekly_reset,
            five_hour_min_remaining: account.five_hour_min_remaining,
            weekly_min_remaining: account.weekly_min_remaining,
            eligible,
            reauth_required,
            provider_id,
        });
        usage.insert(account.account_id.clone(), snapshot);
    }

    let manual_active = now < manual_until();
    let decision = if manual_active {
        RotationDecision {
            target: selected_account_id(),
            paused: false,
            reason: "manual selection".to_string(),
            next_reset: None,
        }
    } else {
        let mut runtime = runtime_store()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        choose_target(&config, &usage, &mut runtime, now)
    };

    if decision.paused {
        log::info!(
            "[CodexRotation] paused: {} (next reset {:?})",
            decision.reason,
            decision.next_reset
        );
    }

    if config.manual_paused {
        update_status(|status| {
            status.enabled = true;
            status.auto_paused = false;
            status.manual_paused = true;
            status.anchor_provider_id = Some(config.anchor_provider_id.clone());
            status.anchor_login = anchor_login.clone();
            status.selected_account_id = selected_account_id();
            status.next_reset = decision.next_reset;
            status.last_decision_at = Some(now);
            status.accounts = account_statuses;
            status.last_error = None;
        });
        emit_status(app);
        return Ok(poll_seconds);
    }

    let previous = selected_account_id();
    if let Some(target) = decision.target.clone() {
        set_selected_account_id(Some(target.clone()));
        if previous.as_deref() != Some(target.as_str()) {
            log::info!("[CodexRotation] serving account -> {target}");
            update_status(|status| status.last_switch_at = Some(now));
        }
    }

    let selected = selected_account_id();
    let selected_login = selected.as_ref().and_then(|id| {
        account_statuses
            .iter()
            .find(|account| &account.account_id == id)
            .map(|account| account.login.clone())
    });

    maybe_heartbeat(&config, selected.as_deref(), decision.paused, now).await;

    update_status(|status| {
        status.enabled = true;
        status.auto_paused = decision.paused;
        status.manual_paused = config.manual_paused;
        status.anchor_provider_id = Some(config.anchor_provider_id.clone());
        status.anchor_login = anchor_login.clone();
        status.selected_account_id = selected;
        status.selected_login = selected_login;
        status.next_reset = decision.next_reset;
        status.last_decision_at = Some(now);
        status.accounts = account_statuses;
        status.last_error = None;
        status.last_heartbeat_at = (LAST_HEARTBEAT_AT.load(Ordering::SeqCst) > 0)
            .then(|| LAST_HEARTBEAT_AT.load(Ordering::SeqCst));
    });

    emit_status(app);

    Ok(if near_cap {
        config.fast_poll_seconds.max(5)
    } else {
        poll_seconds
    })
}

async fn maybe_heartbeat(
    config: &CodexAccountRotation,
    selected: Option<&str>,
    paused: bool,
    now: i64,
) {
    if !config.heartbeat.enabled || config.manual_paused || paused {
        return;
    }
    let Some(selected) = selected else { return };
    let Some(rule) = config
        .accounts
        .iter()
        .find(|account| account.account_id == selected)
    else {
        return;
    };
    if !rule.heartbeat_eligible {
        return;
    }
    if config.heartbeat.target != "preferred" && config.heartbeat.target != selected {
        return;
    }
    if !is_idle_for(config.heartbeat.idle_minutes.saturating_mul(60)) {
        return;
    }
    let last = LAST_HEARTBEAT_AT.load(Ordering::SeqCst);
    let repeat_ms = (config.heartbeat.repeat_minutes as i64) * 60 * 1000;
    if last > 0 && now.saturating_sub(last) < repeat_ms {
        return;
    }

    let cwd = std::path::PathBuf::from("/home/ai/workspaces/rotation-heartbeat");
    if let Err(error) = std::fs::create_dir_all(&cwd) {
        log::warn!("[CodexRotation] heartbeat workspace failed: {error}");
        return;
    }
    let codex = if std::path::Path::new("/usr/local/bin/codex").exists() {
        "/usr/local/bin/codex"
    } else {
        "codex"
    };
    let mut command = tokio::process::Command::new(codex);
    command
        .args([
            "exec",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--color",
            "never",
            "Reply with exactly OK",
        ])
        .current_dir(&cwd)
        .env("HOME", "/home/ai")
        .env("CODEX_HOME", "/home/ai/.codex")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    match tokio::time::timeout(Duration::from_secs(120), command.status()).await {
        Ok(Ok(status)) if status.success() => {
            LAST_HEARTBEAT_AT.store(now, Ordering::SeqCst);
            log::info!(
                "[CodexRotation] heartbeat started a window for serving account {selected}"
            );
        }
        Ok(Ok(status)) => {
            // Retry sooner than a full repeat interval after a failure.
            LAST_HEARTBEAT_AT.store(now - repeat_ms + 5 * 60 * 1000, Ordering::SeqCst);
            log::warn!("[CodexRotation] heartbeat exited with {status} for {selected}");
        }
        Ok(Err(error)) => {
            LAST_HEARTBEAT_AT.store(now - repeat_ms + 5 * 60 * 1000, Ordering::SeqCst);
            log::warn!("[CodexRotation] heartbeat could not start codex: {error}");
        }
        Err(_) => {
            LAST_HEARTBEAT_AT.store(now - repeat_ms + 5 * 60 * 1000, Ordering::SeqCst);
            log::warn!("[CodexRotation] heartbeat timed out for {selected}");
        }
    }
}

fn emit_status(app: &tauri::AppHandle) {
    if let Err(error) = app.emit("codex-rotation-status", status()) {
        log::debug!("[CodexRotation] emit failed: {error}");
    }
}

fn exactly_one_provider(
    providers: &indexmap::IndexMap<String, Provider>,
    account_id: &str,
) -> Option<String> {
    let mut matches = providers.values().filter(|provider| {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
            .as_deref()
            == Some(account_id)
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(first.id.clone())
    }
}

fn snapshot_from_quota(quota: &crate::services::subscription::SubscriptionQuota) -> UsageSnapshot {
    let mut snapshot = UsageSnapshot::default();
    for tier in &quota.tiers {
        match tier.name.as_str() {
            crate::services::subscription::TIER_FIVE_HOUR => {
                snapshot.five_hour_used = Some(tier.utilization);
                snapshot.five_hour_reset = parse_reset(&tier.resets_at);
            }
            crate::services::subscription::TIER_SEVEN_DAY
            | crate::services::subscription::TIER_WEEKLY_LIMIT => {
                snapshot.weekly_used = Some(tier.utilization);
                snapshot.weekly_reset = parse_reset(&tier.resets_at);
            }
            _ => {}
        }
    }
    snapshot
}

fn parse_reset(value: &Option<String>) -> Option<i64> {
    value
        .as_deref()
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|time| time.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, priority: u8, five: u8, weekly: u8) -> CodexRotationAccount {
        CodexRotationAccount {
            account_id: id.to_string(),
            login: format!("{id}@example.com"),
            priority,
            five_hour_min_remaining: five,
            weekly_min_remaining: weekly,
            heartbeat_eligible: false,
        }
    }

    fn config(accounts: Vec<CodexRotationAccount>) -> CodexAccountRotation {
        CodexAccountRotation {
            enabled: true,
            anchor_provider_id: "anchor".to_string(),
            accounts,
            ..Default::default()
        }
    }

    fn usage(five: f64, weekly: f64) -> UsageSnapshot {
        UsageSnapshot {
            five_hour_used: Some(five),
            five_hour_reset: Some(1_000_000),
            weekly_used: Some(weekly),
            weekly_reset: Some(2_000_000),
        }
    }

    #[test]
    fn picks_highest_priority_eligible_account() {
        let config = config(vec![account("a", 1, 30, 20), account("b", 2, 5, 0)]);
        let mut usage_map = HashMap::new();
        usage_map.insert("a".to_string(), usage(10.0, 10.0));
        usage_map.insert("b".to_string(), usage(10.0, 10.0));
        let mut runtime = RotationRuntime::default();
        let decision = choose_target(&config, &usage_map, &mut runtime, 0);
        assert_eq!(decision.target.as_deref(), Some("a"));
    }

    #[test]
    fn switches_when_remaining_floor_is_reached() {
        let config = config(vec![account("a", 1, 30, 20), account("b", 2, 5, 0)]);
        let mut usage_map = HashMap::new();
        usage_map.insert("a".to_string(), usage(70.0, 10.0));
        usage_map.insert("b".to_string(), usage(10.0, 10.0));
        let mut runtime = RotationRuntime::default();
        let decision = choose_target(&config, &usage_map, &mut runtime, 0);
        assert_eq!(decision.target.as_deref(), Some("b"));
        assert!(runtime.exhausted_until.contains_key("a"));
    }

    #[test]
    fn sticky_until_reset() {
        let config = config(vec![account("a", 1, 30, 20), account("b", 2, 5, 0)]);
        let mut usage_map = HashMap::new();
        usage_map.insert("a".to_string(), usage(70.0, 10.0));
        usage_map.insert("b".to_string(), usage(10.0, 10.0));
        let mut runtime = RotationRuntime::default();
        assert_eq!(
            choose_target(&config, &usage_map, &mut runtime, 0)
                .target
                .as_deref(),
            Some("b")
        );
        // Still exhausted until the reset, even if the reading drops.
        usage_map.insert("a".to_string(), usage(0.0, 0.0));
        assert_eq!(
            choose_target(&config, &usage_map, &mut runtime, 500_000)
                .target
                .as_deref(),
            Some("b")
        );
        // After the reset, the preferred account returns.
        assert_eq!(
            choose_target(&config, &usage_map, &mut runtime, 1_000_001)
                .target
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn all_exhausted_pauses() {
        let config = config(vec![account("a", 1, 30, 20), account("b", 2, 5, 0)]);
        let mut usage_map = HashMap::new();
        usage_map.insert("a".to_string(), usage(100.0, 100.0));
        usage_map.insert("b".to_string(), usage(100.0, 100.0));
        let mut runtime = RotationRuntime::default();
        let decision = choose_target(&config, &usage_map, &mut runtime, 0);
        assert!(decision.paused);
        assert!(decision.target.is_none());
        assert!(decision.next_reset.is_some());
    }

    #[test]
    fn unknown_usage_is_never_selected() {
        let config = config(vec![account("a", 1, 30, 20), account("b", 2, 5, 0)]);
        let mut usage_map = HashMap::new();
        usage_map.insert("b".to_string(), usage(10.0, 10.0));
        let mut runtime = RotationRuntime::default();
        assert_eq!(
            choose_target(&config, &usage_map, &mut runtime, 0)
                .target
                .as_deref(),
            Some("b")
        );
    }
}
