//! OAuth login Tauri commands

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

use crate::auth::claude_oauth::{exchange_claude_code_for_tokens, start_claude_oauth_login, ClaudePendingLogin};
use crate::auth::oauth_server::{start_oauth_login, wait_for_oauth_login, OAuthLoginResult};
use crate::auth::{
    add_account, load_accounts, set_active_account, switch_to_account, switch_to_claude_account,
    touch_account, AUTH_OPERATION_LOCK,
};
use crate::types::{AccountInfo, OAuthLoginInfo, StoredAccount};

struct PendingOAuth {
    rx: oneshot::Receiver<anyhow::Result<OAuthLoginResult>>,
    cancelled: Arc<AtomicBool>,
}

// Global state for pending OAuth login
static PENDING_OAUTH: Mutex<Option<PendingOAuth>> = Mutex::new(None);
// Global state for a pending Claude Code OAuth login awaiting a pasted code
static PENDING_CLAUDE_OAUTH: Mutex<Option<ClaudePendingLogin>> = Mutex::new(None);

/// Start the OAuth login flow
#[tauri::command]
pub async fn start_login(account_name: String) -> Result<OAuthLoginInfo, String> {
    // Cancel any previous pending flow so it does not keep the callback port occupied.
    if let Some(previous) = {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        pending.take()
    } {
        previous.cancelled.store(true, Ordering::Relaxed);
    }

    let (info, rx, cancelled) = start_oauth_login(account_name.trim().to_string())
        .await
        .map_err(|e| e.to_string())?;

    // Store the receiver for later
    {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        *pending = Some(PendingOAuth { rx, cancelled });
    }

    Ok(info)
}

/// Wait for the OAuth login to complete and add the account
#[tauri::command]
pub async fn complete_login() -> Result<AccountInfo, String> {
    let pending = {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        pending
            .take()
            .ok_or_else(|| "No pending OAuth login".to_string())?
    };

    let account = wait_for_oauth_login(pending.rx)
        .await
        .map_err(|e| e.to_string())?;

    let _auth_guard = AUTH_OPERATION_LOCK.lock().await;

    // Add the account to storage
    let stored = add_account(account).map_err(|e| e.to_string())?;

    // Make it active and switch to it
    set_active_account(&stored.id).map_err(|e| e.to_string())?;
    switch_to_account(&stored).map_err(|e| e.to_string())?;
    touch_account(&stored.id).map_err(|e| e.to_string())?;

    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();

    Ok(AccountInfo::from_stored(&stored, active_id))
}

/// Cancel a pending OAuth login
#[tauri::command]
pub async fn cancel_login() -> Result<(), String> {
    let mut pending = PENDING_OAUTH.lock().unwrap();
    if let Some(pending_oauth) = pending.take() {
        pending_oauth.cancelled.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Start the Claude Code OAuth login flow. Unlike Codex, this has no local
/// callback server: the caller must paste back the code Claude shows after
/// login via `complete_claude_login`.
#[tauri::command]
pub async fn start_claude_login(account_name: String) -> Result<OAuthLoginInfo, String> {
    let (info, pending) = start_claude_oauth_login(account_name.trim().to_string());

    let mut slot = PENDING_CLAUDE_OAUTH.lock().unwrap();
    *slot = Some(pending);

    Ok(info)
}

/// Exchange the pasted `CODE#STATE` value for tokens and add the account.
#[tauri::command]
pub async fn complete_claude_login(pasted_code: String) -> Result<AccountInfo, String> {
    let pending = {
        let mut slot = PENDING_CLAUDE_OAUTH.lock().unwrap();
        slot.take()
            .ok_or_else(|| "No pending Claude login".to_string())?
    };

    let (tokens, expires_at) = exchange_claude_code_for_tokens(&pending, &pasted_code)
        .await
        .map_err(|e| e.to_string())?;
    let scopes = tokens
        .scope
        .map(|scope| scope.split(' ').map(str::to_string).collect())
        .unwrap_or_default();

    let account = StoredAccount::new_claude(
        pending.account_name,
        None,
        None,
        tokens.access_token,
        tokens.refresh_token,
        expires_at,
        scopes,
    );

    let _auth_guard = AUTH_OPERATION_LOCK.lock().await;
    let stored = add_account(account).map_err(|e| e.to_string())?;
    set_active_account(&stored.id).map_err(|e| e.to_string())?;
    switch_to_claude_account(&stored).map_err(|e| e.to_string())?;
    touch_account(&stored.id).map_err(|e| e.to_string())?;

    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_id_for(stored.auth_mode.provider());
    Ok(AccountInfo::from_stored(&stored, active_id))
}

/// Cancel a pending Claude Code OAuth login
#[tauri::command]
pub async fn cancel_claude_login() -> Result<(), String> {
    let mut slot = PENDING_CLAUDE_OAUTH.lock().unwrap();
    slot.take();
    Ok(())
}
