//! Claude Code credential storage - reads/writes the credentials Claude Code
//! itself uses: `~/.claude/.credentials.json` on Linux/Windows, or the
//! macOS Keychain item `"Claude Code-credentials"` on macOS.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::types::{AuthData, ClaudeCredentialsFile, ClaudeOAuthTokens, StoredAccount};

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
        write_macos_keychain(&content)
    }

    #[cfg(not(target_os = "macos"))]
    {
        write_credentials_file(&content)
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
