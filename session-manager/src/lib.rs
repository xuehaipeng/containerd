use anyhow::{Context, Result, bail};
use log::{info, warn, debug};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf, Component};
use std::process::{Command, Stdio};
use std::io::{self, Write as IoWrite};
use std::time::Duration;
use std::thread;
use std::sync::Arc;
use parking_lot::RwLock;
use lru::LruCache;
use once_cell::sync::Lazy;
// Removed unused imports
use std::num::NonZeroUsize;
use std::collections::HashSet;

pub mod direct_restore;
pub mod direct_restore_enhanced;
mod optimized_io;
mod resource_manager;
mod async_operations;

// Global LRU cache for path mappings
static PATH_MAPPING_CACHE: Lazy<Arc<RwLock<LruCache<String, PathMapping>>>> = 
    Lazy::new(|| Arc::new(RwLock::new(LruCache::new(NonZeroUsize::new(1000).unwrap()))));



#[derive(Debug, Deserialize, Serialize)]
pub struct PathMappings {
    pub mappings: HashMap<String, PathMapping>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PathMapping {
    #[serde(default = "default_namespace")]
    pub namespace: String,
    pub pod_name: String,
    pub container_name: String,
    pub created_at: String,
    pub pod_hash: String,
    pub snapshot_hash: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub last_accessed: Option<String>,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug)]
pub struct SessionInfo {
    pub pod_hash: String,
    pub snapshot_hash: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug)]
pub struct TransferResult {
    pub success_count: usize,
    pub error_count: usize,
    pub skipped_count: usize,
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub struct PodInfo {
    pub namespace: String,
    pub pod_name: String,
    pub container_name: String,
}

impl PodInfo {
    pub fn from_args_and_env(
        namespace: Option<String>,
        pod_name: Option<String>,
        container_name: Option<String>,
    ) -> Result<Self> {
        let namespace = namespace
            .or_else(|| std::env::var("CURRENT_NAMESPACE").ok())
            .ok_or_else(|| anyhow::anyhow!("Namespace not provided via argument or CURRENT_NAMESPACE environment variable"))?;
        
        let pod_name = pod_name
            .or_else(|| std::env::var("HOSTNAME").ok())
            .ok_or_else(|| anyhow::anyhow!("Pod name not provided via argument or HOSTNAME environment variable"))?;
        
        let container_name = container_name
            .or_else(|| std::env::var("CURRENT_CONTAINER_NAME").ok())
            .ok_or_else(|| anyhow::anyhow!("Container name not provided via argument or CURRENT_CONTAINER_NAME environment variable"))?;

        Ok(PodInfo {
            namespace,
            pod_name,
            container_name,
        })
    }
}

pub fn validate_path_security(path: &Path, allowed_base: &Path) -> Result<()> {
    let canonical_path = path.canonicalize()
        .with_context(|| format!("Failed to canonicalize path: {}", path.display()))?;
    
    let canonical_base = allowed_base.canonicalize()
        .with_context(|| format!("Failed to canonicalize base path: {}", allowed_base.display()))?;
    
    if !canonical_path.starts_with(&canonical_base) {
        bail!("Path traversal detected: {} is outside allowed base {}", 
              canonical_path.display(), canonical_base.display());
    }
    
    // Additional check for suspicious path components
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
            _ => {} // Allow other components
        }
    }
    
    Ok(())
}

pub async fn find_current_session_async(
    mappings_file: &Path,
    pod_info: &PodInfo,
) -> Result<Option<SessionInfo>> {
    find_current_session_cached(mappings_file, pod_info).await
}

pub fn find_current_session(
    mappings_file: &Path,
    pod_info: &PodInfo,
) -> Result<Option<SessionInfo>> {
    if !mappings_file.exists() {
        warn!("Path mappings file not found: {}", mappings_file.display());
        return Ok(None);
    }

    let content = optimized_io::read_file_optimized(mappings_file)
        .with_context(|| format!("Failed to read mappings file: {}", mappings_file.display()))?;

    if content.trim().is_empty() {
        warn!("Path mappings file is empty: {}", mappings_file.display());
        return Ok(None);
    }

    let path_mappings: PathMappings = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse path mappings JSON from {}", mappings_file.display()))?;

    info!("Loaded {} path mappings", path_mappings.mappings.len());

    // Find the most recent matching entry
    let mut best_match: Option<(String, PathMapping)> = None;
    let mut latest_time: Option<chrono::DateTime<chrono::Utc>> = None;

    for (path_key, mapping) in path_mappings.mappings {
        if mapping.namespace == pod_info.namespace
            && mapping.pod_name == pod_info.pod_name
            && mapping.container_name == pod_info.container_name
        {
            let created_at = chrono::DateTime::parse_from_rfc3339(&mapping.created_at)
                .with_context(|| format!("Invalid created_at timestamp: {} for mapping {}", mapping.created_at, path_key))?
                .with_timezone(&chrono::Utc);

            if latest_time.map_or(true, |t| created_at > t) {
                latest_time = Some(created_at);
                best_match = Some((path_key, mapping));
            }
        }
    }

    match best_match {
        Some((path_key, mapping)) => {
            let created_at = chrono::DateTime::parse_from_rfc3339(&mapping.created_at)?
                .with_timezone(&chrono::Utc);
            
            info!("Found matching session mapping: {}", path_key);
            
            Ok(Some(SessionInfo {
                pod_hash: mapping.pod_hash,
                snapshot_hash: mapping.snapshot_hash,
                created_at,
            }))
        }
        None => {
            info!("No matching session found for namespace={}, pod={}, container={}", 
                  pod_info.namespace, pod_info.pod_name, pod_info.container_name);
            Ok(None)
        }
    }
}

pub fn is_directory_empty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(true);
    }
    
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("Failed to read directory: {}", path.display()))?;
    Ok(entries.next().is_none())
}

pub fn show_directory_contents(path: &Path) -> Result<()> {
    if !path.exists() {
        debug!("  Directory does not exist: {}", path.display());
        return Ok(());
    }
    
    let entries = fs::read_dir(path)
        .with_context(|| format!("Failed to read directory: {}", path.display()))?;
    
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let metadata = entry.metadata()
            .with_context(|| format!("Failed to get metadata for: {}", entry.path().display()))?;
        
        if metadata.is_dir() {
            debug!("  d {}", file_name.to_string_lossy());
        } else {
            debug!("  f {} ({}bytes)", file_name.to_string_lossy(), metadata.len());
        }
    }
    
    Ok(())
}

pub fn create_directory_with_lock(path: &Path) -> Result<()> {
    let lock_file = path.with_extension("lock");
    
    // Try to acquire lock
    let _lock = acquire_file_lock(&lock_file)
        .with_context(|| format!("Failed to acquire lock for directory creation: {}", path.display()))?;
    
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create directory: {}", path.display()))?;
        info!("Created directory: {}", path.display());
    } else {
        debug!("Directory already exists: {}", path.display());
    }
    
    Ok(())
}

fn acquire_file_lock(lock_file: &Path) -> Result<File> {
    let mut attempts = 0;
    const MAX_ATTEMPTS: u32 = 30;
    const RETRY_DELAY: Duration = Duration::from_millis(100);
    
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_file)
        {
            Ok(mut file) => {
                // Write process info to lock file
                writeln!(file, "pid={}", std::process::id())?;
                writeln!(file, "timestamp={}", chrono::Utc::now().to_rfc3339())?;
                return Ok(file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                attempts += 1;
                if attempts >= MAX_ATTEMPTS {
                    bail!("Failed to acquire lock after {} attempts: {}", MAX_ATTEMPTS, lock_file.display());
                }
                warn!("Lock file exists, waiting... (attempt {}/{})", attempts, MAX_ATTEMPTS);
                thread::sleep(RETRY_DELAY);
            }
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create lock file: {}", lock_file.display()));
            }
        }
    }
}

pub fn transfer_data_rsync(source: &Path, target: &Path, timeout: u64) -> Result<TransferResult> {
    let mut result = TransferResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    info!("Using rsync for data transfer from {} to {}", source.display(), target.display());
    
    let output = Command::new("timeout")
        .arg(timeout.to_string())
        .arg("rsync")
        .arg("-av")
        .arg("--delete")
        .arg("--ignore-errors")
        .arg("--force")
        .arg("--stats")
        .arg(format!("{}/", source.display()))
        .arg(format!("{}/", target.display()))
        .output()
        .with_context(|| "Failed to execute rsync command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    
    debug!("Rsync stdout: {}", stdout);
    
    if output.status.success() {
        info!("Rsync transfer completed successfully");
        // Parse rsync stats for file count (simplified)
        result.success_count = 1;
    } else {
        match output.status.code() {
            Some(124) => {
                result.errors.push("Operation timed out".to_string());
                result.error_count += 1;
            }
            Some(code) => {
                warn!("Rsync transfer completed with exit code {}: {}", code, stderr);
                result.errors.push(format!("Rsync exit code {}: {}", code, stderr));
                // Don't count as error if it's just warnings
                if code < 12 { // rsync exit codes < 12 are usually warnings
                    result.success_count = 1;
                } else {
                    result.error_count += 1;
                }
            }
            None => {
                result.errors.push("Rsync was terminated by signal".to_string());
                result.error_count += 1;
            }
        }
    }

    Ok(result)
}

pub fn transfer_data_tar(source: &Path, target: &Path, timeout: u64) -> Result<TransferResult> {
    let mut result = TransferResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    info!("Using tar for data transfer from {} to {}", source.display(), target.display());
    
    // Create tar source process
    let mut source_cmd = Command::new("timeout")
        .arg(timeout.to_string())
        .arg("tar")
        .arg("-cf")
        .arg("-")
        .arg("--exclude=.*.tar")
        .arg("--ignore-failed-read")
        .arg("-C")
        .arg(source)
        .arg(".")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "Failed to start tar source command")?;

    // Get stdout handle safely
    let source_stdout = source_cmd.stdout.take()
        .ok_or_else(|| anyhow::anyhow!("Failed to get stdout from tar source command"))?;

    // Create tar target process
    let target_cmd = Command::new("timeout")
        .arg(timeout.to_string())
        .arg("tar")
        .arg("-xf")
        .arg("-")
        .arg("--overwrite")
        .arg("-C")
        .arg(target)
        .stdin(source_stdout)
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "Failed to start tar target command")?;

    // Wait for both processes to complete
    let source_result = source_cmd.wait()
        .with_context(|| "Failed to wait for tar source command")?;
    
    let target_output = target_cmd.wait_with_output()
        .with_context(|| "Failed to wait for tar target command")?;

    // Check results
    if source_result.success() && target_output.status.success() {
        info!("Tar transfer completed successfully");
        result.success_count = 1;
    } else {
        let target_stderr = String::from_utf8_lossy(&target_output.stderr);
        
        if !source_result.success() {
            result.errors.push(format!("Tar source failed with exit code: {:?}", source_result.code()));
            result.error_count += 1;
        }
        
        if !target_output.status.success() {
            if target_stderr.contains("Exiting with failure status due to previous errors") {
                warn!("Tar transfer completed with some skipped files (this is normal for busy files)");
                result.skipped_count += 1;
                result.success_count = 1; // Still consider it successful
            } else {
                warn!("Tar target failed: {}", target_stderr);
                result.errors.push(format!("Tar target error: {}", target_stderr));
                result.error_count += 1;
            }
        }
    }

    Ok(result)
}

pub fn transfer_data(source: &Path, target: &Path, timeout: u64) -> Result<TransferResult> {
    // Validate paths for security
    validate_path_security(source, &PathBuf::from("/"))?;
    validate_path_security(target, &PathBuf::from("/"))?;
    
    // Use resource manager for optimized operations
    let resource_manager = resource_manager::ResourceManager::global();
    
    resource_manager.thread_pool.execute_io(|| {
        // Try optimized rsync first if available
        if which::which("rsync").is_ok() {
            transfer_data_rsync(source, target, timeout)
        } else {
            transfer_data_tar(source, target, timeout)
        }
    })
}

/// Cached version of find_current_session with async support
async fn find_current_session_cached(
    mappings_file: &Path,
    pod_info: &PodInfo,
) -> Result<Option<SessionInfo>> {
    crate::async_operations::find_current_session_cached(mappings_file, pod_info).await
}

/// Transfer data with optimized parallel operations
pub async fn transfer_data_parallel(source: &Path, target: &Path, timeout: u64) -> Result<TransferResult> {
    // Validate paths for security
    validate_path_security(source, &PathBuf::from("/"))?;
    validate_path_security(target, &PathBuf::from("/"))?;
    
    let mut result = TransferResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };
    
    info!("Using optimized parallel transfer from {} to {}", source.display(), target.display());
    
    // Use async file operations with timeout
    let transfer_future = optimized_io::copy_file_async(source, target);
    let timeout_duration = std::time::Duration::from_secs(timeout);
    
    match tokio::time::timeout(timeout_duration, transfer_future).await {
        Ok(Ok(bytes_copied)) => {
            info!("Parallel transfer completed successfully: {} bytes", bytes_copied);
            result.success_count = 1;
        }
        Ok(Err(e)) => {
            warn!("Parallel transfer failed: {}", e);
            result.errors.push(format!("Transfer error: {}", e));
            result.error_count = 1;
        }
        Err(_) => {
            result.errors.push("Operation timed out".to_string());
            result.error_count = 1;
        }
    }
    
    Ok(result)
}

/// Optimized file integrity verification using Blake3 hashing
pub fn verify_file_integrity(file1: &Path, file2: &Path) -> Result<bool> {
    let resource_manager = resource_manager::ResourceManager::global();
    
    resource_manager.thread_pool.execute_compute(|| {
        let hash1 = optimized_io::hash_file_parallel(file1)?;
        let hash2 = optimized_io::hash_file_parallel(file2)?;
        Ok(hash1 == hash2)
    })
}

/// Detect mounted paths by parsing /proc/mounts and return them as a HashSet
pub fn get_mounted_paths() -> Result<HashSet<PathBuf>> {
    let mut mounted_paths = HashSet::new();
    
    let mounts_content = fs::read_to_string("/proc/mounts")
        .context("Failed to read /proc/mounts")?;
    
    for line in mounts_content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let mount_point = parts[1];
            // Skip root filesystem mount
            if mount_point != "/" {
                mounted_paths.insert(PathBuf::from(mount_point));
            }
        }
    }
    
    info!("Detected {} mounted paths (excluding root /)", mounted_paths.len());
    debug!("Mounted paths: {:?}", mounted_paths);
    
    Ok(mounted_paths)
}

/// Check if a path or any of its parents are mounted
pub fn is_path_mounted(path: &Path, mounted_paths: &HashSet<PathBuf>) -> bool {
    // Check if the exact path is mounted
    if mounted_paths.contains(path) {
        return true;
    }
    
    // Check if any parent directory is a mount point
    for ancestor in path.ancestors() {
        if mounted_paths.contains(ancestor) {
            return true;
        }
    }
    
    false
}

/// Transfer data with mount bypassing capability
pub fn transfer_data_with_mount_bypass(source: &Path, target: &Path, timeout: u64, bypass_mounts: bool) -> Result<TransferResult> {
    // Validate paths for security
    validate_path_security(source, &PathBuf::from("/"))?;
    validate_path_security(target, &PathBuf::from("/"))?;
    
    if bypass_mounts {
        info!("Mount bypass enabled - detecting mounted paths");
        let mounted_paths = get_mounted_paths()?;
        transfer_data_with_exclusions_robust(source, target, timeout, &mounted_paths)
    } else {
        transfer_data(source, target, timeout)
    }
}

/// Robust transfer with multiple fallback strategies
fn transfer_data_with_exclusions_robust(source: &Path, target: &Path, timeout: u64, mounted_paths: &HashSet<PathBuf>) -> Result<TransferResult> {
    // Try rsync first if available
    if which::which("rsync").is_ok() {
        info!("Using rsync for transfer with mount exclusions");
        match transfer_data_with_exclusions_rsync(source, target, timeout, mounted_paths) {
            Ok(result) if result.error_count == 0 => return Ok(result),
            Ok(result) => {
                warn!("Rsync completed with errors, trying native fallback");
                debug!("Rsync errors: {:?}", result.errors);
            }
            Err(e) => {
                warn!("Rsync failed: {}, trying native fallback", e);
            }
        }
    } else {
        info!("rsync not available, using native file operations");
    }
    
    // Fall back to native Rust file operations
    transfer_data_with_exclusions_native(source, target, timeout, mounted_paths)
}

/// Native Rust file copying with mount exclusions
fn transfer_data_with_exclusions_native(source: &Path, target: &Path, timeout: u64, mounted_paths: &HashSet<PathBuf>) -> Result<TransferResult> {
    let mut result = TransferResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    info!("Using native file operations with mount exclusions from {} to {}", source.display(), target.display());
    
    let start_time = std::time::Instant::now();
    let timeout_duration = std::time::Duration::from_secs(timeout);
    
    // Create target directory if it doesn't exist
    if !target.exists() {
        fs::create_dir_all(target)
            .with_context(|| format!("Failed to create target directory: {}", target.display()))?;
    }
    
    // Recursively copy files with mount exclusions
    copy_directory_recursive(source, target, source, mounted_paths, &mut result, start_time, timeout_duration)?;
    
    if result.success_count > 0 || (result.success_count == 0 && result.error_count == 0) {
        info!("Native transfer completed successfully: {} files copied, {} skipped, {} errors", 
              result.success_count, result.skipped_count, result.error_count);
    }
    
    Ok(result)
}

/// Recursively copy directory contents with exclusions
fn copy_directory_recursive(
    current_source: &Path,
    current_target: &Path, 
    source_root: &Path,
    mounted_paths: &HashSet<PathBuf>,
    result: &mut TransferResult,
    start_time: std::time::Instant,
    timeout: std::time::Duration,
) -> Result<()> {
    // Check timeout
    if start_time.elapsed() > timeout {
        result.errors.push("Operation timed out".to_string());
        result.error_count += 1;
        return Err(anyhow::anyhow!("Transfer operation timed out"));
    }
    
    let entries = match fs::read_dir(current_source) {
        Ok(entries) => entries,
        Err(e) => {
            let error_msg = format!("Failed to read directory {}: {}", current_source.display(), e);
            warn!("{}", error_msg);
            result.errors.push(error_msg);
            result.error_count += 1;
            return Ok(()); // Continue with other directories
        }
    };
    
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                let error_msg = format!("Failed to read directory entry in {}: {}", current_source.display(), e);
                warn!("{}", error_msg);
                result.errors.push(error_msg);
                result.error_count += 1;
                continue;
            }
        };
        
        let source_path = entry.path();
        let file_name = entry.file_name();
        let target_path = current_target.join(&file_name);
        
        // Check if this path should be excluded (mounted path)
        if is_path_excluded(&source_path, source_root, mounted_paths) {
            debug!("Skipping mounted path: {}", source_path.display());
            result.skipped_count += 1;
            continue;
        }
        
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(e) => {
                let error_msg = format!("Failed to get metadata for {}: {}", source_path.display(), e);
                warn!("{}", error_msg);
                result.errors.push(error_msg);
                result.error_count += 1;
                continue;
            }
        };
        
        if metadata.is_dir() {
            // Create target directory
            if let Err(e) = fs::create_dir_all(&target_path) {
                let error_msg = format!("Failed to create directory {}: {}", target_path.display(), e);
                warn!("{}", error_msg);
                result.errors.push(error_msg);
                result.error_count += 1;
                continue;
            }
            
            // Recursively copy directory contents
            copy_directory_recursive(&source_path, &target_path, source_root, mounted_paths, result, start_time, timeout)?;
        } else if metadata.is_file() {
            // Copy file
            match copy_file_with_permissions(&source_path, &target_path) {
                Ok(_) => {
                    result.success_count += 1;
                    debug!("Copied file: {} -> {}", source_path.display(), target_path.display());
                }
                Err(e) => {
                    let error_msg = format!("Failed to copy file {} to {}: {}", source_path.display(), target_path.display(), e);
                    warn!("{}", error_msg);
                    result.errors.push(error_msg);
                    result.error_count += 1;
                }
            }
        } else if metadata.file_type().is_symlink() {
            // Handle symlinks
            match copy_symlink(&source_path, &target_path) {
                Ok(_) => {
                    result.success_count += 1;
                    debug!("Copied symlink: {} -> {}", source_path.display(), target_path.display());
                }
                Err(e) => {
                    let error_msg = format!("Failed to copy symlink {} to {}: {}", source_path.display(), target_path.display(), e);
                    warn!("{}", error_msg);
                    result.errors.push(error_msg);
                    result.error_count += 1;
                }
            }
        } else {
            // Skip special files (devices, pipes, etc.)
            debug!("Skipping special file: {}", source_path.display());
            result.skipped_count += 1;
        }
        
        // Check timeout periodically
        if start_time.elapsed() > timeout {
            result.errors.push("Operation timed out".to_string());
            result.error_count += 1;
            return Err(anyhow::anyhow!("Transfer operation timed out"));
        }
    }
    
    Ok(())
}

/// Check if a path should be excluded based on mount points
fn is_path_excluded(file_path: &Path, source_root: &Path, mounted_paths: &HashSet<PathBuf>) -> bool {
    // Get the path relative to source root to check against mounted paths
    if let Ok(relative_path) = file_path.strip_prefix(source_root) {
        let absolute_path = PathBuf::from("/").join(relative_path);
        
        // Check if this absolute path or any of its parents is mounted
        if is_path_mounted(&absolute_path, mounted_paths) {
            return true;
        }
    }
    
    false
}

/// Copy a file preserving permissions and metadata
fn copy_file_with_permissions(source: &Path, target: &Path) -> Result<()> {
    // Create parent directory if needed
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for: {}", target.display()))?;
    }
    
    // Copy the file
    fs::copy(source, target)
        .with_context(|| format!("Failed to copy file from {} to {}", source.display(), target.display()))?;
    
    // Copy permissions
    #[cfg(unix)]
    {
        let metadata = source.metadata()
            .with_context(|| format!("Failed to get metadata for: {}", source.display()))?;
        let permissions = metadata.permissions();
        fs::set_permissions(target, permissions)
            .with_context(|| format!("Failed to set permissions for: {}", target.display()))?;
    }
    
    Ok(())
}

/// Copy a symlink
fn copy_symlink(source: &Path, target: &Path) -> Result<()> {
    let link_target = fs::read_link(source)
        .with_context(|| format!("Failed to read symlink: {}", source.display()))?;
    
    // Remove target if it exists
    if target.exists() {
        fs::remove_file(target)
            .with_context(|| format!("Failed to remove existing target: {}", target.display()))?;
    }
    
    #[cfg(unix)]
    std::os::unix::fs::symlink(&link_target, target)
        .with_context(|| format!("Failed to create symlink from {} to {}", link_target.display(), target.display()))?;
    
    #[cfg(windows)]
    {
        if link_target.is_dir() {
            std::os::windows::fs::symlink_dir(&link_target, target)
                .with_context(|| format!("Failed to create directory symlink from {} to {}", link_target.display(), target.display()))?;
        } else {
            std::os::windows::fs::symlink_file(&link_target, target)
                .with_context(|| format!("Failed to create file symlink from {} to {}", link_target.display(), target.display()))?;
        }
    }
    
    Ok(())
}

/// Transfer data excluding mounted paths using rsync (fallback)
fn transfer_data_with_exclusions_rsync(source: &Path, target: &Path, timeout: u64, mounted_paths: &HashSet<PathBuf>) -> Result<TransferResult> {
    let mut result = TransferResult {
        success_count: 0,
        error_count: 0,
        skipped_count: 0,
        errors: Vec::new(),
    };

    info!("Using rsync with mount exclusions from {} to {}", source.display(), target.display());
    
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout.to_string())
       .arg("rsync")
       .arg("-av")
       .arg("--delete")
       .arg("--ignore-errors")
       .arg("--force")
       .arg("--stats");
    
    // Add exclusions for mounted paths that are within the source directory
    for mount_path in mounted_paths {
        // Only exclude if mount is within source directory
        if let Ok(relative_path) = mount_path.strip_prefix(source) {
            let exclude_pattern = format!("/{}", relative_path.display());
            cmd.arg("--exclude").arg(&exclude_pattern);
            info!("Excluding mounted path: {}", exclude_pattern);
        }
    }
    
    cmd.arg(format!("{}/", source.display()))
       .arg(format!("{}/", target.display()));

    let output = cmd.output()
        .with_context(|| "Failed to execute rsync command with exclusions")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    
    debug!("Rsync stdout: {}", stdout);
    
    if output.status.success() {
        info!("Rsync transfer with mount exclusions completed successfully");
        result.success_count = 1;
    } else {
        match output.status.code() {
            Some(124) => {
                result.errors.push("Operation timed out".to_string());
                result.error_count += 1;
            }
            Some(code) => {
                warn!("Rsync transfer completed with exit code {}: {}", code, stderr);
                result.errors.push(format!("Rsync exit code {}: {}", code, stderr));
                if code < 12 { // rsync exit codes < 12 are usually warnings
                    result.success_count = 1;
                } else {
                    result.error_count += 1;
                }
            }
            None => {
                result.errors.push("Rsync was terminated by signal".to_string());
                result.error_count += 1;
            }
        }
    }

    Ok(result)
}