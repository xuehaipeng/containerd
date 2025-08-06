use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug};
use session_manager::*;
use std::path::PathBuf;
use std::fs::OpenOptions;

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

    info!("=== Optimized Session Backup Tool Started ===");
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

        // Parse path mappings to find current session using optimized async loader
        let current_session = match session_manager::find_current_session_async(&args.mappings_file, &pod_info).await? {
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
            create_directory_with_lock(&args.backup_path)
                .with_context(|| format!("Failed to create backup storage directory: {}", args.backup_path.display()))?;
        } else {
            info!("DRY RUN: Would create backup storage directory: {}", args.backup_path.display());
        }

        // Perform optimized backup with mount bypass option
        info!("Starting optimized backup of session data from {} to {}...", 
              current_session_dir.display(), args.backup_path.display());

        if !args.dry_run {
            // Use optimized transfer with mount bypass capability
            let result = session_manager::transfer_data_with_mount_bypass(&current_session_dir, &args.backup_path, args.timeout, args.bypass_mounts)
                .with_context(|| "Failed to backup session data with optimized transfer")?;
            
            info!("Optimized backup result: {} files copied, {} errors, {} skipped", 
                  result.success_count, result.error_count, result.skipped_count);
            
            if !result.errors.is_empty() {
                warn!("Backup completed with some errors:");
                for error in &result.errors {
                    warn!("  {}", error);
                }
            }
            
            if result.error_count > 0 && result.success_count == 0 {
                return Err(anyhow::anyhow!("Optimized backup failed: {} errors occurred", result.error_count));
            }
        } else {
            info!("DRY RUN: Would copy data from {} to {} using optimized operations{}", 
                  current_session_dir.display(), args.backup_path.display(),
                  if args.bypass_mounts { " with mount bypass" } else { "" });
        }

        // Show backup storage directory contents after backup
        debug!("Backup storage directory contents after optimized backup:");
        if args.backup_path.exists() {
            show_directory_contents(&args.backup_path)?;
        }

        info!("=== Optimized Session Backup Completed ===");
        Ok(())
    })
}

