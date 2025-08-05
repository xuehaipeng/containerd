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
    name = "session-backup",
    about = "Containerd session backup tool for shared storage"
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

    info!("=== Session Backup Tool Started ===");
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
            info!("No current session found in path mappings. Nothing to backup.");
            info!("=== Session Backup Completed (No Session Found) ===");
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

    // Validate current session directory exists and has content
    if !current_session_dir.exists() {
        warn!("Current session directory does not exist: {}", current_session_dir.display());
        info!("=== Session Backup Completed (No Session Directory) ===");
        return Ok(());
    }

    if is_directory_empty(&current_session_dir)? {
        warn!("Current session directory is empty: {}", current_session_dir.display());
        info!("=== Session Backup Completed (Empty Session Directory) ===");
        return Ok(());
    }

    // Show current session directory contents before backup
    debug!("Current session directory contents before backup:");
    show_directory_contents(&current_session_dir)?;

    // Show backup storage directory contents before backup
    debug!("Backup storage directory contents before backup:");
    if args.backup_path.exists() {
        show_directory_contents(&args.backup_path)?;
    } else {
        debug!("Backup storage directory does not exist yet");
    }

    // Create backup storage directory if it doesn't exist
    if !args.dry_run {
        fs::create_dir_all(&args.backup_path)
            .with_context(|| format!("Failed to create backup storage directory: {}", args.backup_path.display()))?;
        info!("Created backup storage directory: {}", args.backup_path.display());
    } else {
        info!("DRY RUN: Would create backup storage directory: {}", args.backup_path.display());
    }

    // Perform backup
    info!("Starting backup of session data from {} to {}...", 
          current_session_dir.display(), args.backup_path.display());

    if !args.dry_run {
        let result = backup_session_data(&current_session_dir, &args.backup_path, args.timeout)?;
        info!("Backup result: {} files copied, {} errors, {} skipped", 
              result.success_count, result.error_count, result.skipped_count);
        
        if !result.errors.is_empty() {
            warn!("Backup completed with some errors:");
            for error in &result.errors {
                warn!("  {}", error);
            }
        }
    } else {
        info!("DRY RUN: Would copy data from {} to {}", 
              current_session_dir.display(), args.backup_path.display());
    }

    // Show backup storage directory contents after backup
    debug!("Backup storage directory contents after backup:");
    if args.backup_path.exists() {
        show_directory_contents(&args.backup_path)?;
    }

    info!("=== Session Backup Completed ===");
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
struct BackupResult {
    success_count: usize,
    error_count: usize,
    skipped_count: usize,
    errors: Vec<String>,
}

fn backup_session_data(source: &Path, target: &Path, timeout: u64) -> Result<BackupResult> {
    let mut result = BackupResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    // Try rsync first if available
    if which::which("rsync").is_ok() {
        info!("Using rsync for backup");
        
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
            info!("Rsync backup completed successfully");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Rsync backup completed with warnings: {}", stderr);
            result.errors.push(format!("Rsync warnings: {}", stderr));
        }
        
        result.success_count = 1; // Simplified counting for rsync
    } else {
        // Fallback to tar if rsync is not available
        info!("Rsync not available, using tar for backup");
        
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
            info!("Tar backup completed successfully");
        } else {
            let stderr = String::from_utf8_lossy(&target_tar.stderr);
            if stderr.contains("Exiting with failure status due to previous errors") {
                warn!("Tar backup completed with some skipped files (this is normal)");
                result.skipped_count += 1;
            } else {
                warn!("Tar backup failed: {}", stderr);
                result.errors.push(format!("Tar error: {}", stderr));
                result.error_count += 1;
            }
        }
    }

    Ok(result)
}