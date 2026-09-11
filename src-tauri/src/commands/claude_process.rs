//! Process detection for the Claude Code CLI (`claude`).
//!
//! Claude Code has no separate desktop shell to detect/relaunch the way Codex
//! does, so this is a plain "is the CLI running" check used to avoid
//! overwriting credentials out from under a live session.

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const CLAUDE_RUNNING_SWITCH_BLOCKED_PREFIX: &str = "Cannot switch accounts while ";

/// Information about running Claude Code CLI processes
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaudeProcessInfo {
    pub count: usize,
    pub can_switch: bool,
    pub pids: Vec<u32>,
}

/// Summary of a force-close operation for active Claude Code CLI processes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KillClaudeProcessesResult {
    pub targeted_count: usize,
    pub killed_pids: Vec<u32>,
    pub failed_pids: Vec<u32>,
}

#[tauri::command]
pub async fn check_claude_processes() -> Result<ClaudeProcessInfo, String> {
    let pids = find_claude_processes().map_err(|e| e.to_string())?;
    let count = pids.len();

    Ok(ClaudeProcessInfo {
        count,
        can_switch: count == 0,
        pids,
    })
}

pub(crate) fn ensure_claude_not_running() -> Result<(), String> {
    let pids = find_claude_processes().map_err(|e| e.to_string())?;

    if pids.is_empty() {
        return Ok(());
    }

    Err(format!(
        "{CLAUDE_RUNNING_SWITCH_BLOCKED_PREFIX}{} Claude Code process{} running",
        pids.len(),
        if pids.len() == 1 { " is" } else { "es are" }
    ))
}

pub(crate) fn is_claude_running_switch_block(error: &str) -> bool {
    error.starts_with(CLAUDE_RUNNING_SWITCH_BLOCKED_PREFIX)
}

#[tauri::command]
pub async fn kill_claude_processes() -> Result<KillClaudeProcessesResult, String> {
    tokio::task::spawn_blocking(kill_claude_processes_blocking)
        .await
        .map_err(|e| e.to_string())?
}

fn kill_claude_processes_blocking() -> Result<KillClaudeProcessesResult, String> {
    let pids = find_claude_processes().map_err(|e| e.to_string())?;
    let targeted_count = pids.len();
    let mut killed_pids = Vec::new();
    let mut failed_pids = Vec::new();

    for pid in pids {
        if force_kill_process(pid) {
            killed_pids.push(pid);
        } else {
            failed_pids.push(pid);
        }
    }

    Ok(KillClaudeProcessesResult {
        targeted_count,
        killed_pids,
        failed_pids,
    })
}

fn force_kill_process(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let killed = Command::new("/bin/kill")
            .arg("-9")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        return killed || !process_exists(pid);
    }

    #[cfg(windows)]
    {
        let killed = Command::new("taskkill")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        return killed || !process_exists(pid);
    }

    #[allow(unreachable_code)]
    false
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        return Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .args(["-o", "pid="])
            .output()
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .split_whitespace()
                        .any(|value| value == pid.to_string())
            })
            .unwrap_or(false);
    }

    #[cfg(windows)]
    {
        return Command::new("tasklist")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(false);
    }

    #[allow(unreachable_code)]
    false
}

/// Find running `claude` CLI processes (excludes this app and app-server/IDE helpers).
fn find_claude_processes() -> anyhow::Result<Vec<u32>> {
    #[cfg(unix)]
    {
        let mut pids = Vec::new();
        let output = Command::new("ps").args(["-axo", "pid=,command="]).output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let mut parts = line.split_whitespace();
                let Some(pid_str) = parts.next() else {
                    continue;
                };
                let command = parts.collect::<Vec<_>>().join(" ");
                if command.is_empty() {
                    continue;
                }

                let Ok(pid) = pid_str.parse::<u32>() else {
                    continue;
                };

                let lowercase_command = command.to_ascii_lowercase();
                if lowercase_command.contains("codex-switcher") {
                    continue;
                }

                let first_token = command.split_whitespace().next().unwrap_or("");
                let is_claude_cli = first_token == "claude" || first_token.ends_with("/claude");
                if !is_claude_cli {
                    continue;
                }

                if pid == std::process::id() || pids.contains(&pid) {
                    continue;
                }

                pids.push(pid);
            }
        }

        pids.sort_unstable();
        pids.dedup();
        return Ok(pids);
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/FI", "IMAGENAME eq claude.exe", "/FO", "CSV", "/NH"])
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut pids = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() > 1 {
                if let Ok(pid) = parts[1].trim_matches('"').parse::<u32>() {
                    pids.push(pid);
                }
            }
        }
        return Ok(pids);
    }

    #[allow(unreachable_code)]
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::find_claude_processes;

    #[test]
    #[ignore = "Read-only inspection of live claude processes; run explicitly"]
    fn inspect_live_claude_targets() {
        let pids = find_claude_processes().unwrap();
        println!("detected claude pids: {pids:?}");
        assert!(!pids.is_empty(), "expected to find a running claude process");
    }
}
