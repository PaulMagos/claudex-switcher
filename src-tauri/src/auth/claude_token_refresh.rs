//! Claude Code OAuth token refresh helpers

use anyhow::{Context, Result};
use chrono::Utc;

use super::claude::switch_to_claude_account;
use super::claude_oauth::refresh_claude_tokens_via_api;
use super::{load_accounts, update_account_claude_tokens, AUTH_OPERATION_LOCK};
use crate::types::{AuthData, Provider, StoredAccount};

const EXPIRY_SKEW_MS: i64 = 60_000;

fn claude_tokens_need_refresh_at(expires_at: i64, now_ms: i64) -> bool {
    expires_at <= now_ms + EXPIRY_SKEW_MS
}

fn claude_tokens_need_refresh(account: &StoredAccount) -> bool {
    match &account.auth_data {
        AuthData::Claude { expires_at, .. } => {
            claude_tokens_need_refresh_at(*expires_at, Utc::now().timestamp_millis())
        }
        _ => false,
    }
}

/// Ensure the account has non-expired Claude Code OAuth tokens.
pub async fn ensure_claude_tokens_fresh(account: &StoredAccount) -> Result<StoredAccount> {
    if !claude_tokens_need_refresh(account) {
        return Ok(account.clone());
    }

    let _auth_guard = AUTH_OPERATION_LOCK.lock().await;
    ensure_claude_tokens_fresh_locked(account).await
}

pub(crate) async fn ensure_claude_tokens_fresh_locked(
    account: &StoredAccount,
) -> Result<StoredAccount> {
    if !matches!(account.auth_data, AuthData::Claude { .. }) {
        return Ok(account.clone());
    }

    let store = load_accounts()?;
    let current = store
        .accounts
        .into_iter()
        .find(|stored| stored.id == account.id)
        .context("Account not found")?;

    if claude_tokens_need_refresh(&current) {
        refresh_claude_tokens_locked(&current).await
    } else {
        Ok(current)
    }
}

/// Force-refresh Claude Code OAuth tokens for an account.
pub async fn refresh_claude_tokens(account: &StoredAccount) -> Result<StoredAccount> {
    if !matches!(account.auth_data, AuthData::Claude { .. }) {
        return Ok(account.clone());
    }

    let _auth_guard = AUTH_OPERATION_LOCK.lock().await;
    refresh_claude_tokens_locked(account).await
}

async fn refresh_claude_tokens_locked(account: &StoredAccount) -> Result<StoredAccount> {
    let refresh_token = match &account.auth_data {
        AuthData::Claude { refresh_token, .. } => refresh_token.clone(),
        _ => return Ok(account.clone()),
    };

    let is_active =
        load_accounts()?.active_id_for(Provider::Claude) == Some(account.id.as_str());

    // Refreshing the active account rewrites its live credentials file/Keychain
    // item. Skip while `claude` is running so a live session never has its
    // credentials swapped out from under it.
    if is_active && crate::commands::claude_process::ensure_claude_not_running().is_err() {
        return Ok(account.clone());
    }

    if refresh_token.trim().is_empty() {
        anyhow::bail!("Missing refresh token for account {}", account.name);
    }

    let (tokens, expires_at) = refresh_claude_tokens_via_api(&refresh_token).await?;
    let scopes = tokens
        .scope
        .map(|scope| scope.split(' ').map(str::to_string).collect());

    let updated = update_account_claude_tokens(
        &account.id,
        tokens.access_token,
        tokens.refresh_token,
        expires_at,
        scopes,
        None,
        None,
    )?;
    println!("[Auth] Refreshed Claude OAuth tokens for: {}", updated.name);

    // Re-read active state after the network request in case it changed while awaiting.
    let is_active =
        load_accounts()?.active_id_for(Provider::Claude) == Some(account.id.as_str());
    if is_active {
        if let Err(err) = switch_to_claude_account(&updated) {
            println!("[Auth] Failed to sync active Claude credentials after token refresh: {err}");
        }
    }

    Ok(updated)
}

/// Build a new Claude account from a refresh token. Used by slim import to
/// recreate full credentials from a compact export.
pub async fn create_claude_account_from_refresh_token(
    account_name: String,
    refresh_token: String,
) -> Result<StoredAccount> {
    if refresh_token.trim().is_empty() {
        anyhow::bail!("Missing refresh token for account {account_name}");
    }

    let (tokens, expires_at) = refresh_claude_tokens_via_api(&refresh_token).await?;
    let next_refresh_token = tokens.refresh_token;
    let scopes = tokens
        .scope
        .map(|scope| scope.split(' ').map(str::to_string).collect())
        .unwrap_or_default();

    Ok(StoredAccount::new_claude(
        account_name,
        None,
        None,
        tokens.access_token,
        next_refresh_token,
        expires_at,
        scopes,
    ))
}

#[cfg(test)]
mod tests {
    use super::claude_tokens_need_refresh_at;

    #[test]
    fn needs_refresh_when_past_expiry() {
        let now = 1_800_000_000_000;
        assert!(claude_tokens_need_refresh_at(now - 1, now));
    }

    #[test]
    fn needs_refresh_within_skew_window() {
        let now = 1_800_000_000_000;
        assert!(claude_tokens_need_refresh_at(now + 30_000, now));
    }

    #[test]
    fn does_not_need_refresh_well_before_expiry() {
        let now = 1_800_000_000_000;
        assert!(!claude_tokens_need_refresh_at(now + 3_600_000, now));
    }
}
