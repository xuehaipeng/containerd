use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    name = "session-restore",
    about = "Containerd session restoration tool for shared storage"
)]
struct Args {
    #[arg(
        long,
        default_value = "/etc/path-mappings.json",
        help = "Path to the path mappings JSON file"
    )]
    mappings_file: PathBuf,

    #[arg(
        long,
        default_value = "/sessions",
        help = "Base path for session directories inside container"
    )]
    sessions_path: PathBuf,

    #[arg(long, help = "Current namespace")]
    namespace: Option<String>,

    #[arg(long, help = "Current pod name")]
    pod_name: Option<String>,

    #[arg(long, help = "Current container name")]
    container_name: Option<String>,

    #[arg(long, default_value = "300", help = "Operation timeout in seconds")]
    timeout: u64,

    #[arg(long, help = "Dry run mode - don't actually copy files")]
    dry_run: bool,

    #[arg(long, help = "Skip cleanup of old sessions")]
    skip_cleanup: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct PathMappings {
    mappings: HashMap<String, PathMapping>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PathMapping {
    #[serde(default = "default_namespace")]
    namespace: String,
    pod_name: String,
    container_name: String,
    created_at: String,
    pod_hash: String,
    snapshot_hash: String,
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(default)]
    last_accessed: Option<String>,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug)]
struct SessionInfo {
    pod_hash: String,
    snapshot_hash: String,
    path: PathBuf,
    created_at: DateTime<Utc>,
    mod_time: SystemTime,
}

#[derive(Debug)]
struct RestoreResult {
    success_count: usize,
    fail_count: usize,
    skip_count: usize,
    errors: Vec<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    info!("=== Session Restore Tool Started ===");
    info!("Args: {:?}", args);

    // Get current pod information
    let namespace = args
        .namespace
        .or_else(|| std::env::var("CURRENT_NAMESPACE").ok())
        .unwrap_or_else(|| "default".to_string());
    
    let pod_name = args
        .pod_name
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "nb-test-0".to_string());
    
    let container_name = args
        .container_name
        .or_else(|| std::env::var("CURRENT_CONTAINER_NAME").ok())
        .unwrap_or_else(|| "inference".to_string());

    info!(
        "Pod info: namespace={}, pod={}, container={}",
        namespace, pod_name, container_name
    );

    // Parse path mappings to find current session
    let current_session = match find_current_session(&args.mappings_file, &namespace, &pod_name, &container_name)? {
        Some(session) => session,
        None => {
            info!("No current session found in path mappings. Starting fresh.");
            return Ok(());
        }
    };

    info!(
        "Current session: pod_hash={}, snapshot_hash={}, created_at={}",
        current_session.pod_hash, current_session.snapshot_hash, current_session.created_at
    );

    // Find all available sessions for this pod
    let available_sessions = find_available_sessions(&args.sessions_path, &current_session.pod_hash)?;
    info!("Found {} available sessions", available_sessions.len());
    
    for session in &available_sessions {
        info!(
            "  Session: {} (mod_time: {:?})",
            session.snapshot_hash,
            session.mod_time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
        );
    }

    // Find the most recent previous session (not the current one)
    let previous_session = find_previous_session(&available_sessions, &current_session.snapshot_hash)?;

    match previous_session {
        Some(prev) => {
            info!(
                "Previous session identified: {} at {:?}",
                prev.snapshot_hash, prev.path
            );

            if !args.dry_run {
                // Perform restoration
                let result = restore_from_session(&prev.path, args.timeout)?;
                info!(
                    "Restoration complete: {} success, {} failed, {} skipped",
                    result.success_count, result.fail_count, result.skip_count
                );

                if !result.errors.is_empty() {
                    warn!("Restoration errors:");
                    for error in &result.errors {
                        warn!("  {}", error);
                    }
                }

                // Cleanup old sessions
                if !args.skip_cleanup {
                    cleanup_old_sessions(
                        &args.sessions_path,
                        &current_session.pod_hash,
                        &current_session.snapshot_hash,
                        &prev.snapshot_hash,
                        args.timeout,
                    )?;
                }
            } else {
                info!("Dry run mode: would restore from {}", prev.path.display());
            }
        }
        None => {
            info!("No previous session found. Starting with fresh session.");
        }
    }

    info!("=== Session Restore Tool Completed ===");
    Ok(())
}

fn find_current_session(
    mappings_file: &Path,
    namespace: &str,
    pod_name: &str,
    container_name: &str,
) -> Result<Option<SessionInfo>> {
    if !mappings_file.exists() {
        warn!("Path mappings file not found: {}", mappings_file.display());
        return Ok(None);
    }

    let content = fs::read_to_string(mappings_file)
        .with_context(|| format!("Failed to read mappings file: {}", mappings_file.display()))?;

    let path_mappings: PathMappings = serde_json::from_str(&content)
        .with_context(|| "Failed to parse path mappings JSON")?;

    info!("Loaded {} path mappings", path_mappings.mappings.len());

    // Find the most recent matching entry
    let mut best_match: Option<(String, PathMapping)> = None;
    let mut latest_time: Option<DateTime<Utc>> = None;

    for (path_key, mapping) in path_mappings.mappings {
        if mapping.namespace == namespace
            && mapping.pod_name == pod_name
            && mapping.container_name == container_name
        {
            let created_at = DateTime::parse_from_rfc3339(&mapping.created_at)
                .with_context(|| format!("Invalid created_at timestamp: {}", mapping.created_at))?
                .with_timezone(&Utc);

            if latest_time.map_or(true, |t| created_at > t) {
                latest_time = Some(created_at);
                best_match = Some((path_key, mapping));
            }
        }
    }

    match best_match {
        Some((_, mapping)) => {
            let created_at = DateTime::parse_from_rfc3339(&mapping.created_at)?
                .with_timezone(&Utc);
            
            Ok(Some(SessionInfo {
                pod_hash: mapping.pod_hash,
                snapshot_hash: mapping.snapshot_hash,
                path: PathBuf::from("/"), // This is the current session, we don't need the path
                created_at,
                mod_time: SystemTime::now(), // Not relevant for current session
            }))
        }
        None => Ok(None),
    }
}

fn find_available_sessions(
    sessions_path: &Path,
    pod_hash: &str,
) -> Result<Vec<SessionInfo>> {
    let pod_sessions_path = sessions_path.join(pod_hash);
    
    if !pod_sessions_path.exists() {
        info!("Pod sessions directory not found: {}", pod_sessions_path.display());
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();

    for entry in fs::read_dir(&pod_sessions_path)
        .with_context(|| format!("Failed to read directory: {}", pod_sessions_path.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_dir() {
            let session_hash = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            let fs_path = path.join("fs");
            if fs_path.exists() {
                let metadata = fs::metadata(&path)?;
                let mod_time = metadata.modified()?;

                sessions.push(SessionInfo {
                    pod_hash: pod_hash.to_string(),
                    snapshot_hash: session_hash,
                    path: fs_path,
                    created_at: Utc::now(), // We don't have the exact creation time from filesystem
                    mod_time,
                });
            }
        }
    }

    // Sort by modification time (newest first)
    sessions.sort_by(|a, b| b.mod_time.cmp(&a.mod_time));

    Ok(sessions)
}

fn find_previous_session(
    available_sessions: &[SessionInfo],
    current_snapshot_hash: &str,
) -> Result<Option<SessionInfo>> {
    // Find the most recent session that is not the current one and has content
    for session in available_sessions {
        if session.snapshot_hash != current_snapshot_hash {
            // Check if the session has content
            if has_meaningful_content(&session.path)? {
                info!(
                    "Selected previous session: {} with content at {}",
                    session.snapshot_hash,
                    session.path.display()
                );
                return Ok(Some(session.clone()));
            } else {
                info!(
                    "Skipping empty session: {} at {}",
                    session.snapshot_hash,
                    session.path.display()
                );
            }
        }
    }

    Ok(None)
}

fn has_meaningful_content(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    // Check if there are any files in the directory
    for entry in WalkDir::new(path).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() {
            // Found at least one file
            return Ok(true);
        }
    }

    Ok(false)
}

fn restore_from_session(source_path: &Path, timeout: u64) -> Result<RestoreResult> {
    info!("Starting restoration from: {}", source_path.display());

    let mut result = RestoreResult {
        success_count: 0,
        fail_count: 0,
        skip_count: 0,
        errors: Vec::new(),
    };

    // Try rsync first if available
    if which::which("rsync").is_ok() {
        info!("Using rsync for restoration");
        let output = Command::new("timeout")
            .arg(timeout.to_string())
            .arg("rsync")
            .arg("-av")
            .arg("--delete")
            .arg("--ignore-errors")
            .arg("--partial")
            .arg("--no-times")
            .arg("--no-perms")
            .arg(format!("{}/", source_path.display()))
            .arg("/")
            .output()
            .with_context(|| "Failed to execute rsync")?;

        if output.status.success() {
            info!("Rsync completed successfully");
            result.success_count += 1;
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Rsync completed with warnings: {}", stderr);
            result.errors.push(format!("Rsync warnings: {}", stderr));
        }
    } else {
        // Fallback to manual copy
        info!("Rsync not available, using manual copy");
        result = manual_copy(source_path, Path::new("/"))?;
    }

    Ok(result)
}

fn manual_copy(source: &Path, target: &Path) -> Result<RestoreResult> {
    let mut result = RestoreResult {
        success_count: 0,
        fail_count: 0,
        skip_count: 0,
        errors: Vec::new(),
    };

    for entry in WalkDir::new(source) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                result.errors.push(format!("Walk error: {}", e));
                result.fail_count += 1;
                continue;
            }
        };

        let source_path = entry.path();
        let relative_path = source_path.strip_prefix(source)?;
        
        // Skip the root directory itself
        if relative_path.as_os_str().is_empty() {
            continue;
        }
        
        let target_path = target.join(relative_path);

        if entry.file_type().is_dir() {
            match fs::create_dir_all(&target_path) {
                Ok(_) => {
                    result.success_count += 1;
                }
                Err(e) => {
                    result.errors.push(format!("Failed to create directory {}: {}", target_path.display(), e));
                    result.fail_count += 1;
                }
            }
        } else if entry.file_type().is_file() {
            match fs::copy(source_path, &target_path) {
                Ok(_) => {
                    result.success_count += 1;
                }
                Err(e) => {
                    result.errors.push(format!("Failed to copy file {} to {}: {}", source_path.display(), target_path.display(), e));
                    result.skip_count += 1;
                }
            }
        }
    }

    Ok(result)
}

fn cleanup_old_sessions(
    sessions_path: &Path,
    pod_hash: &str,
    current_session: &str,
    previous_session: &str,
    timeout: u64,
) -> Result<()> {
    info!("Starting cleanup of old sessions");
    
    let pod_sessions_path = sessions_path.join(pod_hash);
    if !pod_sessions_path.exists() {
        return Ok(());
    }

    let mut cleanup_count = 0;
    let current_time = SystemTime::now();

    for entry in fs::read_dir(&pod_sessions_path)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_dir() {
            let session_hash = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name,
                None => continue,
            };

            // Keep current session and previous session
            if session_hash == current_session || session_hash == previous_session {
                info!("Keeping session: {}", session_hash);
                continue;
            }

            // Safety check: don't delete recently created directories (within 5 minutes)
            let metadata = fs::metadata(&path)?;
            let mod_time = metadata.modified()?;
            
            if let Ok(duration) = current_time.duration_since(mod_time) {
                if duration.as_secs() < 300 {
                    info!(
                        "Safety: keeping recently created session {} (created {} seconds ago)",
                        session_hash,
                        duration.as_secs()
                    );
                    continue;
                }
            }

            info!("Removing old session: {}", session_hash);
            
            let output = Command::new("timeout")
                .arg(timeout.to_string())
                .arg("rm")
                .arg("-rf")
                .arg(&path)
                .output()
                .with_context(|| format!("Failed to remove directory: {}", path.display()))?;

            if output.status.success() {
                cleanup_count += 1;
            } else {
                warn!(
                    "Failed to remove session {}: {}",
                    session_hash,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    info!("Cleanup complete. Removed {} old sessions", cleanup_count);
    Ok(())
}

impl Clone for SessionInfo {
    fn clone(&self) -> Self {
        Self {
            pod_hash: self.pod_hash.clone(),
            snapshot_hash: self.snapshot_hash.clone(),
            path: self.path.clone(),
            created_at: self.created_at,
            mod_time: self.mod_time,
        }
    }
}