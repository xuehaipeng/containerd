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

#[derive(Debug)]
pub struct DirectRestoreEngineEnhanced {
    pub dry_run: bool,
    pub timeout: u64,
    pub max_retries: u32,
    pub retry_delay: Duration,
}

impl DirectRestoreEngineEnhanced {
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

    /// Restore files directly to container root filesystem with move-first optimization
    pub fn restore_to_container_root(&self, backup_path: &Path) -> Result<DirectRestoreResult> {
        let start_time = SystemTime::now();
        
        info!("Starting enhanced direct container root restoration with move optimization from: {}", backup_path.display());
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

        // Use enhanced directory processing with move optimization
        self.process_directory_with_move_optimization(backup_path, backup_path, &mut result)?;

        result.duration = start_time.elapsed().unwrap_or(Duration::from_secs(0));
        
        info!("Enhanced direct restore completed:");
        info!("  Total files: {}", result.total_files);
        info!("  Successful: {}", result.successful_files);
        info!("  Skipped: {}", result.skipped_files);
        info!("  Failed: {}", result.failed_files);
        info!("  Cleaned from backup: {}", result.cleaned_files);
        info!("  Duration: {:?}", result.duration);

        Ok(result)
    }

    /// Enhanced directory processing with move-first optimization and symlink support
    fn process_directory_with_move_optimization(&self, current_dir: &Path, backup_root: &Path, result: &mut DirectRestoreResult) -> Result<()> {
        debug!("Processing directory with move optimization: {}", current_dir.display());

        // Try bulk directory move first for top-level directories
        if self.should_use_bulk_move(current_dir, backup_root) {
            if let Ok(moved_count) = self.try_bulk_directory_move(current_dir, backup_root) {
                result.total_files += moved_count;
                result.successful_files += moved_count;
                result.cleaned_files += moved_count; // Files are automatically cleaned by move
                info!("Bulk moved {} files from {}", moved_count, current_dir.display());
                return Ok(());
            }
        }

        // Fall back to individual file processing with move-first strategy
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
            } else if metadata.is_file() || metadata.file_type().is_symlink() {
                // Include both regular files and symlinks
                file_paths.push(entry_path);
            } else {
                // Handle other special file types
                debug!("Skipping special file type: {}", entry_path.display());
                result.skipped_files += 1;
                result.skipped_details.push(SkippedFile {
                    path: entry_path.clone(),
                    reason: "Special file type (not regular file or symlink)".to_string(),
                });
            }
        }
        
        result.total_files += file_paths.len();
        
        // Process files with move-first strategy
        let resource_manager = ResourceManager::global();
        let file_results: Vec<_> = resource_manager.thread_pool.io_pool().install(|| {
            file_paths.par_iter().map(|file_path| {
                self.process_single_file_with_move_first(file_path, backup_root)
            }).collect()
        });
        
        // Aggregate results
        for file_result in file_results {
            match file_result {
                Ok(file_outcome) => {
                    match file_outcome {
                        FileProcessOutcome::Success => result.successful_files += 1,
                        FileProcessOutcome::Skipped(reason) => {
                            result.skipped_files += 1;
                            // Note: We'd need better error tracking to include path details
                        }
                        FileProcessOutcome::Failed(error) => {
                            result.failed_files += 1;
                            // Note: We'd need better error tracking to include path details
                        }
                        FileProcessOutcome::Cleaned => {
                            result.successful_files += 1;
                            result.cleaned_files += 1;
                        }
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
            self.process_directory_with_move_optimization(&dir_path, backup_root, result)?;
        }

        Ok(())
    }

    /// Check if we should attempt bulk directory move
    fn should_use_bulk_move(&self, current_dir: &Path, backup_root: &Path) -> bool {
        if self.dry_run {
            return false; // Skip bulk moves in dry run
        }

        // Only use bulk move for direct children of backup root that are common directories
        if let Some(parent) = current_dir.parent() {
            if parent == backup_root {
                if let Some(dir_name) = current_dir.file_name() {
                    let dir_name_str = dir_name.to_string_lossy();
                    return matches!(dir_name_str.as_ref(), "usr" | "home" | "opt" | "var" | "etc" | "root");
                }
            }
        }
        
        false
    }

    /// Try to move an entire directory tree efficiently
    fn try_bulk_directory_move(&self, src_dir: &Path, backup_root: &Path) -> Result<usize> {
        let target_path = self.map_backup_to_container_path(src_dir, backup_root)?;
        
        // Count files for statistics
        let file_count = self.count_files_recursive(src_dir)?;
        
        // Create parent directories if needed
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directories for: {}", target_path.display()))?;
        }

        // Try atomic rename first (most efficient)
        match fs::rename(src_dir, &target_path) {
            Ok(()) => {
                info!("Atomic directory move successful: {} -> {} ({} files)", 
                      src_dir.display(), target_path.display(), file_count);
                return Ok(file_count);
            }
            Err(e) => {
                debug!("Atomic rename failed (likely cross-filesystem): {}", e);
            }
        }

        // If atomic rename fails, use recursive move
        let moved_count = self.move_directory_recursive(src_dir, &target_path)?;
        
        // Remove source directory after successful move
        if src_dir.exists() {
            fs::remove_dir_all(src_dir)
                .with_context(|| format!("Failed to remove source directory after move: {}", src_dir.display()))?;
        }
        
        Ok(moved_count)
    }

    /// Recursively move directory contents
    fn move_directory_recursive(&self, src_dir: &Path, dst_dir: &Path) -> Result<usize> {
        let mut moved_count = 0;
        
        // Create destination directory
        fs::create_dir_all(dst_dir)
            .with_context(|| format!("Failed to create destination directory: {}", dst_dir.display()))?;
        
        for entry in fs::read_dir(src_dir)? {
            let entry = entry?;
            let src_path = entry.path();
            let file_name = src_path.file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid file name: {}", src_path.display()))?;
            let dst_path = dst_dir.join(file_name);
            
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                moved_count += self.move_directory_recursive(&src_path, &dst_path)?;
            } else {
                // Move individual file (handles regular files and symlinks)
                match fs::rename(&src_path, &dst_path) {
                    Ok(()) => {
                        moved_count += 1;
                        debug!("Moved file: {} -> {}", src_path.display(), dst_path.display());
                    }
                    Err(_) => {
                        // Fall back to copy for cross-filesystem moves
                        if metadata.file_type().is_symlink() {
                            self.copy_symlink(&src_path, &dst_path)?;
                        } else {
                            fs::copy(&src_path, &dst_path)
                                .with_context(|| format!("Failed to copy file: {}", src_path.display()))?;
                            self.preserve_file_attributes(&src_path, &dst_path)?;
                        }
                        
                        fs::remove_file(&src_path)
                            .with_context(|| format!("Failed to remove source file: {}", src_path.display()))?;
                        
                        moved_count += 1;
                        debug!("Copy+delete file (cross-fs): {} -> {}", src_path.display(), dst_path.display());
                    }
                }
            }
        }
        
        Ok(moved_count)
    }

    /// Count files recursively in a directory
    fn count_files_recursive(&self, dir_path: &Path) -> Result<usize> {
        let mut count = 0;
        
        fn count_files_in_dir(dir: &Path, counter: &mut usize) -> Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                
                if path.is_dir() {
                    count_files_in_dir(&path, counter)?;
                } else {
                    *counter += 1;
                }
            }
            Ok(())
        }
        
        count_files_in_dir(dir_path, &mut count)?;
        Ok(count)
    }

    /// Process a single file with move-first strategy
    fn process_single_file_with_move_first(&self, backup_file_path: &Path, backup_root: &Path) -> Result<FileProcessOutcome> {
        // Map backup file path to container target path
        let target_path = match self.map_backup_to_container_path(backup_file_path, backup_root) {
            Ok(path) => path,
            Err(e) => {
                error!("Failed to map backup path to container path: {} - {}", backup_file_path.display(), e);
                return Ok(FileProcessOutcome::Failed(format!("Path mapping failed: {}", e)));
            }
        };

        debug!("Processing file with move-first: {} -> {}", backup_file_path.display(), target_path.display());

        // Try move first (most efficient)
        let move_result = self.move_file_with_retry(backup_file_path, &target_path);
        
        match move_result {
            CopyResult::Success => {
                info!("Successfully moved: {}", target_path.display());
                Ok(FileProcessOutcome::Cleaned) // File is automatically cleaned by move
            }
            CopyResult::Skipped(reason) => {
                info!("Skipped file: {} - {}", target_path.display(), reason);
                Ok(FileProcessOutcome::Skipped(reason))
            }
            CopyResult::Failed(error) => {
                debug!("Move failed, falling back to copy: {} - {}", target_path.display(), error);
                
                // Fall back to copy+delete
                let copy_result = self.copy_file_with_retry(backup_file_path, &target_path);
                match copy_result {
                    CopyResult::Success => {
                        info!("Successfully copied (fallback): {}", target_path.display());
                        
                        // Clean up backup file after successful copy
                        if !self.dry_run {
                            match fs::remove_file(backup_file_path) {
                                Ok(()) => Ok(FileProcessOutcome::Cleaned),
                                Err(e) => {
                                    warn!("Cleanup failed for {}: {}", backup_file_path.display(), e);
                                    Ok(FileProcessOutcome::Success)
                                }
                            }
                        } else {
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
        }
    }

    /// Move file with retry mechanism (most efficient)
    pub fn move_file_with_retry(&self, src: &Path, dst: &Path) -> CopyResult {
        for attempt in 0..=self.max_retries {
            let result = self.move_file_with_fallback(src, dst);
            
            match &result {
                CopyResult::Skipped(reason) if self.is_transient_error(reason) => {
                    if attempt < self.max_retries {
                        debug!("Transient error on move attempt {} for {}: {}. Retrying in {:?}...", 
                               attempt + 1, dst.display(), reason, self.retry_delay);
                        thread::sleep(self.retry_delay);
                        continue;
                    } else {
                        warn!("Max move retries ({}) exceeded for {}: {}", 
                              self.max_retries, dst.display(), reason);
                        return result;
                    }
                }
                _ => return result,
            }
        }
        
        CopyResult::Failed("Unexpected retry loop exit".to_string())
    }

    /// Move file with graceful error handling (atomic operation)
    pub fn move_file_with_fallback(&self, src: &Path, dst: &Path) -> CopyResult {
        if self.dry_run {
            info!("DRY RUN: Would move {} -> {}", src.display(), dst.display());
            return CopyResult::Success;
        }

        // Create parent directories if needed
        if let Some(parent) = dst.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return CopyResult::Failed(format!("Failed to create parent directories: {}", e));
            }
        }

        // Check if source is a symlink and handle accordingly
        match fs::symlink_metadata(src) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    // Handle symlinks specially
                    match self.move_symlink(src, dst) {
                        Ok(()) => CopyResult::Success,
                        Err(e) => CopyResult::Failed(format!("Failed to move symlink: {}", e)),
                    }
                } else {
                    // Regular file - try atomic move
                    match fs::rename(src, dst) {
                        Ok(()) => {
                            debug!("Atomic move successful: {} -> {}", src.display(), dst.display());
                            CopyResult::Success
                        }
                        Err(e) => {
                            // Classify the error
                            if self.is_file_busy(&e) {
                                CopyResult::Skipped(format!("File busy: {}", e))
                            } else if self.is_file_readonly(&e) {
                                CopyResult::Skipped(format!("Read-only filesystem: {}", e))
                            } else if self.is_permission_denied(&e) {
                                CopyResult::Skipped(format!("Permission denied: {}", e))
                            } else if e.kind() == io::ErrorKind::CrossesDevices {
                                // Cross-device move - will need copy+delete fallback
                                CopyResult::Failed(format!("Cross-device move (fallback needed): {}", e))
                            } else {
                                CopyResult::Failed(format!("Move failed: {}", e))
                            }
                        }
                    }
                }
            }
            Err(e) => CopyResult::Failed(format!("Failed to get file metadata: {}", e)),
        }
    }

    /// Move symlink preserving its target
    fn move_symlink(&self, src: &Path, dst: &Path) -> Result<()> {
        // Read the symlink target
        let link_target = fs::read_link(src)
            .with_context(|| format!("Failed to read symlink: {}", src.display()))?;
        
        // Create the new symlink
        self.copy_symlink(src, dst)?;
        
        // Remove the original symlink
        fs::remove_file(src)
            .with_context(|| format!("Failed to remove source symlink: {}", src.display()))?;
        
        debug!("Moved symlink: {} -> {} (target: {})", 
               src.display(), dst.display(), link_target.display());
        
        Ok(())
    }

    /// Copy symlink preserving its target
    fn copy_symlink(&self, src: &Path, dst: &Path) -> Result<()> {
        let link_target = fs::read_link(src)
            .with_context(|| format!("Failed to read symlink: {}", src.display()))?;
        
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&link_target, dst)
                .with_context(|| format!("Failed to create symlink: {}", dst.display()))?;
        }
        
        #[cfg(windows)]
        {
            if link_target.is_dir() {
                std::os::windows::fs::symlink_dir(&link_target, dst)
                    .with_context(|| format!("Failed to create directory symlink: {}", dst.display()))?;
            } else {
                std::os::windows::fs::symlink_file(&link_target, dst)
                    .with_context(|| format!("Failed to create file symlink: {}", dst.display()))?;
            }
        }
        
        Ok(())
    }

    /// Copy file with retry mechanism for fallback
    pub fn copy_file_with_retry(&self, src: &Path, dst: &Path) -> CopyResult {
        for attempt in 0..=self.max_retries {
            let result = self.copy_file_with_fallback(src, dst);
            
            match &result {
                CopyResult::Skipped(reason) if self.is_transient_error(reason) => {
                    if attempt < self.max_retries {
                        debug!("Transient error on copy attempt {} for {}: {}. Retrying in {:?}...", 
                               attempt + 1, dst.display(), reason, self.retry_delay);
                        thread::sleep(self.retry_delay);
                        continue;
                    } else {
                        warn!("Max copy retries ({}) exceeded for {}: {}", 
                              self.max_retries, dst.display(), reason);
                        return result;
                    }
                }
                _ => return result,
            }
        }
        
        CopyResult::Failed("Unexpected retry loop exit".to_string())
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

        // Check if it's a symlink first
        if let Ok(metadata) = fs::symlink_metadata(src) {
            if metadata.file_type().is_symlink() {
                match self.copy_symlink(src, dst) {
                    Ok(()) => return CopyResult::Success,
                    Err(e) => return CopyResult::Failed(format!("Failed to copy symlink: {}", e)),
                }
            }
        }

        // Regular file copy
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

    /// Map backup file path to container target path
    pub fn map_backup_to_container_path(&self, backup_file_path: &Path, backup_root: &Path) -> Result<PathBuf> {
        // Get relative path from backup root
        let relative_path = backup_file_path.strip_prefix(backup_root)
            .with_context(|| format!("Backup file path {} is not under backup root {}", 
                                   backup_file_path.display(), backup_root.display()))?;

        // Map directly to container root
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

    /// Check if an error reason indicates a transient condition that might be retried
    fn is_transient_error(&self, reason: &str) -> bool {
        reason.contains("File busy") || reason.contains("Resource busy")
    }

    /// Check if error indicates file is busy
    fn is_file_busy(&self, error: &io::Error) -> bool {
        match error.kind() {
            io::ErrorKind::ResourceBusy => true,
            _ => {
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
}