use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug};
use session_manager::*;
use std::path::PathBuf;
use std::fs::OpenOptions;

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
    init_file_logging("session-restore")?;
    let args = Args::parse();

    info!("=== Session Restore Tool Started ===");
    info!("Mappings file: {}", args.mappings_file.display());
    info!("Sessions path: {}", args.sessions_path.display());
    info!("Backup path: {}", args.backup_path.display());
    info!("Timeout: {} seconds", args.timeout);
    info!("Dry run: {}", args.dry_run);

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

    // Parse path mappings to find current session
    let current_session = match find_current_session(&args.mappings_file, &pod_info)? {
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
        create_directory_with_lock(&current_session_dir)
            .with_context(|| format!("Failed to create current session directory: {}", current_session_dir.display()))?;
    } else {
        info!("DRY RUN: Would create current session directory: {}", current_session_dir.display());
    }

    // Perform restore
    info!("Starting restore of session data from {} to {}...", 
          args.backup_path.display(), current_session_dir.display());

    if !args.dry_run {
        let result = transfer_data(&args.backup_path, &current_session_dir, args.timeout)
            .with_context(|| "Failed to restore session data")?;
        info!("Restore result: {} files copied, {} errors, {} skipped", 
              result.success_count, result.error_count, result.skipped_count);
        
        if !result.errors.is_empty() {
            warn!("Restore completed with some errors:");
            for error in &result.errors {
                warn!("  {}", error);
            }
        }
        
        if result.error_count > 0 && result.success_count == 0 {
            return Err(anyhow::anyhow!("Restore failed: {} errors occurred", result.error_count));
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

