use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug};
use session_manager::*;
use session_manager::direct_restore::DirectRestoreEngine;
use std::path::PathBuf;
use std::fs::OpenOptions;

#[derive(Parser, Debug)]
#[command(
    name = "session-restore",
    about = "Containerd session restore tool with direct container root restoration"
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

    info!("=== Session Restore Tool Started (Direct Container Root Mode) ===");
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

    // Show backup storage directory contents before restore
    debug!("Backup storage directory contents before restore:");
    show_directory_contents(&args.backup_path)?;

    // Create direct restore engine
    let restore_engine = DirectRestoreEngine::new(args.dry_run, args.timeout);

    // Perform direct container root restoration
    info!("Starting direct container root restoration from {}...", args.backup_path.display());

    let result = restore_engine.restore_to_container_root(&args.backup_path)
        .with_context(|| "Failed to perform direct container root restoration")?;

    // Report results
    info!("=== Direct Container Root Restoration Results ===");
    info!("Total files processed: {}", result.total_files);
    info!("Successfully restored: {}", result.successful_files);
    info!("Skipped files: {}", result.skipped_files);
    info!("Failed files: {}", result.failed_files);
    info!("Cleaned backup files: {}", result.cleaned_files);
    info!("Duration: {:?}", result.duration);

    if !result.skipped_details.is_empty() {
        info!("Skipped files details:");
        for skipped in &result.skipped_details {
            info!("  {} - {}", skipped.path.display(), skipped.reason);
        }
    }

    if !result.failed_details.is_empty() {
        warn!("Failed files details:");
        for failed in &result.failed_details {
            warn!("  {} - {}", failed.path.display(), failed.error);
        }
    }

    if result.cleaned_files > 0 {
        info!("Successfully cleaned {} backup files after restoration", result.cleaned_files);
    }

    // Determine overall success
    let success_rate = if result.total_files > 0 {
        (result.successful_files as f64 / result.total_files as f64) * 100.0
    } else {
        100.0
    };

    info!("Restoration success rate: {:.1}%", success_rate);

    if result.failed_files > 0 && result.successful_files == 0 {
        return Err(anyhow::anyhow!("Restoration failed: {} files failed, 0 succeeded", result.failed_files));
    }

    info!("=== Session Restore Completed Successfully ===");
    Ok(())
}