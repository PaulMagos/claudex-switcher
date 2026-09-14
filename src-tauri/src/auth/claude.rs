//! Claude Code credential storage - reads/writes the credentials Claude Code
//! itself uses: `~/.claude/.credentials.json` on Linux/Windows, or the
//! macOS Keychain item `"Claude Code-credentials"` on macOS.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::types::{AuthData, ClaudeCredentialsFile, ClaudeOAuthTokens, ClaudeProfilePayload, StoredAccount};

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Get the Claude Code config directory (`$CLAUDE_CONFIG_DIR` or `~/.claude`)
pub fn get_claude_config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }

    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(".claude"))
}

/// Get the path to the official credentials file (used on non-macOS platforms)
pub fn get_claude_credentials_file() -> Result<PathBuf> {
    Ok(get_claude_config_dir()?.join(".credentials.json"))
}

fn current_username() -> Result<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .context("Could not determine current username for Keychain access")
}

fn create_auth_json(account: &StoredAccount) -> Result<ClaudeCredentialsFile> {
    match &account.auth_data {
        AuthData::Claude {
            access_token,
            refresh_token,
            expires_at,
            scopes,
            subscription_type,
        } => Ok(ClaudeCredentialsFile {
            claude_ai_oauth: ClaudeOAuthTokens {
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                expires_at: *expires_at,
                scopes: scopes.clone(),
                subscription_type: subscription_type.clone(),
            },
        }),
        _ => anyhow::bail!("Account is not a Claude OAuth account"),
    }
}

/// Write the account's credentials to wherever Claude Code reads them from.
pub fn switch_to_claude_account(account: &StoredAccount) -> Result<()> {
    if matches!(account.auth_data, AuthData::ClaudeKey { .. }) {
        // API-key accounts don't have a credentials file; Claude Code picks up
        // ANTHROPIC_API_KEY from the environment instead, which this app
        // cannot set for the user's shell. Nothing to write here.
        return Ok(());
    }

    let auth_json = create_auth_json(account)?;
    let content = serde_json::to_string_pretty(&auth_json)
        .context("Failed to serialize Claude credentials")?;

    #[cfg(target_os = "macos")]
    {
        write_macos_keychain(&content)?;
        // The Keychain item alone is not reliably picked up by the real
        // `claude` binary (its internal recognition of an externally
        // written Keychain item is unexplained after extensive testing).
        // CLAUDE_CODE_OAUTH_TOKEN is a first-class, documented env-var auth
        // source claude checks before ever touching the Keychain, so mirror
        // the access token there via the per-user launchd environment -
        // never written to a file, only held in memory for this login
        // session, and picked up by any `claude` process started after
        // this call.
        if let AuthData::Claude {
            access_token,
            subscription_type,
            ..
        } = &account.auth_data
        {
            set_claude_oauth_env(Some(access_token), subscription_type.as_deref());
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        write_credentials_file(&content)
    }
}

/// Mirror (or clear) the active Claude account's access token into the
/// per-user launchd environment as `CLAUDE_CODE_OAUTH_TOKEN`, so new
/// `claude` processes pick it up without any file ever holding the raw
/// token in plaintext. Best-effort: failures are logged, never fatal.
#[cfg(target_os = "macos")]
pub fn set_claude_oauth_env(access_token: Option<&str>, subscription_type: Option<&str>) {
    match access_token {
        Some(token) => {
            if let Err(err) = Command::new("launchctl")
                .args(["setenv", "CLAUDE_CODE_OAUTH_TOKEN", token])
                .status()
            {
                println!("[Auth] Failed to set CLAUDE_CODE_OAUTH_TOKEN: {err}");
            }
            match subscription_type {
                Some(sub) => {
                    if let Err(err) = Command::new("launchctl")
                        .args(["setenv", "CLAUDE_CODE_SUBSCRIPTION_TYPE", sub])
                        .status()
                    {
                        println!("[Auth] Failed to set CLAUDE_CODE_SUBSCRIPTION_TYPE: {err}");
                    }
                }
                None => {
                    let _ = Command::new("launchctl")
                        .args(["unsetenv", "CLAUDE_CODE_SUBSCRIPTION_TYPE"])
                        .status();
                }
            }
        }
        None => {
            let _ = Command::new("launchctl")
                .args(["unsetenv", "CLAUDE_CODE_OAUTH_TOKEN"])
                .status();
            let _ = Command::new("launchctl")
                .args(["unsetenv", "CLAUDE_CODE_SUBSCRIPTION_TYPE"])
                .status();
        }
    }
}

#[cfg(target_os = "macos")]
fn write_macos_keychain(content: &str) -> Result<()> {
    let username = current_username()?;

    // Delete any existing item first; `-U` alone can fail to update an item
    // created by a different process, so make this idempotent.
    let _ = Command::new("security")
        .args(["delete-generic-password", "-a", &username, "-s", KEYCHAIN_SERVICE])
        .output();

    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            &username,
            "-s",
            KEYCHAIN_SERVICE,
            "-w",
            content,
            // Without an ACL, `security` defaults to trusting only itself
            // (/usr/bin/security). The real Claude Code binary is a
            // different process and its native Keychain read would be
            // silently denied, making it report "not logged in" even
            // though the item exists and parses fine.
            "-A",
        ])
        .status()
        .context("Failed to run security(1) to write Claude Code Keychain item")?;

    if !status.success() {
        anyhow::bail!("security add-generic-password failed for Claude Code credentials");
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn write_credentials_file(content: &str) -> Result<()> {
    let path = get_claude_credentials_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create Claude config dir: {}", parent.display()))?;
    }

    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }

    Ok(())
}

/// Read the credentials Claude Code currently has active, if any.
pub fn read_current_claude_auth() -> Result<Option<ClaudeCredentialsFile>> {
    #[cfg(target_os = "macos")]
    {
        read_macos_keychain()
    }

    #[cfg(not(target_os = "macos"))]
    {
        read_credentials_file()
    }
}

#[cfg(target_os = "macos")]
fn read_macos_keychain() -> Result<Option<ClaudeCredentialsFile>> {
    let username = current_username()?;
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            &username,
            "-s",
            KEYCHAIN_SERVICE,
            "-w",
        ])
        .output()
        .context("Failed to run security(1) to read Claude Code Keychain item")?;

    // security(1) exits 44 when the item does not exist.
    if !output.status.success() {
        return Ok(None);
    }

    let content = String::from_utf8_lossy(&output.stdout);
    let content = content.trim();
    if content.is_empty() {
        return Ok(None);
    }

    let parsed: ClaudeCredentialsFile =
        serde_json::from_str(content).context("Failed to parse Claude Code Keychain item")?;
    Ok(Some(parsed))
}

#[cfg(not(target_os = "macos"))]
fn read_credentials_file() -> Result<Option<ClaudeCredentialsFile>> {
    let path = get_claude_credentials_file()?;
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let parsed: ClaudeCredentialsFile = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(parsed))
}

/// Check whether Claude Code currently has any active login.
pub fn has_active_claude_login() -> Result<bool> {
    Ok(read_current_claude_auth()?.is_some())
}

fn build_account_from_credentials(
    credentials: ClaudeCredentialsFile,
    name: String,
) -> StoredAccount {
    let tokens = credentials.claude_ai_oauth;
    StoredAccount::new_claude(
        name,
        None,
        tokens.subscription_type,
        tokens.access_token,
        tokens.refresh_token,
        tokens.expires_at,
        tokens.scopes,
    )
}

/// Import a Claude account from `.credentials.json` file contents.
pub fn import_from_claude_credentials_json(
    content: &str,
    account_name: String,
) -> Result<StoredAccount> {
    let credentials: ClaudeCredentialsFile =
        serde_json::from_str(content).context("Failed to parse Claude credentials contents")?;
    Ok(build_account_from_credentials(
        credentials,
        account_name.trim().to_string(),
    ))
}

/// Path to the account-identity cache Claude Code keeps at `~/.claude.json`
/// (or `$CLAUDE_CONFIG_DIR/.claude.json` when that env var is set) - distinct
/// from the config directory used for `.credentials.json`.
pub fn get_claude_account_json_path() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir).join(".claude.json"));
        }
    }

    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(".claude.json"))
}

/// Mirror the account-identity cache Claude Code itself keeps in
/// `~/.claude.json`'s `oauthAccount` key. The raw OAuth token in the
/// Keychain/credentials file is not the whole picture: Claude Code also
/// reads this cache for the active account's email/org/plan, and it is
/// normally only populated by a real `claude login`. Left untouched, it
/// keeps pointing at whichever account last did a real login, out of sync
/// with whatever account we just switched the Keychain token to.
pub fn sync_oauth_account_cache(profile: &ClaudeProfilePayload) -> Result<()> {
    let path = get_claude_account_json_path()?;

    let mut root: serde_json::Value = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let root_obj = root
        .as_object_mut()
        .context("~/.claude.json is not a JSON object")?;

    let mut oauth_account = root_obj
        .get("oauthAccount")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut set = |key: &str, value: serde_json::Value| {
        if !value.is_null() {
            oauth_account.insert(key.to_string(), value);
        }
    };

    if let Some(account) = &profile.account {
        set("accountUuid", account.uuid.clone().into());
        set("emailAddress", account.email.clone().into());
        set("displayName", account.display_name.clone().into());
        set("fullName", account.full_name.clone().into());
        set("accountCreatedAt", account.created_at.clone().into());
    }
    if let Some(org) = &profile.organization {
        set("organizationUuid", org.uuid.clone().into());
        set("organizationName", org.name.clone().into());
        set("organizationType", org.organization_type.clone().into());
        set("billingType", org.billing_type.clone().into());
        set("seatTier", org.seat_tier.clone().into());
        set("organizationRateLimitTier", org.rate_limit_tier.clone().into());
        set("userRateLimitTier", org.rate_limit_tier.clone().into());
        set(
            "subscriptionCreatedAt",
            org.subscription_created_at.clone().into(),
        );
        if let Some(has_extra) = org.has_extra_usage_enabled {
            oauth_account.insert("hasExtraUsageEnabled".to_string(), has_extra.into());
        }
    }

    oauth_account.insert(
        "profileFetchedAt".to_string(),
        chrono::Utc::now().timestamp_millis().into(),
    );
    oauth_account
        .entry("ccOnboardingFlags")
        .or_insert_with(|| serde_json::json!({}));

    root_obj.insert(
        "oauthAccount".to_string(),
        serde_json::Value::Object(oauth_account),
    );

    let content =
        serde_json::to_string_pretty(&root).context("Failed to serialize ~/.claude.json")?;
    std::fs::write(&path, content).with_context(|| format!("Failed to write {}", path.display()))
}

/// Import a Claude account from a `.credentials.json` file path.
pub fn import_from_claude_credentials_file(
    path: &str,
    account_name: String,
) -> Result<StoredAccount> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read Claude credentials file: {path}"))?;
    import_from_claude_credentials_json(&content, account_name)
        .with_context(|| format!("Failed to parse Claude credentials file: {path}"))
}
