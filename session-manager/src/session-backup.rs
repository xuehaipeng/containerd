use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn, debug, error};
use session_manager::*;
use session_manager::lockless_backup::{execute_backup_with_safety_check, create_directory_simple};
use std::path::PathBuf;
use std::fs::OpenOptions;
use std::process::Command;
use std::thread;
use std::time::Duration;

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

    #[arg(long, help = "Force terminate container immediately after successful backup")]
    force_terminate_after_backup: bool,

    #[arg(
        long,
        default_value = "30",
        help = "Grace period in seconds between SIGTERM and SIGKILL when force terminating (requires --force-terminate-after-backup)"
    )]
    termination_grace_seconds: u64,
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
    info!("Force terminate after backup: {}", args.force_terminate_after_backup);
    if args.force_terminate_after_backup {
        info!("Termination grace period: {} seconds", args.termination_grace_seconds);
    }

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

                // Force terminate container if requested
                if args.force_terminate_after_backup {
                    info!("Backup completed successfully - initiating immediate container termination");
                    
                    match force_terminate_container(args.termination_grace_seconds, args.dry_run) {
                        Ok(()) => {
                            info!("Container termination completed successfully");
                        }
                        Err(e) => {
                            error!("Container termination failed: {}", e);
                            // Don't fail the backup operation due to termination issues
                            warn!("Backup succeeded but termination failed - container will terminate normally via Kubernetes");
                        }
                    }
                } else {
                    info!("Container will terminate normally via Kubernetes (--force-terminate-after-backup not specified)");
                }
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

/// Force terminate container after successful backup completion
/// This helps pods exit immediately instead of waiting for the full terminationGracePeriodSeconds
fn force_terminate_container(grace_seconds: u64, dry_run: bool) -> Result<()> {
    info!("=== Post-Backup Container Termination Started ===");
    info!("Grace period: {} seconds", grace_seconds);
    info!("Dry run mode: {}", dry_run);

    if dry_run {
        info!("DRY RUN: Would send SIGTERM to PID 1, wait {} seconds, then SIGKILL if needed", grace_seconds);
        return Ok(());
    }

    // Step 1: Send SIGTERM to PID 1 (main container process)
    info!("Sending SIGTERM to PID 1 (main container process)");
    
    match Command::new("kill")
        .arg("-TERM")
        .arg("1")
        .output() 
    {
        Ok(output) => {
            if output.status.success() {
                info!("SIGTERM sent successfully to PID 1");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Failed to send SIGTERM to PID 1: {}", stderr);
            }
        }
        Err(e) => {
            error!("Failed to execute kill command for SIGTERM: {}", e);
            // Continue with the process - we'll try SIGKILL anyway
        }
    }

    // Step 2: Wait for graceful termination
    info!("Waiting {} seconds for graceful termination...", grace_seconds);
    thread::sleep(Duration::from_secs(grace_seconds));

    // Step 3: Check if process still exists, if so send SIGKILL
    info!("Checking if PID 1 still exists...");
    
    let still_running = match Command::new("kill")
        .arg("-0")  // Check if process exists without sending signal
        .arg("1")
        .output() 
    {
        Ok(output) => output.status.success(),
        Err(_) => false, // Assume process is gone if we can't check
    };

    if still_running {
        warn!("PID 1 still running after {} seconds, sending SIGKILL", grace_seconds);
        
        match Command::new("kill")
            .arg("-KILL")
            .arg("1")
            .output() 
        {
            Ok(output) => {
                if output.status.success() {
                    info!("SIGKILL sent successfully to PID 1");
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!("Failed to send SIGKILL to PID 1: {}", stderr);
                }
            }
            Err(e) => {
                error!("Failed to execute kill command for SIGKILL: {}", e);
            }
        }
        
        // Give a moment for SIGKILL to take effect
        thread::sleep(Duration::from_secs(1));
        info!("Container termination process completed");
    } else {
        info!("PID 1 terminated gracefully, no SIGKILL needed");
    }

    info!("=== Post-Backup Container Termination Completed ===");
    Ok(())
}