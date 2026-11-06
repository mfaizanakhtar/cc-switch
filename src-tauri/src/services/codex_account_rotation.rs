//! Codex account rotation.
//!
//! Codex stays on one stable official "anchor" card. The proxy validates the
//! client's anchor bearer, then swaps the upstream `Authorization` and
//! `chatgpt-account-id` to the selected serving account. Changing the serving
//! account never rewrites `auth.json` and never restarts Codex.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::provider::Provider;

static SELECTED_ACCOUNT: Mutex<Option<String>> = Mutex::new(None);
static LAST_REQUEST_AT: AtomicI64 = AtomicI64::new(0);
static ACTIVE_REQUESTS: AtomicUsize = AtomicUsize::new(0);

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
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

pub fn is_idle_for(seconds: u64) -> bool {
    if ACTIVE_REQUESTS.load(Ordering::SeqCst) != 0 {
        return false;
    }
    let last = LAST_REQUEST_AT.load(Ordering::SeqCst);
    last > 0 && now_millis().saturating_sub(last) >= (seconds as i64) * 1000
}
