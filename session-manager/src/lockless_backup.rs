use anyhow::{Context, Result};
use log::{info, warn, debug};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct BackupMetadata {
    pub started_at: u64,
    pub process_id: u32,
    pub hostname: String,
    pub operation: String,
    pub status: BackupStatus,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum BackupStatus {
    InProgress,
    Completed,
    Failed,
}

pub struct LocklessBackupManager {
    pub operation_name: String,
    pub enable_metadata: bool,
}

impl LocklessBackupManager {
    pub fn new(operation_name: String) -> Self {
        Self {
            operation_name,
            enable_metadata: true,
        }
    }

    pub fn disable_metadata(mut self) -> Self {
        self.enable_metadata = false;
        self
    }

    /// Create directory without any locking - safe for single-process operations
    pub fn create_directory_lockless(&self, path: &Path) -> Result<()> {
        debug!("Creating directory (lockless): {}", path.display());

        // Check if we should write operation metadata
        let metadata_file = path.with_extension("backup_meta");
        
        if self.enable_metadata {
            self.write_backup_metadata(&metadata_file, BackupStatus::InProgress)?;
        }

        if !path.exists() {
            fs::create_dir_all(path)
                .with_context(|| format!("Failed to create directory: {}", path.display()))?;
            info!("Created directory (lockless): {}", path.display());
        } else {
            debug!("Directory already exists: {}", path.display());
        }

        if self.enable_metadata {
            self.write_backup_metadata(&metadata_file, BackupStatus::Completed)?;
        }

        Ok(())
    }

    /// Execute backup operation with metadata tracking (no locks)
    pub fn execute_backup_operation<F>(&self, operation: F, metadata_path: Option<&Path>) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        let metadata_file = metadata_path.map(|p| p.with_extension("backup_meta"));

        // Start operation metadata
        if let Some(ref meta_file) = metadata_file {
            if self.enable_metadata {
                self.write_backup_metadata(meta_file, BackupStatus::InProgress)?;
            }
        }

        // Execute the actual backup operation
        let result = operation();

        // Update metadata based on result
        if let Some(ref meta_file) = metadata_file {
            if self.enable_metadata {
                let status = match &result {
                    Ok(()) => BackupStatus::Completed,
                    Err(_) => BackupStatus::Failed,
                };
                
                if let Err(e) = self.write_backup_metadata(meta_file, status) {
                    warn!("Failed to update backup metadata: {}", e);
                    // Don't fail the operation just because metadata write failed
                }
            }
        }

        result
    }

    /// Check if another backup might be running (optional safety check)
    pub fn check_concurrent_backup(&self, path: &Path) -> Result<Option<BackupMetadata>> {
        if !self.enable_metadata {
            return Ok(None);
        }

        let metadata_file = path.with_extension("backup_meta");
        
        if !metadata_file.exists() {
            return Ok(None);
        }

        match self.read_backup_metadata(&metadata_file) {
            Ok(metadata) => {
                if metadata.status == BackupStatus::InProgress {
                    let age_seconds = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() - metadata.started_at;

                    // Consider operations older than 30 minutes as stale
                    if age_seconds > 1800 {
                        warn!("Found stale backup metadata ({}s old), proceeding with backup", age_seconds);
                        return Ok(None);
                    }

                    info!("Detected potentially concurrent backup: PID={}, age={}s", 
                          metadata.process_id, age_seconds);
                    return Ok(Some(metadata));
                }
            }
            Err(e) => {
                debug!("Could not read backup metadata (proceeding): {}", e);
            }
        }

        Ok(None)
    }

    /// Write backup operation metadata
    fn write_backup_metadata(&self, metadata_file: &Path, status: BackupStatus) -> Result<()> {
        let metadata = BackupMetadata {
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            process_id: std::process::id(),
            hostname: self.get_hostname(),
            operation: self.operation_name.clone(),
            status,
        };

        let content = serde_json::to_string_pretty(&metadata)
            .context("Failed to serialize backup metadata")?;

        fs::write(metadata_file, content)
            .with_context(|| format!("Failed to write backup metadata: {}", metadata_file.display()))?;

        debug!("Updated backup metadata: {:?}", metadata);
        Ok(())
    }

    /// Read backup operation metadata
    fn read_backup_metadata(&self, metadata_file: &Path) -> Result<BackupMetadata> {
        let content = fs::read_to_string(metadata_file)
            .with_context(|| format!("Failed to read backup metadata: {}", metadata_file.display()))?;

        let metadata: BackupMetadata = serde_json::from_str(&content)
            .context("Failed to parse backup metadata")?;

        Ok(metadata)
    }

    /// Get hostname for metadata
    fn get_hostname(&self) -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("POD_NAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Clean up completed backup metadata files older than specified age
    pub fn cleanup_old_metadata(&self, directory: &Path, max_age_hours: u64) -> Result<usize> {
        if !self.enable_metadata || !directory.exists() {
            return Ok(0);
        }

        let max_age_seconds = max_age_hours * 3600;
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut cleaned_count = 0;

        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "backup_meta") {
                match self.read_backup_metadata(&path) {
                    Ok(metadata) => {
                        let age = current_time - metadata.started_at;
                        
                        // Only clean up completed or failed backups that are old enough
                        if (metadata.status == BackupStatus::Completed || metadata.status == BackupStatus::Failed) 
                           && age > max_age_seconds {
                            
                            match fs::remove_file(&path) {
                                Ok(()) => {
                                    debug!("Cleaned up old backup metadata: {}", path.display());
                                    cleaned_count += 1;
                                }
                                Err(e) => {
                                    warn!("Failed to remove old backup metadata {}: {}", path.display(), e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!("Could not read backup metadata {}: {}", path.display(), e);
                    }
                }
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} old backup metadata files", cleaned_count);
        }

        Ok(cleaned_count)
    }
}

/// Lockless directory creation - optimized for single-process operations
pub fn create_directory_lockless(path: &Path, operation_name: &str) -> Result<()> {
    let manager = LocklessBackupManager::new(operation_name.to_string());
    manager.create_directory_lockless(path)
}

/// Simple lockless directory creation without metadata
pub fn create_directory_simple(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create directory: {}", path.display()))?;
        info!("Created directory: {}", path.display());
    } else {
        debug!("Directory already exists: {}", path.display());
    }
    Ok(())
}

/// Execute backup with optional safety check (but no blocking)
pub fn execute_backup_with_safety_check<F>(
    path: &Path, 
    operation_name: &str, 
    backup_fn: F
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let manager = LocklessBackupManager::new(operation_name.to_string());
    
    // Optional: Check for concurrent operations (informational only)
    if let Ok(Some(metadata)) = manager.check_concurrent_backup(path) {
        warn!("Detected potentially concurrent backup operation: PID={}, started at {}", 
              metadata.process_id, metadata.started_at);
        warn!("Proceeding anyway since session backup should be single-process");
    }

    // Execute backup with metadata tracking
    manager.execute_backup_operation(backup_fn, Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_lockless_directory_creation() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path().join("test_backup");
        
        let result = create_directory_lockless(&test_path, "test_operation");
        assert!(result.is_ok());
        assert!(test_path.exists());
    }

    #[test]
    fn test_lockless_backup_operation() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path().join("test_backup");
        
        let result = execute_backup_with_safety_check(&test_path, "test_backup", || {
            fs::create_dir_all(&test_path)?;
            fs::write(test_path.join("test_file.txt"), "test content")?;
            Ok(())
        });
        
        assert!(result.is_ok());
        assert!(test_path.exists());
        assert!(test_path.join("test_file.txt").exists());
    }

    #[test]
    fn test_metadata_tracking() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path().join("test_backup");
        
        let manager = LocklessBackupManager::new("test".to_string());
        
        let result = manager.execute_backup_operation(|| {
            fs::create_dir_all(&test_path)?;
            Ok(())
        }, Some(&test_path));
        
        assert!(result.is_ok());
        
        // Check that metadata file was created
        let metadata_file = test_path.with_extension("backup_meta");
        assert!(metadata_file.exists());
        
        // Verify metadata content
        let metadata = manager.read_backup_metadata(&metadata_file).unwrap();
        assert_eq!(metadata.status, BackupStatus::Completed);
        assert_eq!(metadata.operation, "test");
    }

    #[test]
    fn test_concurrent_detection() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path().join("test_backup");
        
        let manager = LocklessBackupManager::new("test".to_string());
        
        // Write in-progress metadata
        let metadata_file = test_path.with_extension("backup_meta");
        manager.write_backup_metadata(&metadata_file, BackupStatus::InProgress).unwrap();
        
        // Check for concurrent operation
        let concurrent = manager.check_concurrent_backup(&test_path).unwrap();
        assert!(concurrent.is_some());
        assert_eq!(concurrent.unwrap().status, BackupStatus::InProgress);
    }
}