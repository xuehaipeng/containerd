use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Adaptive buffer size based on filesystem type
pub fn get_optimal_buffer_size(path: &Path) -> usize {
    // Check if path is on network filesystem
    if is_network_filesystem(path) {
        4 * 1024 * 1024  // 4MB for network filesystems
    } else {
        256 * 1024  // 256KB for local filesystems
    }
}

/// Detect if path is on a network filesystem
fn is_network_filesystem(path: &Path) -> bool {
    // Try to get filesystem type from /proc/mounts
    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        if let Some(mount_line) = find_mount_point(path, &mounts) {
            let fs_type = mount_line.split_whitespace().nth(2).unwrap_or("");
            return matches!(fs_type, "nfs" | "nfs4" | "cifs" | "smb" | "glusterfs" | "gpfs" | "lustre" | "ceph" | "beegfs");
        }
    }
    false
}

/// Find the mount point for a given path
fn find_mount_point(path: &Path, mounts: &str) -> Option<String> {
    let path_str = path.to_string_lossy();
    let mut best_match = None;
    let mut best_match_len = 0;
    
    for line in mounts.lines() {
        if let Some(mount_point) = line.split_whitespace().nth(1) {
            if path_str.starts_with(mount_point) && mount_point.len() > best_match_len {
                best_match = Some(line.to_string());
                best_match_len = mount_point.len();
            }
        }
    }
    
    best_match
}

/// Regular file copy with adaptive buffer size
pub fn copy_file_regular(src: &Path, dst: &Path) -> Result<u64> {
    let buffer_size = get_optimal_buffer_size(src);
    
    let src_file = File::open(src)
        .with_context(|| format!("Failed to open source: {}", src.display()))?;
    
    // Create parent directory if needed
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    
    let dst_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .with_context(|| format!("Failed to create destination: {}", dst.display()))?;
    
    let mut reader = BufReader::with_capacity(buffer_size, src_file);
    let mut writer = BufWriter::with_capacity(buffer_size, dst_file);
    
    let bytes_copied = io::copy(&mut reader, &mut writer)?;
    writer.flush()?;
    
    // Copy file permissions
    let src_metadata = fs::metadata(src)?;
    let permissions = src_metadata.permissions();
    fs::set_permissions(dst, permissions)?;
    
    Ok(bytes_copied)
}

/// Copy file using the best available strategy
pub fn copy_file_best_strategy(src: &Path, dst: &Path) -> Result<u64> {
    // For now, use the optimized regular copy with adaptive buffer sizing
    debug!("Using optimized copy with adaptive buffer for {}", src.display());
    copy_file_regular(src, dst)
}

/// Statistics for parallel copy operation
#[derive(Debug, Default)]
pub struct CopyStats {
    pub files_copied: AtomicUsize,
    pub bytes_copied: AtomicU64,
    pub errors: AtomicUsize,
    pub skipped: AtomicUsize,
}

impl CopyStats {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn add_file(&self, bytes: u64) {
        self.files_copied.fetch_add(1, Ordering::Relaxed);
        self.bytes_copied.fetch_add(bytes, Ordering::Relaxed);
    }
    
    pub fn add_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn add_skipped(&self) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn get_summary(&self) -> (usize, u64, usize, usize) {
        (
            self.files_copied.load(Ordering::Relaxed),
            self.bytes_copied.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.skipped.load(Ordering::Relaxed),
        )
    }
}

/// File copy task for parallel processing
#[derive(Clone)]
struct CopyTask {
    src: PathBuf,
    dst: PathBuf,
    relative_path: PathBuf,
}

/// Copy directory using parallel processing with Rayon
pub fn copy_directory_parallel(
    src: &Path,
    dst: &Path,
    max_workers: Option<usize>,
) -> Result<CopyStats> {
    let stats = CopyStats::new();
    let start_time = Instant::now();
    
    info!("Starting parallel copy from {} to {}", src.display(), dst.display());
    
    // Create destination directory
    fs::create_dir_all(dst)?;
    
    // Collect all files to copy
    let tasks = collect_copy_tasks(src, dst)?;
    let total_tasks = tasks.len();
    
    info!("Found {} files to copy", total_tasks);
    
    // Set up thread pool
    let num_workers = max_workers.unwrap_or_else(|| num_cpus::get().min(32));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_workers)
        .thread_name(|i| format!("copy-worker-{}", i))
        .build()?;
    
    // Process files in parallel
    let stats_ref = &stats;
    pool.install(|| {
        tasks.par_iter().for_each(|task| {
            // Create parent directory if needed
            if let Some(parent) = task.dst.parent() {
                let _ = fs::create_dir_all(parent);
            }
            
            // Copy the file
            match copy_file_best_strategy(&task.src, &task.dst) {
                Ok(bytes) => {
                    stats_ref.add_file(bytes);
                    debug!("Copied: {} ({} bytes)", task.relative_path.display(), bytes);
                }
                Err(e) => {
                    warn!("Failed to copy {}: {}", task.relative_path.display(), e);
                    stats_ref.add_error();
                }
            }
        });
    });
    
    let (files, bytes, errors, skipped) = stats.get_summary();
    let elapsed = start_time.elapsed();
    let throughput = if elapsed.as_secs() > 0 {
        bytes / elapsed.as_secs()
    } else {
        bytes
    };
    
    info!(
        "Parallel copy completed in {:.2}s: {} files, {} bytes ({}/s), {} errors, {} skipped",
        elapsed.as_secs_f64(),
        files,
        bytes,
        format_bytes(throughput),
        errors,
        skipped
    );
    
    Ok(stats)
}

/// Collect all files to copy recursively
fn collect_copy_tasks(src: &Path, dst: &Path) -> Result<Vec<CopyTask>> {
    let mut tasks = Vec::new();
    collect_copy_tasks_recursive(src, dst, src, &mut tasks)?;
    Ok(tasks)
}

fn collect_copy_tasks_recursive(
    current: &Path,
    dst_root: &Path,
    src_root: &Path,
    tasks: &mut Vec<CopyTask>,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(src_root)?;
        let dst_path = dst_root.join(relative);
        
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            fs::create_dir_all(&dst_path)?;
            collect_copy_tasks_recursive(&path, dst_root, src_root, tasks)?;
        } else if metadata.is_file() {
            tasks.push(CopyTask {
                src: path.clone(),
                dst: dst_path,
                relative_path: relative.to_path_buf(),
            });
        }
    }
    
    Ok(())
}

/// Format bytes in human-readable format
fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    
    format!("{:.2} {}", size, UNITS[unit_index])
}

#[cfg(feature = "io_uring")]
pub mod io_uring_copy {
    use super::*;
    use tokio_uring::fs::{File as UringFile, OpenOptions};
    
    /// Copy file using io_uring (requires Linux 5.6+)
    pub async fn copy_file_io_uring(src: &Path, dst: &Path) -> Result<u64> {
        // Start io_uring runtime
        tokio_uring::start(async {
            copy_file_io_uring_inner(src, dst).await
        })
    }
    
    async fn copy_file_io_uring_inner(src: &Path, dst: &Path) -> Result<u64> {
        let src_file = UringFile::open(src).await
            .with_context(|| format!("Failed to open source with io_uring: {}", src.display()))?;
        
        // Create parent directory if needed
        if let Some(parent) = dst.parent() {
            tokio_uring::fs::create_dir_all(parent).await?;
        }
        
        let dst_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst)
            .await
            .with_context(|| format!("Failed to create destination with io_uring: {}", dst.display()))?;
        
        // Get file size
        let metadata = tokio_uring::fs::metadata(src).await?;
        let file_size = metadata.len();
        
        // Use large buffer for io_uring
        const BUFFER_SIZE: usize = 16 * 1024 * 1024; // 16MB
        let mut offset = 0u64;
        let mut total_copied = 0u64;
        
        while offset < file_size {
            let to_read = std::cmp::min(BUFFER_SIZE, (file_size - offset) as usize);
            let buf = vec![0u8; to_read];
            
            // Read from source
            let (res, buf) = src_file.read_at(buf, offset).await;
            let bytes_read = res?;
            
            if bytes_read == 0 {
                break;
            }
            
            // Write to destination
            let (res, _) = dst_file.write_at(buf, offset).await;
            let bytes_written = res?;
            
            offset += bytes_written as u64;
            total_copied += bytes_written as u64;
        }
        
        // Sync to ensure data is written
        dst_file.sync_all().await?;
        
        Ok(total_copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[test]
    fn test_buffer_size_detection() {
        let local_path = Path::new("/tmp/test");
        let buffer_size = get_optimal_buffer_size(local_path);
        assert_eq!(buffer_size, 256 * 1024);
    }
    
    #[test]
    fn test_copy_strategies() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("source.txt");
        let dst = temp_dir.path().join("dest.txt");
        
        // Create test file
        fs::write(&src, b"test content").unwrap();
        
        // Test copy
        let bytes = copy_file_best_strategy(&src, &dst).unwrap();
        assert_eq!(bytes, 12);
        
        // Verify content
        let content = fs::read(&dst).unwrap();
        assert_eq!(content, b"test content");
    }
}