use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug};
use session_manager::*;
use session_manager::direct_restore::DirectRestoreEngine;
use std::path::PathBuf;
use std::fs::OpenOptions;
use std::time::Instant;
use chrono;

/// Get version string with git hash and build timestamp
fn get_version() -> &'static str {
    concat!(
        env!("CARGO_PKG_VERSION"),
        " (git: ", env!("GIT_HASH"),
        ", branch: ", env!("GIT_BRANCH"),
        ", built: ", env!("BUILD_TIME"), ")"
    )
}

#[derive(Parser, Debug)]
#[command(
    name = "session-restore",
    about = "Containerd session restore tool with direct container root restoration",
    version = get_version(),
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

    #[arg(long, help = "Fast mode - reduced validation for better performance")]
    fast_mode: bool,

    #[arg(long, help = "Use async operations for improved performance")]
    async_mode: bool,
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

#[tokio::main]
async fn main() -> Result<()> {
    // Record start time for total execution timing
    let start_time = Instant::now();
    
    // Initialize file-based logging to /tmp
    init_file_logging("session-restore")?;
    let args = Args::parse();

    info!("=== Session Restore Tool Started (Direct Container Root Mode) ===");
    info!("Version: {}", get_version());
    info!("Backup path: {}", args.backup_path.display());
    info!("Timeout: {} seconds", args.timeout);
    info!("Dry run: {}", args.dry_run);
    info!("Fast mode: {}", args.fast_mode);
    info!("Async mode: {}", args.async_mode);
    info!("Using COPY-ONLY mode for crash safety - cleanup happens after full success");

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
        
        // Create completion marker even when no backup data exists
        let completion_marker_path = "/tmp/session-restore-complete";
        if let Err(e) = std::fs::write(completion_marker_path, format!("no backup data at {}", chrono::Utc::now().to_rfc3339())) {
            warn!("Failed to create completion marker at {}: {}", completion_marker_path, e);
        } else {
            info!("Created completion marker: {}", completion_marker_path);
        }
        
        info!("=== Session Restore Completed (No Backup Data) ===");
        return Ok(());
    }

    if is_directory_empty(&args.backup_path)? {
        warn!("Backup storage directory is empty: {}", args.backup_path.display());
        
        // Create completion marker even when backup directory is empty
        let completion_marker_path = "/tmp/session-restore-complete";
        if let Err(e) = std::fs::write(completion_marker_path, format!("empty backup data at {}", chrono::Utc::now().to_rfc3339())) {
            warn!("Failed to create completion marker at {}: {}", completion_marker_path, e);
        } else {
            info!("Created completion marker: {}", completion_marker_path);
        }
        
        info!("=== Session Restore Completed (Empty Backup Data) ===");
        return Ok(());
    }

    // Show backup storage directory contents before restore
    debug!("Backup storage directory contents before restore:");
    show_directory_contents(&args.backup_path)?;

    // Create direct restore engine with performance optimizations
    let mut restore_engine = DirectRestoreEngine::new(args.dry_run, args.timeout);
    
    // Apply performance optimizations
    if args.fast_mode {
        restore_engine = restore_engine.with_fast_mode(true);
        info!("Fast mode enabled - reduced validation overhead");
    }
    
    if args.async_mode {
        restore_engine = restore_engine.with_async_mode(true);
        info!("Async mode enabled - using async I/O operations");
    }

    // Perform direct container root restoration
    // Prefer buffered copy for overlayfs: set env hint to tune fast_copy
   std::env::set_var("SESSION_MANAGER_FORCE_OVERLAY", "true");

   info!("Starting optimized direct container root restoration from {}...", args.backup_path.display());

    let result = if args.async_mode {
        // Use async operations for better performance
        restore_engine.restore_to_container_root_async(&args.backup_path).await
            .with_context(|| "Failed to perform async direct container root restoration")?
    } else {
        restore_engine.restore_to_container_root(&args.backup_path)
            .with_context(|| "Failed to perform direct container root restoration")?
    };

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

    // Calculate and log total execution time
    let total_duration = start_time.elapsed();
    
    // Create completion marker for synchronization with main container process
    let completion_marker_path = "/tmp/session-restore-complete";
    if let Err(e) = std::fs::write(completion_marker_path, format!("completed at {}", chrono::Utc::now().to_rfc3339())) {
        warn!("Failed to create completion marker at {}: {}", completion_marker_path, e);
        // Don't fail the operation for marker creation issues
    } else {
        info!("Created completion marker: {}", completion_marker_path);
    }
    
    info!("=== Session Restore Completed Successfully ===");
    info!("Total execution time: {:.3}s", total_duration.as_secs_f64());
    Ok(())
}