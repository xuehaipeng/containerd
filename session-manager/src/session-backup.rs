use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug};
use session_manager::*;
use session_manager::lockless_backup::{execute_backup_with_safety_check, create_directory_simple};
use std::path::PathBuf;
use std::fs::OpenOptions;

#[derive(Parser, Debug)]
#[command(
    name = "session-backup",
    about = "Lockless containerd session backup tool optimized for single-process operations"
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

    #[arg(long, default_value = "900", help = "Operation timeout in seconds")]
    timeout: u64,

    #[arg(long, help = "Dry run mode - don't actually copy files")]
    dry_run: bool,

    #[arg(long, default_value = "true", help = "Whether to bypass mounted paths during backup")]
    bypass_mounts: bool,
}

fn init_file_logging(binary_name: &str) -> Result<()> {
    use env_logger::fmt::Target;
    
    // Create log file path
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let log_file_path = format!("/tmp/{}-{}.log", binary_name, timestamp);
    
    // Create or open log file
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("Failed to create log file: {}", log_file_path))?;
    
    // Initialize env_logger with file target and debug level
    env_logger::Builder::new()
        .target(Target::Pipe(Box::new(log_file)))
        .filter_level(log::LevelFilter::Debug)
        .format_timestamp_secs()
        .init();
    
    // Also log to stderr for immediate feedback
    eprintln!("Logging to file: {}", log_file_path);
    
    Ok(())
}

fn main() -> Result<()> {
    // Initialize file-based logging to /tmp
    init_file_logging("session-backup")?;
    let args = Args::parse();

    info!("=== Session Backup Tool Started (Lockless) ===");
    info!("Mappings file: {}", args.mappings_file.display());
    info!("Sessions path: {}", args.sessions_path.display());
    info!("Backup path: {}", args.backup_path.display());
    info!("Timeout: {} seconds", args.timeout);
    info!("Dry run: {}", args.dry_run);
    info!("Bypass mounts: {}", args.bypass_mounts);

    // Initialize Tokio runtime for async operations
    let rt = tokio::runtime::Runtime::new()
        .context("Failed to create async runtime")?;

    rt.block_on(async {
        // Get current pod information
        let pod_info = PodInfo::from_args_and_env(
            args.namespace,
            args.pod_name,
            args.container_name,
        ).with_context(|| "Failed to determine pod information")?;

        info!(
            "Pod info: namespace={}, pod={}, container={}",
            pod_info.namespace, pod_info.pod_name, pod_info.container_name
        );

        // Find current session directory asynchronously
        let session_info = find_current_session_async(&args.mappings_file, &pod_info).await?;

        let session_info = match session_info {
            Some(info) => info,
            None => {
                warn!("No current session found for namespace={}, pod={}, container={}", 
                      pod_info.namespace, pod_info.pod_name, pod_info.container_name);
                info!("=== Session Backup Completed (No Session Found) ===");
                return Ok(());
            }
        };

        info!(
            "Current session: pod_hash={}, snapshot_hash={}, created_at={}",
            session_info.pod_hash, session_info.snapshot_hash, session_info.created_at
        );

        // Build current session directory path
        let current_session_dir = args.sessions_path
            .join(&session_info.pod_hash)
            .join(&session_info.snapshot_hash)
            .join("fs");

        info!("Current session directory: {}", current_session_dir.display());
        info!("Backup storage directory: {}", args.backup_path.display());

        // Validate that session directory exists and has content
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

        // Show directory contents before backup
        debug!("Current session directory contents before backup:");
        show_directory_contents(&current_session_dir)?;

        debug!("Backup storage directory contents before backup:");
        show_directory_contents(&args.backup_path)?;

        // Execute lockless backup operation
        info!("Starting lockless backup operation...");
        
        let backup_operation = format!("session-backup-{}-{}-{}", 
                                      pod_info.namespace, pod_info.pod_name, pod_info.container_name);

        let result = execute_backup_with_safety_check(&args.backup_path, &backup_operation, || {
            perform_backup_operation(&current_session_dir, &args.backup_path, args.timeout, args.bypass_mounts, args.dry_run)
        });

        match result {
            Ok(()) => {
                info!("=== Session Backup Completed Successfully ===");
                
                // Show final backup directory contents
                debug!("Backup storage directory contents after backup:");
                show_directory_contents(&args.backup_path)?;
            }
            Err(e) => {
                return Err(e).with_context(|| "Session backup operation failed");
            }
        }

        Ok(())
    })
}

/// Perform the actual backup operation without locking
fn perform_backup_operation(
    source_dir: &PathBuf,
    backup_dir: &PathBuf,
    timeout: u64,
    bypass_mounts: bool,
    dry_run: bool,
) -> Result<()> {
    info!("Performing lockless backup: {} -> {}", source_dir.display(), backup_dir.display());

    // Create backup directory (lockless)
    create_directory_simple(backup_dir)
        .with_context(|| format!("Failed to create backup directory: {}", backup_dir.display()))?;

    if dry_run {
        info!("DRY RUN: Would backup {} to {}", source_dir.display(), backup_dir.display());
        return Ok(());
    }

    // Perform the actual transfer
    let transfer_result = if bypass_mounts {
        info!("Using mount-bypass transfer for lockless backup");
        transfer_data_with_mount_bypass(source_dir, backup_dir, timeout, true)
    } else {
        info!("Using standard transfer for lockless backup");
        transfer_data(source_dir, backup_dir, timeout)
    };

    match transfer_result {
        Ok(result) => {
            info!("Backup transfer completed:");
            info!("  Success count: {}", result.success_count);
            info!("  Error count: {}", result.error_count);
            info!("  Skipped count: {}", result.skipped_count);
            
            if result.error_count > 0 {
                warn!("Backup completed with {} errors:", result.error_count);
                for error in &result.errors {
                    warn!("  - {}", error);
                }
            }
            
            // Consider backup successful even with some errors (common with busy files)
            if result.success_count > 0 || result.error_count == 0 {
                info!("Lockless backup operation succeeded");
                Ok(())
            } else {
                Err(anyhow::anyhow!("Backup failed: {} errors, no successful transfers", result.error_count))
            }
        }
        Err(e) => {
            Err(e).with_context(|| "Backup transfer operation failed")
        }
    }
}