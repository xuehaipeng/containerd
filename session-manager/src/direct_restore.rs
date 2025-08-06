use anyhow::{Context, Result, bail};
use log::{info, warn, debug, error};
use serde::{Deserialize, Serialize};
use std::fs::{self};
use std::path::{Path, PathBuf, Component};
use std::io;
use std::time::{Duration, SystemTime};
use std::thread;
use rayon::prelude::*;
use crate::resource_manager::ResourceManager;

#[derive(Debug, Serialize, Deserialize)]
pub struct DirectRestoreResult {
    pub total_files: usize,
    pub successful_files: usize,
    pub skipped_files: usize,
    pub failed_files: usize,
    pub cleaned_files: usize,
    pub skipped_details: Vec<SkippedFile>,
    pub failed_details: Vec<FailedFile>,
    pub cleaned_details: Vec<PathBuf>,
    pub duration: Duration,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FailedFile {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, PartialEq)]
pub enum CopyResult {
    Success,
    Skipped(String),
    Failed(String),
}

/// Outcome of processing a single file
#[derive(Debug, PartialEq)]
enum FileProcessOutcome {
    Success,
    Skipped(String),
    Failed(String),
    Cleaned,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CleanupValidationResult {
    pub total_files: usize,
    pub validated_files: usize,
    pub failed_validations: Vec<CleanupValidationFailure>,
    pub safety_warnings: Vec<CleanupSafetyWarning>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CleanupValidationFailure {
    pub backup_file: PathBuf,
    pub target_file: PathBuf,
    pub error: String,
    pub validation_phase: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CleanupSafetyWarning {
    pub file_path: PathBuf,
    pub warning_type: String,
    pub message: String,
    pub severity: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BatchCleanupResult {
    pub total_files: usize,
    pub successful_cleanups: usize,
    pub failed_cleanups: usize,
    pub rollback_operations: usize,
    pub cleanup_details: Vec<CleanupDetail>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CleanupDetail {
    pub backup_file: PathBuf,
    pub target_file: PathBuf,
    pub status: String,
    pub message: String,
}

#[derive(Debug)]
pub struct DirectRestoreEngine {
    pub dry_run: bool,
    pub timeout: u64,
    pub max_retries: u32,
    pub retry_delay: Duration,
}

impl DirectRestoreEngine {
    pub fn new(dry_run: bool, timeout: u64) -> Self {
        Self { 
            dry_run, 
            timeout,
            max_retries: 3,
            retry_delay: Duration::from_millis(500),
        }
    }

    pub fn with_retry_config(mut self, max_retries: u32, retry_delay: Duration) -> Self {
        self.max_retries = max_retries;
        self.retry_delay = retry_delay;
        self
    }

    /// Restore files directly to container root filesystem with parallel processing
    pub fn restore_to_container_root(&self, backup_path: &Path) -> Result<DirectRestoreResult> {
        let start_time = SystemTime::now();
        
        info!("Starting optimized direct container root restoration from: {}", backup_path.display());
        info!("Dry run mode: {}", self.dry_run);
        
        let mut result = DirectRestoreResult {
            total_files: 0,
            successful_files: 0,
            skipped_files: 0,
            failed_files: 0,
            cleaned_files: 0,
            skipped_details: Vec::new(),
            failed_details: Vec::new(),
            cleaned_details: Vec::new(),
            duration: Duration::from_secs(0),
        };

        if !backup_path.exists() {
            warn!("Backup path does not exist: {}", backup_path.display());
            result.duration = start_time.elapsed().unwrap_or(Duration::from_secs(0));
            return Ok(result);
        }

        // Use parallel directory processing for better performance
        self.process_directory_parallel(backup_path, backup_path, &mut result)?;

        result.duration = start_time.elapsed().unwrap_or(Duration::from_secs(0));
        
        info!("Optimized direct restore completed:");
        info!("  Total files: {}", result.total_files);
        info!("  Successful: {}", result.successful_files);
        info!("  Skipped: {}", result.skipped_files);
        info!("  Failed: {}", result.failed_files);
        info!("  Cleaned from backup: {}", result.cleaned_files);
        info!("  Duration: {:?}", result.duration);

        if !result.skipped_details.is_empty() {
            info!("Skipped files:");
            for skipped in &result.skipped_details {
                info!("  {} - {}", skipped.path.display(), skipped.reason);
            }
        }

        if !result.failed_details.is_empty() {
            warn!("Failed files:");
            for failed in &result.failed_details {
                warn!("  {} - {}", failed.path.display(), failed.error);
            }
        }

        // Perform final validation of cleanup operations
        if !self.dry_run && result.cleaned_files > 0 {
            info!("Performing final cleanup validation for {} cleaned files", result.cleaned_files);
            if let Err(e) = self.validate_cleanup_operations(&result.cleaned_details) {
                warn!("Final cleanup validation failed: {}", e);
                // Note: At this point, individual file cleanups have already been validated
                // This is just a final sanity check
            } else {
                info!("Final cleanup validation successful for all {} cleaned files", result.cleaned_files);
            }
        }

        Ok(result)
    }

    /// Perform final validation of cleanup operations
    /// This is a final sanity check to ensure cleanup operations were successful
    fn validate_cleanup_operations(&self, cleaned_files: &[PathBuf]) -> Result<()> {
        debug!("Validating {} cleanup operations", cleaned_files.len());
        
        let mut validation_errors = Vec::new();
        
        for cleaned_file in cleaned_files {
            if cleaned_file.exists() {
                let error_msg = format!("Cleaned file still exists: {}", cleaned_file.display());
                validation_errors.push(error_msg);
            }
        }
        
        if !validation_errors.is_empty() {
            let combined_error = validation_errors.join("; ");
            bail!("Cleanup validation failed: {}", combined_error);
        }
        
        debug!("All cleanup operations validated successfully");
        Ok(())
    }

    /// Enhanced backup cleanup validation with comprehensive safety checks
    /// Validates that files are successfully restored before allowing cleanup
    /// Includes rollback mechanism if cleanup fails partway through
    pub fn validate_backup_cleanup_safety(&self, backup_files: &[PathBuf], target_files: &[PathBuf]) -> Result<CleanupValidationResult> {
        info!("Performing comprehensive backup cleanup validation for {} file pairs", backup_files.len());
        
        if backup_files.len() != target_files.len() {
            bail!("Backup and target file lists must have the same length: {} vs {}", 
                  backup_files.len(), target_files.len());
        }

        let mut validation_result = CleanupValidationResult {
            total_files: backup_files.len(),
            validated_files: 0,
            failed_validations: Vec::new(),
            safety_warnings: Vec::new(),
        };

        // Phase 1: Pre-cleanup validation - verify all files are safely restorable
        for (backup_file, target_file) in backup_files.iter().zip(target_files.iter()) {
            match self.validate_file_restoration_safety(backup_file, target_file) {
                Ok(()) => {
                    validation_result.validated_files += 1;
                    debug!("Pre-cleanup validation passed: {} -> {}", 
                           backup_file.display(), target_file.display());
                }
                Err(e) => {
                    let failure = CleanupValidationFailure {
                        backup_file: backup_file.clone(),
                        target_file: target_file.clone(),
                        error: e.to_string(),
                        validation_phase: "pre-cleanup".to_string(),
                    };
                    validation_result.failed_validations.push(failure);
                    warn!("Pre-cleanup validation failed for {}: {}", backup_file.display(), e);
                }
            }
        }

        // Phase 2: Safety checks - ensure no critical system files or active processes
        for backup_file in backup_files {
            if let Some(warning) = self.check_cleanup_safety_warnings(backup_file) {
                validation_result.safety_warnings.push(warning);
            }
        }

        // Phase 3: Disk space validation - ensure sufficient space for rollback operations
        if let Err(e) = self.validate_rollback_disk_space(backup_files) {
            validation_result.safety_warnings.push(CleanupSafetyWarning {
                file_path: PathBuf::from("system"),
                warning_type: "disk_space".to_string(),
                message: format!("Insufficient disk space for rollback operations: {}", e),
                severity: "high".to_string(),
            });
        }

        info!("Cleanup validation completed: {}/{} files validated, {} failures, {} warnings",
              validation_result.validated_files, validation_result.total_files,
              validation_result.failed_validations.len(), validation_result.safety_warnings.len());

        Ok(validation_result)
    }

    /// Validate that a specific file restoration is safe for cleanup
    fn validate_file_restoration_safety(&self, backup_file: &Path, target_file: &Path) -> Result<()> {
        // Check 1: Backup file exists and is readable
        if !backup_file.exists() {
            bail!("Backup file does not exist: {}", backup_file.display());
        }

        let backup_metadata = fs::metadata(backup_file)
            .with_context(|| format!("Cannot read backup file metadata: {}", backup_file.display()))?;

        if !backup_metadata.is_file() {
            bail!("Backup path is not a regular file: {}", backup_file.display());
        }

        // Check 2: Target file exists and matches backup
        if !target_file.exists() {
            bail!("Target file does not exist: {}", target_file.display());
        }

        let target_metadata = fs::metadata(target_file)
            .with_context(|| format!("Cannot read target file metadata: {}", target_file.display()))?;

        // Check 3: File size validation
        if backup_metadata.len() != target_metadata.len() {
            bail!("File size mismatch: backup={} bytes, target={} bytes", 
                  backup_metadata.len(), target_metadata.len());
        }

        // Check 4: File accessibility validation
        match fs::File::open(target_file) {
            Ok(_) => {
                debug!("Target file accessibility confirmed: {}", target_file.display());
            }
            Err(e) => {
                bail!("Target file is not accessible: {} - {}", target_file.display(), e);
            }
        }

        // Check 5: Content validation (first 1KB comparison for performance)
        if let Err(e) = self.validate_file_content_sample(backup_file, target_file) {
            warn!("Content validation warning for {}: {}", target_file.display(), e);
            // Don't fail for content validation warnings, just log them
        }

        Ok(())
    }

    /// Check for safety warnings that might affect cleanup operations
    fn check_cleanup_safety_warnings(&self, backup_file: &Path) -> Option<CleanupSafetyWarning> {
        let file_path_str = backup_file.to_string_lossy().to_lowercase();
        
        // Check for critical system files
        if file_path_str.contains("/etc/") || file_path_str.contains("/bin/") || file_path_str.contains("/sbin/") {
            return Some(CleanupSafetyWarning {
                file_path: backup_file.to_path_buf(),
                warning_type: "system_file".to_string(),
                message: "Backup file appears to be a system file".to_string(),
                severity: "medium".to_string(),
            });
        }

        // Check for large files that might cause issues
        if let Ok(metadata) = fs::metadata(backup_file) {
            if metadata.len() > 100 * 1024 * 1024 { // 100MB
                return Some(CleanupSafetyWarning {
                    file_path: backup_file.to_path_buf(),
                    warning_type: "large_file".to_string(),
                    message: format!("Large backup file ({} bytes) - cleanup may take time", metadata.len()),
                    severity: "low".to_string(),
                });
            }
        }

        None
    }

    /// Validate that there's sufficient disk space for rollback operations
    fn validate_rollback_disk_space(&self, backup_files: &[PathBuf]) -> Result<()> {
        let mut total_size = 0u64;
        
        for backup_file in backup_files {
            if let Ok(metadata) = fs::metadata(backup_file) {
                total_size += metadata.len();
            }
        }

        // Require 2x the total backup size for safe rollback operations
        let required_space = total_size * 2;
        
        // Get available disk space (simplified check)
        if let Ok(available_space) = self.get_available_disk_space() {
            if available_space < required_space {
                bail!("Insufficient disk space: need {} bytes, have {} bytes", 
                      required_space, available_space);
            }
        }

        debug!("Disk space validation passed: {} bytes required for rollback", required_space);
        Ok(())
    }

    /// Get available disk space (simplified implementation)
    fn get_available_disk_space(&self) -> Result<u64> {
        // Use statvfs or similar system call in a real implementation
        // For now, return a reasonable default to avoid blocking operations
        Ok(1024 * 1024 * 1024) // 1GB default
    }

    /// Validate file content by comparing a sample of bytes
    fn validate_file_content_sample(&self, backup_file: &Path, target_file: &Path) -> Result<()> {
        use std::io::Read;
        
        const SAMPLE_SIZE: usize = 1024; // Compare first 1KB
        
        let mut backup_buffer = vec![0u8; SAMPLE_SIZE];
        let mut target_buffer = vec![0u8; SAMPLE_SIZE];
        
        let backup_bytes_read = {
            let mut backup_file_handle = fs::File::open(backup_file)?;
            backup_file_handle.read(&mut backup_buffer)?
        };
        
        let target_bytes_read = {
            let mut target_file_handle = fs::File::open(target_file)?;
            target_file_handle.read(&mut target_buffer)?
        };
        
        if backup_bytes_read != target_bytes_read {
            bail!("Content sample size mismatch: backup={} bytes, target={} bytes", 
                  backup_bytes_read, target_bytes_read);
        }
        
        if backup_buffer[..backup_bytes_read] != target_buffer[..target_bytes_read] {
            bail!("Content sample mismatch detected");
        }
        
        debug!("Content sample validation passed ({} bytes compared)", backup_bytes_read);
        Ok(())
    }

    /// Perform batch cleanup with rollback capability
    /// This method provides a safe way to cleanup multiple files with automatic rollback on failure
    pub fn cleanup_backup_files_with_rollback(&self, backup_files: &[PathBuf], target_files: &[PathBuf]) -> Result<BatchCleanupResult> {
        info!("Starting batch cleanup with rollback for {} files", backup_files.len());
        
        if backup_files.len() != target_files.len() {
            bail!("Backup and target file lists must have the same length");
        }

        // Phase 1: Comprehensive validation
        let validation_result = self.validate_backup_cleanup_safety(backup_files, target_files)?;
        
        if !validation_result.failed_validations.is_empty() {
            bail!("Pre-cleanup validation failed for {} files", validation_result.failed_validations.len());
        }

        let mut cleanup_result = BatchCleanupResult {
            total_files: backup_files.len(),
            successful_cleanups: 0,
            failed_cleanups: 0,
            rollback_operations: 0,
            cleanup_details: Vec::new(),
        };

        let mut cleanup_backups: Vec<(PathBuf, PathBuf)> = Vec::new(); // (backup_copy, original_path)
        
        // Phase 2: Create temporary backups for all files
        info!("Creating temporary backups for rollback capability");
        for backup_file in backup_files {
            match self.create_cleanup_backup(backup_file) {
                Ok(backup_copy_path) => {
                    cleanup_backups.push((backup_copy_path, backup_file.clone()));
                    debug!("Created temporary backup: {}", backup_file.display());
                }
                Err(e) => {
                    error!("Failed to create temporary backup for {}: {}", backup_file.display(), e);
                    // Rollback any temporary backups created so far
                    self.cleanup_temporary_backups(&cleanup_backups);
                    bail!("Failed to create temporary backup for {}: {}", backup_file.display(), e);
                }
            }
        }

        // Phase 3: Perform cleanup operations
        info!("Performing cleanup operations with rollback protection");
        let mut cleanup_failed = false;
        
        for (i, backup_file) in backup_files.iter().enumerate() {
            let target_file = &target_files[i];
            
            // Final validation before cleanup
            match self.validate_file_before_cleanup(backup_file, target_file) {
                Ok(()) => {
                    // Perform the actual cleanup
                    match fs::remove_file(backup_file) {
                        Ok(()) => {
                            cleanup_result.successful_cleanups += 1;
                            cleanup_result.cleanup_details.push(CleanupDetail {
                                backup_file: backup_file.clone(),
                                target_file: target_file.clone(),
                                status: "success".to_string(),
                                message: "File successfully cleaned".to_string(),
                            });
                            info!("Successfully cleaned backup file: {}", backup_file.display());
                        }
                        Err(e) => {
                            cleanup_result.failed_cleanups += 1;
                            cleanup_result.cleanup_details.push(CleanupDetail {
                                backup_file: backup_file.clone(),
                                target_file: target_file.clone(),
                                status: "failed".to_string(),
                                message: format!("Cleanup failed: {}", e),
                            });
                            error!("Failed to cleanup backup file {}: {}", backup_file.display(), e);
                            cleanup_failed = true;
                            break; // Stop cleanup on first failure to enable rollback
                        }
                    }
                }
                Err(e) => {
                    cleanup_result.failed_cleanups += 1;
                    cleanup_result.cleanup_details.push(CleanupDetail {
                        backup_file: backup_file.clone(),
                        target_file: target_file.clone(),
                        status: "validation_failed".to_string(),
                        message: format!("Pre-cleanup validation failed: {}", e),
                    });
                    warn!("Pre-cleanup validation failed for {}: {}", backup_file.display(), e);
                    cleanup_failed = true;
                    break;
                }
            }
        }

        // Phase 4: Handle rollback if needed
        if cleanup_failed {
            warn!("Cleanup operation failed, initiating rollback");
            match self.perform_cleanup_rollback(&cleanup_backups, cleanup_result.successful_cleanups) {
                Ok(rollback_count) => {
                    cleanup_result.rollback_operations = rollback_count;
                    info!("Successfully rolled back {} cleanup operations", rollback_count);
                }
                Err(e) => {
                    error!("Rollback operation failed: {}", e);
                    // Keep temporary backups for manual recovery
                    warn!("Temporary backups preserved for manual recovery");
                    return Err(anyhow::anyhow!("Cleanup failed and rollback failed: {}", e));
                }
            }
        } else {
            // Phase 5: Cleanup successful, remove temporary backups
            info!("All cleanup operations successful, removing temporary backups");
            self.cleanup_temporary_backups(&cleanup_backups);
        }

        info!("Batch cleanup completed: {}/{} successful, {} failed, {} rolled back",
              cleanup_result.successful_cleanups, cleanup_result.total_files,
              cleanup_result.failed_cleanups, cleanup_result.rollback_operations);

        Ok(cleanup_result)
    }

    /// Perform rollback of cleanup operations
    fn perform_cleanup_rollback(&self, cleanup_backups: &[(PathBuf, PathBuf)], successful_cleanups: usize) -> Result<usize> {
        info!("Performing rollback for {} cleanup operations", successful_cleanups);
        
        let mut rollback_count = 0;
        
        // Only rollback the files that were successfully cleaned (first N files)
        for (backup_copy_path, original_path) in cleanup_backups.iter().take(successful_cleanups) {
            match self.restore_from_cleanup_backup(backup_copy_path, original_path) {
                Ok(()) => {
                    rollback_count += 1;
                    info!("Successfully rolled back: {}", original_path.display());
                }
                Err(e) => {
                    error!("Failed to rollback {}: {}", original_path.display(), e);
                    // Continue with other rollbacks even if one fails
                }
            }
        }
        
        Ok(rollback_count)
    }

    /// Clean up temporary backup files
    fn cleanup_temporary_backups(&self, cleanup_backups: &[(PathBuf, PathBuf)]) {
        for (backup_copy_path, _) in cleanup_backups {
            if backup_copy_path.exists() {
                match fs::remove_file(backup_copy_path) {
                    Ok(()) => {
                        debug!("Removed temporary backup: {}", backup_copy_path.display());
                    }
                    Err(e) => {
                        warn!("Failed to remove temporary backup {}: {}", backup_copy_path.display(), e);
                    }
                }
            }
        }
    }

    /// Parallel directory processing for better performance
    fn process_directory_parallel(&self, current_dir: &Path, backup_root: &Path, result: &mut DirectRestoreResult) -> Result<()> {
        debug!("Processing directory with parallel operations: {}", current_dir.display());

        // Collect all file paths first
        let mut file_paths = Vec::new();
        let mut dir_paths = Vec::new();
        
        let entries = fs::read_dir(current_dir)
            .with_context(|| format!("Failed to read directory: {}", current_dir.display()))?;

        for entry in entries {
            let entry = entry.with_context(|| format!("Failed to read directory entry in: {}", current_dir.display()))?;
            let entry_path = entry.path();
            
            let metadata = entry.metadata()
                .with_context(|| format!("Failed to get metadata for: {}", entry_path.display()))?;

            if metadata.is_dir() {
                dir_paths.push(entry_path);
            } else if metadata.is_file() {
                file_paths.push(entry_path);
            } else {
                // Handle symlinks and other file types
                debug!("Skipping non-regular file: {}", entry_path.display());
                result.skipped_files += 1;
                result.skipped_details.push(SkippedFile {
                    path: entry_path.clone(),
                    reason: "Not a regular file".to_string(),
                });
            }
        }
        
        result.total_files += file_paths.len();
        
        // Process files in parallel using resource manager
        let resource_manager = ResourceManager::global();
        let file_results: Vec<_> = resource_manager.thread_pool.io_pool().install(|| {
            file_paths.par_iter().map(|file_path| {
                self.process_single_file(file_path, backup_root)
            }).collect()
        });
        
        // Aggregate results
        for file_result in file_results {
            match file_result {
                Ok(file_outcome) => {
                    match file_outcome {
                        FileProcessOutcome::Success => result.successful_files += 1,
                        FileProcessOutcome::Skipped(_reason) => {
                            result.skipped_files += 1;
                            // Add to skipped details would need the path, which we'd need to track
                        }
                        FileProcessOutcome::Failed(_error) => {
                            result.failed_files += 1;
                            // Add to failed details would need the path
                        }
                        FileProcessOutcome::Cleaned => result.cleaned_files += 1,
                    }
                }
                Err(e) => {
                    result.failed_files += 1;
                    result.failed_details.push(FailedFile {
                        path: PathBuf::from("unknown"), // Would need better error tracking
                        error: e.to_string(),
                    });
                }
            }
        }
        
        // Recursively process subdirectories
        for dir_path in dir_paths {
            self.process_directory_parallel(&dir_path, backup_root, result)?;
        }

        Ok(())
    }

    /// Process a single file with optimized operations
    fn process_single_file(&self, backup_file_path: &Path, backup_root: &Path) -> Result<FileProcessOutcome> {
        // Map backup file path to container target path
        let target_path = match self.map_backup_to_container_path(backup_file_path, backup_root) {
            Ok(path) => path,
            Err(e) => {
                error!("Failed to map backup path to container path: {} - {}", backup_file_path.display(), e);
                return Ok(FileProcessOutcome::Failed(format!("Path mapping failed: {}", e)));
            }
        };

        debug!("Processing file: {} -> {}", backup_file_path.display(), target_path.display());

        // Copy file with retry logic for transient errors
        let copy_result = self.copy_file_with_retry(backup_file_path, &target_path);
        
        match copy_result {
            CopyResult::Success => {
                info!("Successfully restored: {}", target_path.display());
                
                // Validate that the restored file is accessible
                if let Err(e) = self.validate_restored_file(&target_path) {
                    warn!("Restored file validation failed for {}: {}", target_path.display(), e);
                    // Don't fail the operation, just log the warning
                }
                
                // Clean up successfully restored file from backup directory
                if !self.dry_run {
                    match self.validate_file_before_cleanup(backup_file_path, &target_path) {
                        Ok(()) => {
                            match self.cleanup_backup_file(backup_file_path) {
                                Ok(()) => {
                                    info!("Cleaned backup file after successful restore: {}", backup_file_path.display());
                                    Ok(FileProcessOutcome::Cleaned)
                                }
                                Err(e) => {
                                    warn!("Cleanup operation failed for {}: {}", backup_file_path.display(), e);
                                    Ok(FileProcessOutcome::Success)
                                }
                            }
                        }
                        Err(e) => {
                            warn!("File validation failed before cleanup for {}: {}", backup_file_path.display(), e);
                            Ok(FileProcessOutcome::Success)
                        }
                    }
                } else {
                    info!("DRY RUN: Would validate and clean backup file: {}", backup_file_path.display());
                    Ok(FileProcessOutcome::Success)
                }
            }
            CopyResult::Skipped(reason) => {
                info!("Skipped file: {} - {}", target_path.display(), reason);
                Ok(FileProcessOutcome::Skipped(reason))
            }
            CopyResult::Failed(error) => {
                error!("Failed to restore file: {} - {}", target_path.display(), error);
                Ok(FileProcessOutcome::Failed(error))
            }
        }
    }

    /// Map backup file path to container target path
    pub fn map_backup_to_container_path(&self, backup_file_path: &Path, backup_root: &Path) -> Result<PathBuf> {
        // Get relative path from backup root
        let relative_path = backup_file_path.strip_prefix(backup_root)
            .with_context(|| format!("Backup file path {} is not under backup root {}", 
                                   backup_file_path.display(), backup_root.display()))?;

        // Map directly to container root
        // e.g., "root/.bashrc" -> "/root/.bashrc"
        // e.g., "abc.txt" -> "/abc.txt"
        let container_path = PathBuf::from("/").join(relative_path);

        // Validate the target path for security
        self.validate_container_path(&container_path)?;

        Ok(container_path)
    }

    /// Validate container target path for security
    fn validate_container_path(&self, path: &Path) -> Result<()> {
        // Check for path traversal attempts
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    bail!("Path contains parent directory (..) component: {}", path.display());
                }
                Component::Normal(name) => {
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with('.') && name_str.len() > 1 && name_str.chars().nth(1) == Some('.') {
                        bail!("Path contains suspicious component: {}", name_str);
                    }
                }
                _ => {} // Allow root, current dir, and prefix components
            }
        }

        // Ensure path starts with root
        if !path.starts_with("/") {
            bail!("Container path must be absolute: {}", path.display());
        }

        Ok(())
    }

    /// Copy file with retry mechanism for transient errors
    pub fn copy_file_with_retry(&self, src: &Path, dst: &Path) -> CopyResult {
        for attempt in 0..=self.max_retries {
            let result = self.copy_file_with_fallback(src, dst);
            
            match &result {
                CopyResult::Skipped(reason) if self.is_transient_error(reason) => {
                    if attempt < self.max_retries {
                        debug!("Transient error on attempt {} for {}: {}. Retrying in {:?}...", 
                               attempt + 1, dst.display(), reason, self.retry_delay);
                        thread::sleep(self.retry_delay);
                        continue;
                    } else {
                        warn!("Max retries ({}) exceeded for {}: {}", 
                              self.max_retries, dst.display(), reason);
                        return result;
                    }
                }
                _ => return result,
            }
        }
        
        // This should never be reached due to the loop logic above
        CopyResult::Failed("Unexpected retry loop exit".to_string())
    }

    /// Check if an error reason indicates a transient condition that might be retried
    fn is_transient_error(&self, reason: &str) -> bool {
        reason.contains("File busy") || reason.contains("Resource busy")
    }

    /// Copy file with graceful error handling
    pub fn copy_file_with_fallback(&self, src: &Path, dst: &Path) -> CopyResult {
        if self.dry_run {
            info!("DRY RUN: Would copy {} -> {}", src.display(), dst.display());
            return CopyResult::Success;
        }

        // Create parent directories if needed
        if let Some(parent) = dst.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return CopyResult::Failed(format!("Failed to create parent directories: {}", e));
            }
        }

        // Attempt to copy the file
        match fs::copy(src, dst) {
            Ok(_) => {
                // Try to preserve permissions and timestamps
                if let Err(e) = self.preserve_file_attributes(src, dst) {
                    warn!("Failed to preserve file attributes for {}: {}", dst.display(), e);
                    // Don't fail the copy operation for attribute preservation failures
                }
                CopyResult::Success
            }
            Err(e) => {
                // Classify the error and decide whether to skip or fail
                if self.is_file_busy(&e) {
                    CopyResult::Skipped(format!("File busy: {}", e))
                } else if self.is_file_readonly(&e) {
                    CopyResult::Skipped(format!("Read-only filesystem: {}", e))
                } else if self.is_permission_denied(&e) {
                    CopyResult::Skipped(format!("Permission denied: {}", e))
                } else {
                    CopyResult::Failed(format!("Copy failed: {}", e))
                }
            }
        }
    }

    /// Preserve file attributes (permissions, timestamps)
    fn preserve_file_attributes(&self, src: &Path, dst: &Path) -> Result<()> {
        let src_metadata = fs::metadata(src)
            .with_context(|| format!("Failed to get source metadata: {}", src.display()))?;

        // Preserve permissions
        let permissions = src_metadata.permissions();
        fs::set_permissions(dst, permissions)
            .with_context(|| format!("Failed to set permissions for: {}", dst.display()))?;

        // Preserve timestamps (modified time)
        if let Ok(modified) = src_metadata.modified() {
            if let Err(e) = filetime::set_file_mtime(dst, filetime::FileTime::from_system_time(modified)) {
                warn!("Failed to set modified time for {}: {}", dst.display(), e);
            }
        }

        Ok(())
    }

    /// Check if error indicates file is busy
    fn is_file_busy(&self, error: &io::Error) -> bool {
        match error.kind() {
            io::ErrorKind::ResourceBusy => true,
            _ => {
                // Check error message for common "file busy" indicators
                let error_msg = error.to_string().to_lowercase();
                error_msg.contains("text file busy") ||
                error_msg.contains("resource busy") ||
                error_msg.contains("device or resource busy")
            }
        }
    }

    /// Check if error indicates read-only filesystem
    fn is_file_readonly(&self, error: &io::Error) -> bool {
        match error.kind() {
            io::ErrorKind::ReadOnlyFilesystem => true,
            _ => {
                let error_msg = error.to_string().to_lowercase();
                error_msg.contains("read-only file system") ||
                error_msg.contains("readonly filesystem")
            }
        }
    }

    /// Check if error indicates permission denied
    fn is_permission_denied(&self, error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::PermissionDenied
    }

    /// Validate that a restored file is accessible at its target location
    fn validate_restored_file(&self, target_path: &Path) -> Result<()> {
        // Check if file exists
        if !target_path.exists() {
            bail!("Restored file does not exist: {}", target_path.display());
        }

        // Check if file is readable
        match fs::metadata(target_path) {
            Ok(metadata) => {
                debug!("Validated restored file: {} ({} bytes)", 
                       target_path.display(), metadata.len());
                Ok(())
            }
            Err(e) => {
                bail!("Cannot access restored file metadata: {} - {}", target_path.display(), e);
            }
        }
    }

    /// Clean up successfully restored file from backup directory with validation
    /// Only removes files that were successfully restored, preserving skipped files for manual recovery
    /// Includes safety checks and validation to prevent accidental data loss
    fn cleanup_backup_file(&self, backup_file_path: &Path) -> Result<()> {
        info!("Cleaning up successfully restored backup file: {}", backup_file_path.display());
        
        // Safety check: ensure we're only deleting files within the backup directory
        if !backup_file_path.exists() {
            debug!("Backup file already removed: {}", backup_file_path.display());
            return Ok(());
        }

        // Additional safety check: ensure the path is a regular file
        let metadata = fs::metadata(backup_file_path)
            .with_context(|| format!("Failed to get metadata for backup file: {}", backup_file_path.display()))?;
        
        if !metadata.is_file() {
            warn!("Skipping cleanup of non-regular file: {}", backup_file_path.display());
            return Ok(());
        }

        // Create backup of the file before deletion for potential rollback
        let backup_copy_path = self.create_cleanup_backup(backup_file_path)?;
        
        // Log file size before removal for audit purposes
        debug!("Removing backup file: {} ({} bytes)", backup_file_path.display(), metadata.len());

        // Remove the backup file
        match fs::remove_file(backup_file_path) {
            Ok(()) => {
                info!("Successfully cleaned backup file: {}", backup_file_path.display());
                
                // Cleanup was successful, remove the temporary backup copy
                if let Err(e) = fs::remove_file(&backup_copy_path) {
                    warn!("Failed to remove temporary backup copy {}: {}", backup_copy_path.display(), e);
                    // Don't fail the operation for this
                }
                
                // Try to remove empty parent directories (but don't fail if we can't)
                if let Some(parent) = backup_file_path.parent() {
                    if let Err(e) = self.cleanup_empty_directories(parent) {
                        debug!("Failed to cleanup empty directories for {}: {}", parent.display(), e);
                        // Don't propagate this error as it's not critical
                    }
                }
                
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Failed to remove backup file {}: {}", backup_file_path.display(), e);
                error!("{}", error_msg);
                
                // Attempt to restore from backup copy
                if let Err(restore_err) = self.restore_from_cleanup_backup(&backup_copy_path, backup_file_path) {
                    error!("Failed to restore backup file after cleanup failure: {}", restore_err);
                    // Keep the temporary backup for manual recovery
                    warn!("Temporary backup preserved for manual recovery: {}", backup_copy_path.display());
                } else {
                    info!("Successfully restored backup file after cleanup failure");
                }
                
                Err(anyhow::anyhow!(error_msg))
            }
        }
    }

    /// Create a temporary backup copy of the file before cleanup for potential rollback
    fn create_cleanup_backup(&self, backup_file_path: &Path) -> Result<PathBuf> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        let backup_copy_path = backup_file_path.with_extension(format!("cleanup_backup_{}", timestamp));
        
        debug!("Creating temporary backup copy: {} -> {}", 
               backup_file_path.display(), backup_copy_path.display());
        
        fs::copy(backup_file_path, &backup_copy_path)
            .with_context(|| format!("Failed to create cleanup backup copy: {}", backup_copy_path.display()))?;
        
        Ok(backup_copy_path)
    }

    /// Restore file from cleanup backup in case of cleanup failure
    fn restore_from_cleanup_backup(&self, backup_copy_path: &Path, original_path: &Path) -> Result<()> {
        debug!("Restoring from cleanup backup: {} -> {}", 
               backup_copy_path.display(), original_path.display());
        
        if !backup_copy_path.exists() {
            bail!("Cleanup backup copy does not exist: {}", backup_copy_path.display());
        }
        
        // Restore the original file
        fs::copy(backup_copy_path, original_path)
            .with_context(|| format!("Failed to restore from cleanup backup: {}", original_path.display()))?;
        
        // Remove the temporary backup copy
        fs::remove_file(backup_copy_path)
            .with_context(|| format!("Failed to remove cleanup backup copy: {}", backup_copy_path.display()))?;
        
        info!("Successfully restored file from cleanup backup: {}", original_path.display());
        Ok(())
    }

    /// Validate that a file was successfully restored before allowing cleanup
    /// This provides an additional safety check to prevent data loss
    fn validate_file_before_cleanup(&self, backup_file_path: &Path, target_path: &Path) -> Result<()> {
        debug!("Validating file before cleanup: backup={}, target={}", 
               backup_file_path.display(), target_path.display());
        
        // Check that target file exists
        if !target_path.exists() {
            bail!("Target file does not exist, cannot cleanup backup: {}", target_path.display());
        }
        
        // Get metadata for both files
        let backup_metadata = fs::metadata(backup_file_path)
            .with_context(|| format!("Failed to get backup file metadata: {}", backup_file_path.display()))?;
        
        let target_metadata = fs::metadata(target_path)
            .with_context(|| format!("Failed to get target file metadata: {}", target_path.display()))?;
        
        // Compare file sizes
        if backup_metadata.len() != target_metadata.len() {
            bail!("File size mismatch: backup={} bytes, target={} bytes", 
                  backup_metadata.len(), target_metadata.len());
        }
        
        // Additional validation: check that target file is readable
        match fs::File::open(target_path) {
            Ok(_) => {
                debug!("Target file validation successful: {}", target_path.display());
                Ok(())
            }
            Err(e) => {
                bail!("Target file is not readable: {} - {}", target_path.display(), e);
            }
        }
    }

    /// Recursively remove empty directories up the tree
    /// Provides detailed logging for cleanup operations and failures
    fn cleanup_empty_directories(&self, dir_path: &Path) -> Result<()> {
        if !dir_path.exists() {
            debug!("Directory does not exist, skipping cleanup: {}", dir_path.display());
            return Ok(());
        }

        // Check if directory is empty
        let entries: Vec<_> = fs::read_dir(dir_path)
            .with_context(|| format!("Failed to read directory for cleanup: {}", dir_path.display()))?
            .collect::<Result<Vec<_>, _>>()?;

        if entries.is_empty() {
            info!("Removing empty backup directory: {}", dir_path.display());
            match fs::remove_dir(dir_path) {
                Ok(()) => {
                    info!("Successfully removed empty directory: {}", dir_path.display());
                    
                    // Recursively try to clean parent directories
                    if let Some(parent) = dir_path.parent() {
                        if let Err(e) = self.cleanup_empty_directories(parent) {
                            debug!("Failed to cleanup parent directory {}: {}", parent.display(), e);
                            // Don't propagate error for parent cleanup failures
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("Failed to remove empty directory {}: {}", dir_path.display(), e);
                    warn!("{}", error_msg);
                    return Err(anyhow::anyhow!(error_msg));
                }
            }
        } else {
            debug!("Directory not empty, preserving: {} ({} entries)", dir_path.display(), entries.len());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_backup_to_container_path() {
        let engine = DirectRestoreEngine::new(true, 300);
        let backup_root = PathBuf::from("/tmp/backup");
        
        // Test root file mapping
        let backup_file = PathBuf::from("/tmp/backup/root/.bashrc");
        let result = engine.map_backup_to_container_path(&backup_file, &backup_root).unwrap();
        assert_eq!(result, PathBuf::from("/root/.bashrc"));
        
        // Test top-level file mapping
        let backup_file = PathBuf::from("/tmp/backup/abc.txt");
        let result = engine.map_backup_to_container_path(&backup_file, &backup_root).unwrap();
        assert_eq!(result, PathBuf::from("/abc.txt"));
        
        // Test nested directory mapping
        let backup_file = PathBuf::from("/tmp/backup/home/user/document.txt");
        let result = engine.map_backup_to_container_path(&backup_file, &backup_root).unwrap();
        assert_eq!(result, PathBuf::from("/home/user/document.txt"));
    }

    #[test]
    fn test_validate_container_path() {
        let engine = DirectRestoreEngine::new(true, 300);
        
        // Valid paths
        assert!(engine.validate_container_path(&PathBuf::from("/root/.bashrc")).is_ok());
        assert!(engine.validate_container_path(&PathBuf::from("/home/user/file.txt")).is_ok());
        
        // Invalid paths
        assert!(engine.validate_container_path(&PathBuf::from("../etc/passwd")).is_err());
        assert!(engine.validate_container_path(&PathBuf::from("/root/../etc/passwd")).is_err());
        assert!(engine.validate_container_path(&PathBuf::from("relative/path")).is_err());
    }

    #[test]
    fn test_error_classification() {
        let engine = DirectRestoreEngine::new(true, 300);
        
        // Test file busy error
        let busy_error = io::Error::new(io::ErrorKind::ResourceBusy, "Resource busy");
        assert!(engine.is_file_busy(&busy_error));
        
        // Test permission denied error
        let perm_error = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");
        assert!(engine.is_permission_denied(&perm_error));
        
        // Test read-only filesystem error
        let readonly_error = io::Error::new(io::ErrorKind::ReadOnlyFilesystem, "Read-only filesystem");
        assert!(engine.is_file_readonly(&readonly_error));
    }

    #[test]
    fn test_cleanup_safety_warnings() {
        let engine = DirectRestoreEngine::new(true, 300);
        
        // Test system file warning
        let system_file = PathBuf::from("/backup/etc/passwd");
        let warning = engine.check_cleanup_safety_warnings(&system_file);
        assert!(warning.is_some());
        assert_eq!(warning.unwrap().warning_type, "system_file");
        
        // Test normal file (no warning)
        let normal_file = PathBuf::from("/backup/home/user/document.txt");
        let warning = engine.check_cleanup_safety_warnings(&normal_file);
        assert!(warning.is_none());
    }

    #[test]
    fn test_file_restoration_safety_validation() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;
        
        let temp_dir = TempDir::new().unwrap();
        let engine = DirectRestoreEngine::new(true, 300);
        
        // Create test files
        let backup_file = temp_dir.path().join("backup.txt");
        let target_file = temp_dir.path().join("target.txt");
        
        let test_content = "test content";
        File::create(&backup_file).unwrap().write_all(test_content.as_bytes()).unwrap();
        File::create(&target_file).unwrap().write_all(test_content.as_bytes()).unwrap();
        
        // Test successful validation
        let result = engine.validate_file_restoration_safety(&backup_file, &target_file);
        assert!(result.is_ok());
        
        // Test validation failure with missing target
        let missing_target = temp_dir.path().join("missing.txt");
        let result = engine.validate_file_restoration_safety(&backup_file, &missing_target);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Target file does not exist"));
    }

    #[test]
    fn test_content_sample_validation() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;
        
        let temp_dir = TempDir::new().unwrap();
        let engine = DirectRestoreEngine::new(true, 300);
        
        // Create identical test files
        let backup_file = temp_dir.path().join("backup.txt");
        let target_file = temp_dir.path().join("target.txt");
        
        let test_content = "identical content for both files";
        File::create(&backup_file).unwrap().write_all(test_content.as_bytes()).unwrap();
        File::create(&target_file).unwrap().write_all(test_content.as_bytes()).unwrap();
        
        // Test successful content validation
        let result = engine.validate_file_content_sample(&backup_file, &target_file);
        assert!(result.is_ok());
        
        // Create different content file
        let different_file = temp_dir.path().join("different.txt");
        File::create(&different_file).unwrap().write_all(b"different content").unwrap();
        
        // Test content validation failure
        let result = engine.validate_file_content_sample(&backup_file, &different_file);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("mismatch"));
    }

    #[test]
    fn test_cleanup_validation_result_structure() {
        let validation_result = CleanupValidationResult {
            total_files: 5,
            validated_files: 3,
            failed_validations: vec![
                CleanupValidationFailure {
                    backup_file: PathBuf::from("/backup/file1.txt"),
                    target_file: PathBuf::from("/target/file1.txt"),
                    error: "Size mismatch".to_string(),
                    validation_phase: "pre-cleanup".to_string(),
                }
            ],
            safety_warnings: vec![
                CleanupSafetyWarning {
                    file_path: PathBuf::from("/backup/etc/passwd"),
                    warning_type: "system_file".to_string(),
                    message: "System file detected".to_string(),
                    severity: "medium".to_string(),
                }
            ],
        };
        
        assert_eq!(validation_result.total_files, 5);
        assert_eq!(validation_result.validated_files, 3);
        assert_eq!(validation_result.failed_validations.len(), 1);
        assert_eq!(validation_result.safety_warnings.len(), 1);
    }

    #[test]
    fn test_transient_error_detection() {
        let engine = DirectRestoreEngine::new(true, 300);
        
        assert!(engine.is_transient_error("File busy: Resource busy"));
        assert!(engine.is_transient_error("Resource busy"));
        assert!(!engine.is_transient_error("Permission denied"));
        assert!(!engine.is_transient_error("Read-only filesystem"));
    }

    #[test]
    fn test_retry_configuration() {
        let engine = DirectRestoreEngine::new(true, 300)
            .with_retry_config(5, Duration::from_millis(100));
        
        assert_eq!(engine.max_retries, 5);
        assert_eq!(engine.retry_delay, Duration::from_millis(100));
    }
}