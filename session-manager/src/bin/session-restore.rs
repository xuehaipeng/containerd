use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser, Debug)]
#[command(
    name = "session-restore",
    about = "Containerd session restore tool for shared storage"
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
        default_value = "/etc/sessions",
        help = "Base path for session directories inside container"
    )]
    sessions_path: PathBuf,

    #[arg(
        long,
        default_value = "/etc/backup",
        help = "Backup storage path"
    )]
    backup_path: PathBuf,

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
    created_at: chrono::DateTime<chrono::Utc>,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    info!("=== Session Restore Tool Started ===");
    info!("Mappings file: {}", args.mappings_file.display());
    info!("Sessions path: {}", args.sessions_path.display());
    info!("Backup path: {}", args.backup_path.display());
    info!("Timeout: {} seconds", args.timeout);
    info!("Dry run: {}", args.dry_run);

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
            info!("No current session found in path mappings. Nothing to restore.");
            info!("=== Session Restore Completed (No Session Found) ===");
            return Ok(());
        }
    };

    info!(
        "Current session: pod_hash={}, snapshot_hash={}, created_at={}",
        current_session.pod_hash, current_session.snapshot_hash, current_session.created_at
    );

    // Construct current session directory path
    let current_session_dir = args.sessions_path
        .join(&current_session.pod_hash)
        .join(&current_session.snapshot_hash)
        .join("fs");

    info!("Current session directory: {}", current_session_dir.display());
    info!("Backup storage directory: {}", args.backup_path.display());

    // Validate backup storage directory exists and has content
    if !args.backup_path.exists() {
        warn!("Backup storage directory does not exist: {}", args.backup_path.display());
        info!("=== Session Restore Completed (No Backup Data) ===");
        return Ok(());
    }

    if is_directory_empty(&args.backup_path)? {
        warn!("Backup storage directory is empty: {}", args.backup_path.display());
        info!("=== Session Restore Completed (Empty Backup Data) ===");
        return Ok(());
    }

    // Show current session directory status before restore
    debug!("Current session directory status before restore:");
    if current_session_dir.exists() {
        debug!("  Current session directory exists");
        show_directory_contents(&current_session_dir)?;
    } else {
        debug!("  Current session directory does not exist yet");
    }

    // Show backup storage directory contents before restore
    debug!("Backup storage directory contents before restore:");
    show_directory_contents(&args.backup_path)?;

    // Ensure current session directory exists
    if !args.dry_run {
        fs::create_dir_all(&current_session_dir)
            .with_context(|| format!("Failed to create current session directory: {}", current_session_dir.display()))?;
        info!("Created current session directory: {}", current_session_dir.display());
    } else {
        info!("DRY RUN: Would create current session directory: {}", current_session_dir.display());
    }

    // Perform restore
    info!("Starting restore of session data from {} to {}...", 
          args.backup_path.display(), current_session_dir.display());

    if !args.dry_run {
        let result = restore_session_data(&args.backup_path, &current_session_dir, args.timeout)?;
        info!("Restore result: {} files copied, {} errors, {} skipped", 
              result.success_count, result.error_count, result.skipped_count);
        
        if !result.errors.is_empty() {
            warn!("Restore completed with some errors:");
            for error in &result.errors {
                warn!("  {}", error);
            }
        }
    } else {
        info!("DRY RUN: Would copy data from {} to {}", 
              args.backup_path.display(), current_session_dir.display());
    }

    // Show current session directory contents after restore
    debug!("Current session directory contents after restore:");
    if current_session_dir.exists() {
        show_directory_contents(&current_session_dir)?;
    }

    info!("=== Session Restore Completed ===");
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
    let mut latest_time: Option<chrono::DateTime<chrono::Utc>> = None;

    for (path_key, mapping) in path_mappings.mappings {
        if mapping.namespace == namespace
            && mapping.pod_name == pod_name
            && mapping.container_name == container_name
        {
            let created_at = chrono::DateTime::parse_from_rfc3339(&mapping.created_at)
                .with_context(|| format!("Invalid created_at timestamp: {}", mapping.created_at))?
                .with_timezone(&chrono::Utc);

            if latest_time.map_or(true, |t| created_at > t) {
                latest_time = Some(created_at);
                best_match = Some((path_key, mapping));
            }
        }
    }

    match best_match {
        Some((_, mapping)) => {
            let created_at = chrono::DateTime::parse_from_rfc3339(&mapping.created_at)?
                .with_timezone(&chrono::Utc);
            
            Ok(Some(SessionInfo {
                pod_hash: mapping.pod_hash,
                snapshot_hash: mapping.snapshot_hash,
                created_at,
            }))
        }
        None => Ok(None),
    }
}

fn is_directory_empty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(true);
    }
    
    let mut entries = fs::read_dir(path)?;
    Ok(entries.next().is_none())
}

fn show_directory_contents(path: &Path) -> Result<()> {
    if !path.exists() {
        debug!("  Directory does not exist: {}", path.display());
        return Ok(());
    }
    
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let metadata = entry.metadata()?;
        
        if metadata.is_dir() {
            debug!("  d {}", file_name.to_string_lossy());
        } else {
            debug!("  f {}", file_name.to_string_lossy());
        }
    }
    
    Ok(())
}

#[derive(Debug)]
struct RestoreResult {
    success_count: usize,
    error_count: usize,
    skipped_count: usize,
    errors: Vec<String>,
}

fn restore_session_data(source: &Path, target: &Path, timeout: u64) -> Result<RestoreResult> {
    let mut result = RestoreResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    // Try rsync first if available
    if which::which("rsync").is_ok() {
        info!("Using rsync for restore");
        
        let output = Command::new("timeout")
            .arg(timeout.to_string())
            .arg("rsync")
            .arg("-av")
            .arg("--delete")
            .arg("--ignore-errors")
            .arg("--force")
            .arg(format!("{}/", source.display()))
            .arg(format!("{}/", target.display()))
            .output()
            .with_context(|| "Failed to execute rsync")?;

        if output.status.success() {
            info!("Rsync restore completed successfully");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Rsync restore completed with warnings: {}", stderr);
            result.errors.push(format!("Rsync warnings: {}", stderr));
        }
        
        result.success_count = 1; // Simplified counting for rsync
    } else {
        // Fallback to tar if rsync is not available
        info!("Rsync not available, using tar for restore");
        
        // Create tar archive and extract it to target
        let source_tar = Command::new("timeout")
            .arg(timeout.to_string())
            .arg("tar")
            .arg("-cf")
            .arg("-")
            .arg("--exclude=.*.tar")
            .arg("--ignore-failed-read")
            .arg("-C")
            .arg(source)
            .arg(".")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .with_context(|| "Failed to start tar source command")?;

        let target_tar = Command::new("timeout")
            .arg(timeout.to_string())
            .arg("tar")
            .arg("-xf")
            .arg("-")
            .arg("--overwrite")
            .arg("-C")
            .arg(target)
            .stdin(source_tar.stdout.unwrap())
            .output()
            .with_context(|| "Failed to execute tar target command")?;

        if target_tar.status.success() {
            info!("Tar restore completed successfully");
        } else {
            let stderr = String::from_utf8_lossy(&target_tar.stderr);
            if stderr.contains("Exiting with failure status due to previous errors") {
                warn!("Tar restore completed with some skipped files (this is normal)");
                result.skipped_count += 1;
            } else {
                warn!("Tar restore failed: {}", stderr);
                result.errors.push(format!("Tar error: {}", stderr));
                result.error_count += 1;
            }
        }
    }

    Ok(result)
}