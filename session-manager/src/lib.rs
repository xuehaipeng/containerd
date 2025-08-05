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

pub mod direct_restore;

#[derive(Debug, Deserialize, Serialize)]
pub struct PathMappings {
    pub mappings: HashMap<String, PathMapping>,
}

#[derive(Debug, Deserialize, Serialize)]
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

pub fn find_current_session(
    mappings_file: &Path,
    pod_info: &PodInfo,
) -> Result<Option<SessionInfo>> {
    if !mappings_file.exists() {
        warn!("Path mappings file not found: {}", mappings_file.display());
        return Ok(None);
    }

    let content = fs::read_to_string(mappings_file)
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
    
    // Try rsync first if available
    if which::which("rsync").is_ok() {
        transfer_data_rsync(source, target, timeout)
    } else {
        transfer_data_tar(source, target, timeout)
    }
}