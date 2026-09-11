//! Usage/warm-up client for Claude Code OAuth and Anthropic API-key accounts.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::DateTime;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, USER_AGENT},
    StatusCode,
};
use serde_json::json;

use crate::auth::{ensure_claude_tokens_fresh, refresh_claude_tokens};
use crate::types::{AuthData, ClaudeProfilePayload, ClaudeUsagePayload, StoredAccount, UsageInfo};

const ANTHROPIC_API: &str = "https://api.anthropic.com";
const OAUTH_BETA_HEADER_VALUE: &str = "oauth-2025-04-20";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const WARMUP_MODEL: &str = "claude-haiku-4-5-20251001";
// Anthropic's OAuth beta rejects requests unless the system prompt identifies
// the client as Claude Code; this is required for OAuth (not API-key) warm-up.
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const SESSION_WINDOW_MINUTES: i64 = 5 * 60;
const WEEKLY_WINDOW_MINUTES: i64 = 7 * 24 * 60;
const USER_AGENT_VALUE: &str = "claude-cli/1.0.0";

// Anthropic's oauth/usage endpoint enforces a tight polling budget for
// third-party clients. This app has three independent callers that can all
// want fresh usage around the same moment (the main window's 60s poll, the
// tray's own 60s background poller, and the tray popup fetching on open) —
// without a shared cache each of those would issue its own HTTP request and
// blow through the budget in minutes. Keyed by account id, process-wide so
// every caller in this app instance shares one real fetch per TTL window.
const USAGE_CACHE_TTL: Duration = Duration::from_secs(4 * 60);
const USAGE_BACKOFF_DURATION: Duration = Duration::from_secs(30 * 60);

struct CachedUsage {
    usage: UsageInfo,
    fetched_at: Instant,
}

static USAGE_CACHE: LazyLock<Mutex<HashMap<String, CachedUsage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static USAGE_BACKOFF_UNTIL: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached_usage(account_id: &str) -> Option<UsageInfo> {
    USAGE_CACHE
        .lock()
        .ok()?
        .get(account_id)
        .map(|entry| entry.usage.clone())
}

fn cache_is_fresh(account_id: &str) -> bool {
    USAGE_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(account_id).map(|entry| entry.fetched_at.elapsed() < USAGE_CACHE_TTL))
        .unwrap_or(false)
}

fn is_backing_off(account_id: &str) -> bool {
    USAGE_BACKOFF_UNTIL
        .lock()
        .ok()
        .and_then(|map| map.get(account_id).copied())
        .is_some_and(|until| Instant::now() < until)
}

fn store_cache(account_id: &str, usage: UsageInfo) {
    if let Ok(mut cache) = USAGE_CACHE.lock() {
        cache.insert(
            account_id.to_string(),
            CachedUsage {
                usage,
                fetched_at: Instant::now(),
            },
        );
    }
    if let Ok(mut backoff) = USAGE_BACKOFF_UNTIL.lock() {
        backoff.remove(account_id);
    }
}

fn start_backoff(account_id: &str) {
    if let Ok(mut backoff) = USAGE_BACKOFF_UNTIL.lock() {
        backoff.insert(account_id.to_string(), Instant::now() + USAGE_BACKOFF_DURATION);
    }
}

/// Get usage information for a Claude account.
pub async fn get_claude_usage(account: &StoredAccount) -> Result<UsageInfo> {
    match &account.auth_data {
        AuthData::ClaudeKey { .. } => Ok(UsageInfo {
            account_id: account.id.clone(),
            plan_type: Some("api_key".to_string()),
            primary_used_percent: None,
            primary_window_minutes: None,
            primary_resets_at: None,
            secondary_used_percent: None,
            secondary_window_minutes: None,
            secondary_resets_at: None,
            has_credits: None,
            unlimited_credits: None,
            credits_balance: None,
            error: Some("Usage info not available for API key accounts".to_string()),
        }),
        AuthData::Claude { .. } => get_usage_with_claude_oauth_cached(account).await,
        _ => anyhow::bail!("Account is not a Claude account"),
    }
}

/// Cache/backoff wrapper around `get_usage_with_claude_oauth` shared by every
/// poller in this process (main window, tray background poller, tray popup).
async fn get_usage_with_claude_oauth_cached(account: &StoredAccount) -> Result<UsageInfo> {
    // Serve straight from cache when it's fresh, or when we're intentionally
    // backing off after a rate limit and have something to show instead of
    // flashing an "unavailable" error every poll.
    if cache_is_fresh(&account.id) || is_backing_off(&account.id) {
        if let Some(usage) = cached_usage(&account.id) {
            return Ok(usage);
        }
    }

    match get_usage_with_claude_oauth(account).await {
        Ok(usage) if usage.error.is_none() => {
            store_cache(&account.id, usage.clone());
            Ok(usage)
        }
        Ok(usage) => {
            // Rate limited or otherwise errored: back off for a while and
            // keep serving the last known-good reading if we have one.
            start_backoff(&account.id);
            Ok(cached_usage(&account.id).unwrap_or(usage))
        }
        Err(err) => {
            start_backoff(&account.id);
            match cached_usage(&account.id) {
                Some(usage) => Ok(usage),
                None => Err(err),
            }
        }
    }
}

async fn get_usage_with_claude_oauth(account: &StoredAccount) -> Result<UsageInfo> {
    let fresh_account = ensure_claude_tokens_fresh(account).await?;
    let access_token = extract_claude_access_token(&fresh_account)?;

    let response = send_claude_usage_request(access_token).await?;

    if response.status() == StatusCode::UNAUTHORIZED {
        let refreshed_account = refresh_claude_tokens(&fresh_account).await?;
        let retry_token = extract_claude_access_token(&refreshed_account)?;
        let retry_response = send_claude_usage_request(retry_token).await?;
        return parse_usage_response(&refreshed_account, retry_response).await;
    }

    parse_usage_response(&fresh_account, response).await
}

async fn parse_usage_response(
    account: &StoredAccount,
    response: reqwest::Response,
) -> Result<UsageInfo> {
    let status = response.status();

    if status == StatusCode::TOO_MANY_REQUESTS {
        return Ok(UsageInfo::error(
            account.id.clone(),
            "Rate limited while polling Claude usage; try again shortly.".to_string(),
        ));
    }

    if !status.is_success() {
        return Ok(UsageInfo::error(
            account.id.clone(),
            format!("API error: {status}"),
        ));
    }

    let payload: ClaudeUsagePayload = response
        .json()
        .await
        .context("Failed to parse Claude usage response")?;

    Ok(convert_payload_to_usage_info(account, payload))
}

fn convert_payload_to_usage_info(account: &StoredAccount, payload: ClaudeUsagePayload) -> UsageInfo {
    let plan_type = match &account.auth_data {
        AuthData::Claude {
            subscription_type, ..
        } => subscription_type.clone(),
        _ => None,
    };

    let extra_usage = payload.extra_usage;
    let has_credits = extra_usage.as_ref().map(|e| e.is_enabled);
    let unlimited_credits = extra_usage
        .as_ref()
        .map(|e| e.is_enabled && e.monthly_limit.is_none());
    let credits_balance = extra_usage.as_ref().and_then(|e| {
        let used = e.used_credits?;
        let currency = e.currency.as_deref().unwrap_or("$");
        Some(match e.monthly_limit {
            Some(limit) => format!("{currency}{used:.2} / {currency}{limit:.2}"),
            None => format!("{currency}{used:.2}"),
        })
    });

    UsageInfo {
        account_id: account.id.clone(),
        plan_type,
        primary_used_percent: payload.five_hour.as_ref().and_then(window_used_percent),
        primary_window_minutes: payload
            .five_hour
            .as_ref()
            .map(|_| SESSION_WINDOW_MINUTES),
        primary_resets_at: payload.five_hour.as_ref().and_then(window_resets_at),
        secondary_used_percent: payload.seven_day.as_ref().and_then(window_used_percent),
        secondary_window_minutes: payload
            .seven_day
            .as_ref()
            .map(|_| WEEKLY_WINDOW_MINUTES),
        secondary_resets_at: payload.seven_day.as_ref().and_then(window_resets_at),
        has_credits,
        unlimited_credits,
        credits_balance,
        error: None,
    }
}

fn window_used_percent(window: &crate::types::ClaudeUsageWindow) -> Option<f64> {
    // Anthropic's oauth/usage endpoint reports `utilization` as an
    // already-scaled 0-100 percentage (e.g. 9.0 == 9% used), not a 0-1 fraction.
    window.utilization
}

fn window_resets_at(window: &crate::types::ClaudeUsageWindow) -> Option<i64> {
    window
        .resets_at
        .as_ref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp())
}

#[derive(Debug, Clone)]
pub struct ClaudeAccountMetadata {
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

/// Derive a plan label from the profile response. Individual Pro/Max flags
/// take priority; otherwise fall back to the organization's plan type (e.g.
/// a Team seat has neither `has_claude_max` nor `has_claude_pro` set).
fn derive_claude_plan_type(payload: &ClaudeProfilePayload) -> Option<String> {
    if let Some(account) = &payload.account {
        if account.has_claude_max {
            return Some("max".to_string());
        }
        if account.has_claude_pro {
            return Some("pro".to_string());
        }
    }

    match payload
        .organization
        .as_ref()
        .and_then(|org| org.organization_type.as_deref())
    {
        Some("claude_team") => Some("team".to_string()),
        Some("claude_enterprise") => Some("enterprise".to_string()),
        Some(other) => Some(other.trim_start_matches("claude_").to_string()),
        None => None,
    }
}

pub async fn fetch_claude_account_metadata(account: &StoredAccount) -> Result<ClaudeAccountMetadata> {
    let access_token = extract_claude_access_token(account)?;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{ANTHROPIC_API}/api/oauth/profile"))
        .headers(build_claude_oauth_headers(access_token)?)
        .send()
        .await
        .context("Failed to send Claude profile request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Claude profile request failed: {status} - {body}");
    }

    let payload: ClaudeProfilePayload = response
        .json()
        .await
        .context("Failed to parse Claude profile response")?;

    let plan_type = derive_claude_plan_type(&payload);
    Ok(ClaudeAccountMetadata {
        email: payload.account.and_then(|a| a.email),
        plan_type,
    })
}

/// Send a minimal authenticated request to warm up account traffic paths.
pub async fn warmup_claude_account(account: &StoredAccount) -> Result<()> {
    match &account.auth_data {
        AuthData::ClaudeKey { key } => warmup_with_claude_api_key(key).await,
        AuthData::Claude { .. } => warmup_with_claude_oauth(account).await,
        _ => anyhow::bail!("Account is not a Claude account"),
    }
}

async fn warmup_with_claude_oauth(account: &StoredAccount) -> Result<()> {
    let fresh_account = ensure_claude_tokens_fresh(account).await?;
    let access_token = extract_claude_access_token(&fresh_account)?;

    let mut response = send_claude_warmup_request(access_token, true).await?;

    if response.status() == StatusCode::UNAUTHORIZED {
        let refreshed_account = refresh_claude_tokens(&fresh_account).await?;
        let retry_token = extract_claude_access_token(&refreshed_account)?;
        response = send_claude_warmup_request(retry_token, true).await?;
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        println!("[Warmup] Claude warm-up error response: {body}");
        anyhow::bail!("Claude warm-up failed with status {status}");
    }

    Ok(())
}

async fn warmup_with_claude_api_key(api_key: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let payload = build_warmup_payload(false);
    let response = client
        .post(format!("{ANTHROPIC_API}/v1/messages"))
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&payload)
        .send()
        .await
        .context("Failed to send Claude API key warm-up request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        println!("[Warmup] Claude API key warm-up error response: {body}");
        anyhow::bail!("Claude API key warm-up failed with status {status}");
    }

    Ok(())
}

fn build_warmup_payload(include_system_prompt: bool) -> serde_json::Value {
    let mut payload = json!({
        "model": WARMUP_MODEL,
        "max_tokens": 1,
        "messages": [
            { "role": "user", "content": "Hi" }
        ]
    });

    if include_system_prompt {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("system".to_string(), json!(CLAUDE_CODE_SYSTEM_PROMPT));
        }
    }

    payload
}

fn build_claude_oauth_headers(access_token: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}")).context("Invalid access token")?,
    );
    headers.insert(
        HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static(OAUTH_BETA_HEADER_VALUE),
    );
    Ok(headers)
}

fn extract_claude_access_token(account: &StoredAccount) -> Result<&str> {
    match &account.auth_data {
        AuthData::Claude { access_token, .. } => Ok(access_token.as_str()),
        _ => anyhow::bail!("Account is not using Claude OAuth"),
    }
}

async fn send_claude_usage_request(access_token: &str) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let headers = build_claude_oauth_headers(access_token)?;

    client
        .get(format!("{ANTHROPIC_API}/api/oauth/usage"))
        .headers(headers)
        .send()
        .await
        .context("Failed to send Claude usage request")
}

async fn send_claude_warmup_request(access_token: &str, include_system_prompt: bool) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let mut headers = build_claude_oauth_headers(access_token)?;
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    let payload = build_warmup_payload(include_system_prompt);

    client
        .post(format!("{ANTHROPIC_API}/v1/messages"))
        .headers(headers)
        .json(&payload)
        .send()
        .await
        .context("Failed to send Claude warm-up request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ClaudeUsageWindow;

    fn unique_test_id(label: &str) -> String {
        format!(
            "{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    fn sample_usage(account_id: &str) -> UsageInfo {
        UsageInfo {
            account_id: account_id.to_string(),
            plan_type: Some("max".to_string()),
            primary_used_percent: Some(9.0),
            primary_window_minutes: Some(SESSION_WINDOW_MINUTES),
            primary_resets_at: None,
            secondary_used_percent: Some(22.0),
            secondary_window_minutes: Some(WEEKLY_WINDOW_MINUTES),
            secondary_resets_at: None,
            has_credits: None,
            unlimited_credits: None,
            credits_balance: None,
            error: None,
        }
    }

    #[test]
    fn cache_is_empty_and_not_fresh_for_an_unknown_account() {
        let id = unique_test_id("unknown");
        assert!(!cache_is_fresh(&id));
        assert!(cached_usage(&id).is_none());
        assert!(!is_backing_off(&id));
    }

    #[test]
    fn storing_usage_makes_the_cache_fresh_and_clears_any_backoff() {
        let id = unique_test_id("store");
        start_backoff(&id);
        assert!(is_backing_off(&id));

        store_cache(&id, sample_usage(&id));

        assert!(cache_is_fresh(&id));
        assert!(!is_backing_off(&id));
        assert_eq!(
            cached_usage(&id).and_then(|u| u.primary_used_percent),
            Some(9.0)
        );
    }

    #[test]
    fn backing_off_serves_the_last_known_good_reading() {
        let id = unique_test_id("backoff");
        store_cache(&id, sample_usage(&id));
        start_backoff(&id);

        // Still backing off, and the earlier good reading is still there for
        // callers to fall back on instead of showing "unavailable".
        assert!(is_backing_off(&id));
        assert_eq!(
            cached_usage(&id).and_then(|u| u.secondary_used_percent),
            Some(22.0)
        );
    }

    fn account_with_plan(plan: Option<&str>) -> StoredAccount {
        StoredAccount::new_claude(
            "Test".into(),
            None,
            plan.map(str::to_string),
            "access".into(),
            "refresh".into(),
            0,
            Vec::new(),
        )
    }

    #[test]
    fn maps_five_hour_and_seven_day_windows_to_primary_and_secondary() {
        let payload = ClaudeUsagePayload {
            five_hour: Some(ClaudeUsageWindow {
                utilization: Some(42.0),
                resets_at: Some("2026-01-01T00:00:00Z".to_string()),
            }),
            seven_day: Some(ClaudeUsageWindow {
                utilization: Some(81.0),
                resets_at: Some("2026-01-05T00:00:00Z".to_string()),
            }),
            extra_usage: None,
        };

        let usage = convert_payload_to_usage_info(&account_with_plan(Some("max")), payload);

        assert_eq!(usage.plan_type.as_deref(), Some("max"));
        assert_eq!(usage.primary_used_percent, Some(42.0));
        assert_eq!(usage.primary_window_minutes, Some(SESSION_WINDOW_MINUTES));
        assert_eq!(usage.secondary_used_percent, Some(81.0));
        assert_eq!(usage.secondary_window_minutes, Some(WEEKLY_WINDOW_MINUTES));
        assert!(usage.primary_resets_at.is_some());
        assert!(usage.secondary_resets_at.is_some());
    }

    #[test]
    fn missing_windows_map_to_none() {
        let payload = ClaudeUsagePayload {
            five_hour: None,
            seven_day: None,
            extra_usage: None,
        };

        let usage = convert_payload_to_usage_info(&account_with_plan(None), payload);

        assert!(usage.primary_used_percent.is_none());
        assert!(usage.secondary_used_percent.is_none());
        assert!(usage.error.is_none());
    }
}
