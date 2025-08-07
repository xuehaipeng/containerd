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
/// Kills all running processes to ensure complete container shutdown
fn force_terminate_container(grace_seconds: u64, dry_run: bool) -> Result<()> {
    info!("=== Post-Backup Container Termination Started ===");
    info!("Grace period: {} seconds", grace_seconds);
    info!("Dry run mode: {}", dry_run);

    if dry_run {
        info!("DRY RUN: Would list all processes, send SIGTERM to all, wait {} seconds, then SIGKILL if needed", grace_seconds);
        return Ok(());
    }

    // Step 1: List all running processes (excluding kernel threads and this process)
    let running_processes = list_all_running_processes()?;
    info!("Found {} running processes to terminate", running_processes.len());
    
    if running_processes.is_empty() {
        info!("No user processes found, container termination not needed");
        return Ok(());
    }

    // Step 2: Send SIGTERM to all processes (excluding kernel threads)
    info!("Sending SIGTERM to all {} running processes...", running_processes.len());
    let mut term_success_count = 0;
    
    for process in &running_processes {
        debug!("Sending SIGTERM to PID {} ({})", process.pid, process.name);
        
        match Command::new("kill")
            .arg("-TERM")
            .arg(&process.pid.to_string())
            .output() 
        {
            Ok(output) => {
                if output.status.success() {
                    term_success_count += 1;
                    debug!("SIGTERM sent successfully to PID {}", process.pid);
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stderr.contains("No such process") {
                        warn!("Failed to send SIGTERM to PID {}: {}", process.pid, stderr);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to execute kill command for PID {}: {}", process.pid, e);
            }
        }
    }
    
    info!("SIGTERM sent to {}/{} processes", term_success_count, running_processes.len());

    // Step 3: Wait for graceful termination
    info!("Waiting {} seconds for graceful termination of all processes...", grace_seconds);
    thread::sleep(Duration::from_secs(grace_seconds));

    // Step 4: Check which processes are still running and send SIGKILL if needed
    info!("Checking for remaining processes after grace period...");
    let remaining_processes = list_all_running_processes()?;
    
    if remaining_processes.is_empty() {
        info!("All processes terminated gracefully, no SIGKILL needed");
    } else {
        warn!("Found {} processes still running after grace period, sending SIGKILL", remaining_processes.len());
        
        let mut kill_success_count = 0;
        for process in &remaining_processes {
            debug!("Sending SIGKILL to PID {} ({})", process.pid, process.name);
            
            match Command::new("kill")
                .arg("-KILL")
                .arg(&process.pid.to_string())
                .output() 
            {
                Ok(output) => {
                    if output.status.success() {
                        kill_success_count += 1;
                        debug!("SIGKILL sent successfully to PID {}", process.pid);
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if !stderr.contains("No such process") {
                            error!("Failed to send SIGKILL to PID {}: {}", process.pid, stderr);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to execute kill command for PID {}: {}", process.pid, e);
                }
            }
        }
        
        info!("SIGKILL sent to {}/{} remaining processes", kill_success_count, remaining_processes.len());
        
        // Give a moment for SIGKILL to take effect
        thread::sleep(Duration::from_secs(2));
        
        // Final check
        let final_processes = list_all_running_processes()?;
        if final_processes.is_empty() {
            info!("All processes successfully terminated");
        } else {
            warn!("Warning: {} processes may still be running after SIGKILL", final_processes.len());
            for process in &final_processes {
                warn!("  Still running: PID {} ({})", process.pid, process.name);
            }
        }
    }

    info!("=== Post-Backup Container Termination Completed ===");
    Ok(())
}

#[derive(Debug)]
struct ProcessInfo {
    pid: u32,
    name: String,
    ppid: u32,
}

/// List all running user processes (excluding kernel threads, init, and this process)
fn list_all_running_processes() -> Result<Vec<ProcessInfo>> {
    // Use different ps command based on OS
    let output = if cfg!(target_os = "macos") {
        Command::new("ps")
            .arg("-eo")
            .arg("pid,ppid,comm,stat")
            .output()
            .with_context(|| "Failed to execute ps command")?
    } else {
        // Linux version
        Command::new("ps")
            .arg("-eo")
            .arg("pid,ppid,comm,stat")
            .arg("--no-headers")
            .output()
            .with_context(|| "Failed to execute ps command")?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("ps command failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    let current_pid = std::process::id();
    let mut skip_header = true;
    
    for line in stdout.lines() {
        // Skip header line on macOS (first line)
        if skip_header && cfg!(target_os = "macos") {
            skip_header = false;
            continue;
        }
        
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        if parts.len() >= 4 {
            if let (Ok(pid), Ok(ppid)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                let name = parts[2].to_string();
                let stat = parts[3];
                
                // Skip this process
                if pid == current_pid {
                    continue;
                }
                
                // Skip kernel threads (processes with names in [brackets])
                if name.starts_with('[') && name.ends_with(']') {
                    continue;
                }
                
                // Skip zombie processes (stat contains 'Z')
                if stat.contains('Z') {
                    continue;
                }
                
                // Include all other processes (including PID 1)
                processes.push(ProcessInfo {
                    pid,
                    name,
                    ppid,
                });
            }
        }
    }
    
    // Sort processes by PID for consistent ordering
    // In a container environment, this ensures child processes are typically terminated before parents
    processes.sort_by_key(|p| p.pid);
    
    debug!("Process termination order:");
    for (i, process) in processes.iter().enumerate() {
        debug!("  {}: PID {} ({}) - PPID {}", i + 1, process.pid, process.name, process.ppid);
    }
    
    Ok(processes)
}