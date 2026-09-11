//! Claude Code OAuth login flow.
//!
//! Unlike Codex, Claude Code's OAuth flow has no localhost redirect capture:
//! `claude.ai/oauth/authorize` sends the browser to a page that displays an
//! authorization code (`CODE#STATE`) for the user to copy and paste back into
//! the CLI (or, here, this app).

use anyhow::{Context, Result};
use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::types::OAuthLoginInfo;

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub(crate) const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub(crate) const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

#[derive(Debug, Clone)]
pub struct ClaudePkceCodes {
    pub code_verifier: String,
    pub code_challenge: String,
}

pub fn generate_claude_pkce() -> ClaudePkceCodes {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);

    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);

    ClaudePkceCodes {
        code_verifier,
        code_challenge,
    }
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A login flow waiting for the user to paste back an authorization code.
#[derive(Debug, Clone)]
pub struct ClaudePendingLogin {
    pub pkce: ClaudePkceCodes,
    pub state: String,
    pub account_name: String,
}

/// Start a Claude Code OAuth login: build the authorize URL and PKCE/state to
/// verify the pasted-back code against.
pub fn start_claude_oauth_login(account_name: String) -> (OAuthLoginInfo, ClaudePendingLogin) {
    let pkce = generate_claude_pkce();
    let state = generate_state();

    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
    ];
    let query_string = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let auth_url = format!("{AUTHORIZE_URL}?{query_string}");

    let login_info = OAuthLoginInfo {
        auth_url,
        callback_port: 0,
        manual_code: true,
    };

    let pending = ClaudePendingLogin {
        pkce,
        state,
        account_name,
    };

    (login_info, pending)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds until expiry
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

fn expires_at_from_now(expires_in: Option<i64>) -> i64 {
    let ttl_seconds = expires_in.unwrap_or(8 * 60 * 60);
    chrono::Utc::now().timestamp_millis() + ttl_seconds.max(0) * 1000
}

/// Parse a pasted `CODE#STATE` value and verify it matches the pending
/// login's state. A bare code with no `#state` suffix is accepted as-is.
fn parse_pasted_code<'a>(pasted: &'a str, expected_state: &str) -> Result<&'a str> {
    let pasted = pasted.trim();
    let (code, state) = pasted
        .split_once('#')
        .unwrap_or((pasted, expected_state));

    if state != expected_state {
        anyhow::bail!("Authorization code state does not match the pending login");
    }

    Ok(code)
}

/// Exchange a pasted `CODE#STATE` value for OAuth tokens.
pub async fn exchange_claude_code_for_tokens(
    pending: &ClaudePendingLogin,
    pasted: &str,
) -> Result<(ClaudeTokenResponse, i64)> {
    let code = parse_pasted_code(pasted, &pending.state)?;
    let state = &pending.state;

    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": state,
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": pending.pkce.code_verifier,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .json(&body)
        .send()
        .await
        .context("Failed to send Claude token exchange request")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("Claude token exchange failed: {status} - {text}");
    }

    let tokens: ClaudeTokenResponse = response
        .json()
        .await
        .context("Failed to parse Claude token exchange response")?;
    let expires_at = expires_at_from_now(tokens.expires_in);

    Ok((tokens, expires_at))
}

/// Refresh Claude Code OAuth tokens using a refresh token.
pub async fn refresh_claude_tokens_via_api(refresh_token: &str) -> Result<(ClaudeTokenResponse, i64)> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .json(&body)
        .send()
        .await
        .context("Failed to send Claude token refresh request")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("Claude token refresh failed: {status} - {text}");
    }

    let tokens: ClaudeTokenResponse = response
        .json()
        .await
        .context("Failed to parse Claude token refresh response")?;
    let expires_at = expires_at_from_now(tokens.expires_in);

    Ok((tokens, expires_at))
}

#[cfg(test)]
mod tests {
    use super::parse_pasted_code;

    #[test]
    fn splits_code_and_state_on_hash() {
        let code = parse_pasted_code("abc123#the-state", "the-state").unwrap();
        assert_eq!(code, "abc123");
    }

    #[test]
    fn rejects_mismatched_state() {
        let error = parse_pasted_code("abc123#wrong-state", "the-state").unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn accepts_bare_code_without_state_suffix() {
        let code = parse_pasted_code("  abc123  ", "the-state").unwrap();
        assert_eq!(code, "abc123");
    }
}
