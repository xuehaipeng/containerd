use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;
use crossbeam_channel::{unbounded, Sender};
use crate::resource_manager::ResourceManager;

#[cfg(target_os = "linux")]
use libc::{posix_fadvise, POSIX_FADV_SEQUENTIAL, POSIX_FADV_DONTNEED};

/// Check if files should be synced to disk after copy (configurable for performance)
fn should_fsync_files() -> bool {
    std::env::var("SESSION_MANAGER_FSYNC")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(true) // Default to true for data safety
}

/// Apply posix_fadvise for sequential access pattern
#[cfg(target_os = "linux")]
fn advise_sequential_access(file: &File, size: u64) {
    let fd = file.as_raw_fd();
    unsafe {
        posix_fadvise(fd, 0, size as libc::off_t, POSIX_FADV_SEQUENTIAL);
    }
}

#[cfg(not(target_os = "linux"))]
fn advise_sequential_access(_file: &File, _size: u64) {
    // No-op on non-Linux platforms
}

/// Apply posix_fadvise to drop pages from cache (reduce cache pollution)
#[cfg(target_os = "linux")]
fn advise_dont_need(file: &File, size: u64) {
    let fd = file.as_raw_fd();
    unsafe {
        posix_fadvise(fd, 0, size as libc::off_t, POSIX_FADV_DONTNEED);
    }
}

#[cfg(not(target_os = "linux"))]
fn advise_dont_need(_file: &File, _size: u64) {
    // No-op on non-Linux platforms
}

/// Check if a directory should be skipped based on device ID comparison
pub fn should_skip_directory(dir: &Path, root_dev: u64) -> bool {
    match dir.metadata() {
        Ok(meta) => {
            let dir_dev = meta.dev();
            if dir_dev != root_dev {
                debug!("Skipping directory {} (different device: {} != {})", 
                    dir.display(), dir_dev, root_dev);
                true
            } else {
                false
            }
        }
        Err(e) => {
            warn!("Failed to get metadata for {}: {}", dir.display(), e);
            false
        }
    }
}

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

/// System call wrapper for copy_file_range
#[cfg(target_os = "linux")]
fn copy_file_range_wrapper(
    fd_in: RawFd,
    off_in: Option<&mut i64>,
    fd_out: RawFd,
    off_out: Option<&mut i64>,
    len: usize,
) -> io::Result<usize> {
    use libc::{c_int, off64_t, size_t};
    
    let off_in_ptr = match off_in {
        Some(off) => off as *mut i64 as *mut off64_t,
        None => std::ptr::null_mut(),
    };
    
    let off_out_ptr = match off_out {
        Some(off) => off as *mut i64 as *mut off64_t,
        None => std::ptr::null_mut(),
    };
    
    let result = unsafe {
        libc::syscall(
            libc::SYS_copy_file_range,
            fd_in as c_int,
            off_in_ptr,
            fd_out as c_int,
            off_out_ptr,
            len as size_t,
            0u32,
        )
    };
    
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as usize)
    }
}

/// Copy file using copy_file_range (Linux 4.5+)
#[cfg(target_os = "linux")]
fn copy_file_with_copy_file_range(src_file: &File, dst_file: &File, size: u64) -> io::Result<u64> {
    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();
    
    let mut copied = 0u64;
    let chunk_size = 1024 * 1024 * 1024; // 1GB chunks
    
    while copied < size {
        let to_copy = std::cmp::min(chunk_size, (size - copied) as usize);
        
        match copy_file_range_wrapper(src_fd, None, dst_fd, None, to_copy) {
            Ok(0) => break, // EOF
            Ok(n) => copied += n as u64,
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                // Cross-device, not supported
                return Err(io::Error::new(io::ErrorKind::Unsupported, "cross-device copy"));
            }
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) || 
                     e.raw_os_error() == Some(libc::EOPNOTSUPP) ||
                     e.raw_os_error() == Some(libc::ENOSYS) => {
                // Not supported
                return Err(io::Error::new(io::ErrorKind::Unsupported, "copy_file_range not supported"));
            }
            Err(e) => return Err(e),
        }
    }
    
    Ok(copied)
}

/// Copy file using sendfile (zero-copy when possible)
#[cfg(target_os = "linux")]
fn copy_file_with_sendfile(src_file: &File, dst_file: &File, size: u64) -> io::Result<u64> {
    use libc::{sendfile64, off64_t};
    
    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();
    
    let mut copied = 0u64;
    let mut offset = 0i64;
    
    while copied < size {
        let to_copy = std::cmp::min(0x7ffff000, (size - copied) as usize); // ~2GB max per call
        
        let result = unsafe {
            sendfile64(
                dst_fd,
                src_fd,
                &mut offset as *mut i64 as *mut off64_t,
                to_copy,
            )
        };
        
        if result < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINVAL) || 
               err.raw_os_error() == Some(libc::ENOSYS) {
                // Not supported
                return Err(io::Error::new(io::ErrorKind::Unsupported, "sendfile not supported"));
            }
            return Err(err);
        } else if result == 0 {
            break; // EOF
        } else {
            copied += result as u64;
        }
    }
    
    Ok(copied)
}

/// Regular file copy with adaptive buffer size and optional fsync for durability
pub fn copy_file_regular(src: &Path, dst: &Path) -> Result<u64> {
    let buffer_size = get_optimal_buffer_size(dst);
    
    let src_file = File::open(src)
        .with_context(|| format!("Failed to open source: {}", src.display()))?;
    
    // Create parent directory if needed
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    let dst_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .with_context(|| format!("Failed to create destination: {}", dst.display()))?;
    
    let mut reader = BufReader::with_capacity(buffer_size, src_file);
    let mut writer = BufWriter::with_capacity(buffer_size, dst_file);
    
    let bytes_copied = io::copy(&mut reader, &mut writer)?;
    let dst_file = writer.into_inner()
        .map_err(|e| anyhow::anyhow!("Failed to flush buffer: {}", e.error()))?;
    
    // DURABILITY: Configurable fsync for data safety
    if should_fsync_files() {
        dst_file.sync_all()
            .with_context(|| format!("Failed to sync file to disk: {}", dst.display()))?;
        debug!("Synced file to disk: {}", dst.display());
    }
    
    // Copy file permissions and timestamps
    let src_metadata = fs::metadata(src)?;
    let permissions = src_metadata.permissions();
    fs::set_permissions(dst, permissions)?;
    
    // Preserve mtime as requested by reviewer
    if let Ok(mtime) = src_metadata.modified() {
        if let Err(e) = filetime::set_file_mtime(dst, filetime::FileTime::from_system_time(mtime)) {
            debug!("Failed to preserve mtime for {}: {}", dst.display(), e);
            // Don't fail the operation for timestamp issues
        }
    }
    
    Ok(bytes_copied)
}

/// Copy file using the best available strategy with kernel-assisted fallback chain
pub fn copy_file_best_strategy(src: &Path, dst: &Path) -> Result<u64> {
    // Check if source is a symlink
    let src_metadata = fs::symlink_metadata(src)?;
    
    if src_metadata.is_symlink() {
        // Handle symlink copying
        let target = fs::read_link(src)
            .with_context(|| format!("Failed to read symlink: {}", src.display()))?;
        
        // Create parent directory if needed
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        
        // Remove destination if it exists
        let _ = fs::remove_file(dst);
        
        // Create the symlink
        std::os::unix::fs::symlink(&target, dst)
            .with_context(|| format!("Failed to create symlink: {} -> {}", dst.display(), target.display()))?;
        
        debug!("Created symlink: {} -> {}", dst.display(), target.display());
        return Ok(0); // Symlinks don't have size
    }
    
    // For regular files, use the kernel-assisted copy chain with cache management
    let file_size = src_metadata.len();
    
    // Open files
    let src_file = File::open(src)
        .with_context(|| format!("Failed to open source: {}", src.display()))?;
    
    // Create parent directory if needed
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    let dst_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .with_context(|| format!("Failed to create destination: {}", dst.display()))?;
    
    // Apply sequential access advice for large files (>1MB)
    if file_size > 1024 * 1024 {
        advise_sequential_access(&src_file, file_size);
    }
    
    // Try kernel-assisted copy methods in order
    #[cfg(target_os = "linux")]
    {
        // 1. Try copy_file_range first (best for same filesystem)
        match copy_file_with_copy_file_range(&src_file, &dst_file, file_size) {
            Ok(bytes) => {
                debug!("Used copy_file_range for {}: {} bytes", src.display(), bytes);
                
                // Apply cache management for large files
                if file_size > 1024 * 1024 {
                    advise_dont_need(&src_file, file_size);
                }
                
                // DURABILITY: Sync file to disk if enabled
                if should_fsync_files() {
                    dst_file.sync_all()
                        .with_context(|| format!("Failed to sync file to disk: {}", dst.display()))?;
                    debug!("Synced file to disk: {}", dst.display());
                }
                
                // Copy permissions and timestamps
                let permissions = src_metadata.permissions();
                fs::set_permissions(dst, permissions)?;
                
                // Preserve mtime as requested by reviewer
                if let Ok(mtime) = src_metadata.modified() {
                    if let Err(e) = filetime::set_file_mtime(dst, filetime::FileTime::from_system_time(mtime)) {
                        debug!("Failed to preserve mtime for {}: {}", dst.display(), e);
                        // Don't fail the operation for timestamp issues
                    }
                }
                
                return Ok(bytes);
            }
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                debug!("copy_file_range not supported, trying sendfile");
            }
            Err(e) => {
                debug!("copy_file_range failed: {}, trying sendfile", e);
            }
        }
        
        // 2. Try sendfile (zero-copy for regular files)
        match copy_file_with_sendfile(&src_file, &dst_file, file_size) {
            Ok(bytes) => {
                debug!("Used sendfile for {}: {} bytes", src.display(), bytes);
                
                // Apply cache management for large files
                if file_size > 1024 * 1024 {
                    advise_dont_need(&src_file, file_size);
                }
                
                // DURABILITY: Sync file to disk if enabled
                if should_fsync_files() {
                    dst_file.sync_all()
                        .with_context(|| format!("Failed to sync file to disk: {}", dst.display()))?;
                    debug!("Synced file to disk: {}", dst.display());
                }
                
                // Copy permissions and timestamps
                let permissions = src_metadata.permissions();
                fs::set_permissions(dst, permissions)?;
                
                // Preserve mtime as requested by reviewer
                if let Ok(mtime) = src_metadata.modified() {
                    if let Err(e) = filetime::set_file_mtime(dst, filetime::FileTime::from_system_time(mtime)) {
                        debug!("Failed to preserve mtime for {}: {}", dst.display(), e);
                        // Don't fail the operation for timestamp issues
                    }
                }
                
                return Ok(bytes);
            }
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                debug!("sendfile not supported, falling back to buffered copy");
            }
            Err(e) => {
                debug!("sendfile failed: {}, falling back to buffered copy", e);
            }
        }
    }
    
    // 3. Fall back to buffered copy with adaptive buffer size
    debug!("Using buffered copy for {}", src.display());
    let result = copy_file_regular(src, dst);
    
    // Apply cache management after buffered copy for large files
    if file_size > 1024 * 1024 {
        if let Ok(_) = &result {
            advise_dont_need(&src_file, file_size);
        }
    }
    
    result
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
    
    /// Add skipped file count (for special files, mount points, etc.)
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

/// Copy directory using streaming parallel processing with producer-consumer pattern
/// This version starts copying immediately while traversal continues in background
pub fn copy_directory_parallel(
    src: &Path,
    dst: &Path,
    _max_workers: Option<usize>, // Ignored - using unified ResourceManager thread pool
) -> Result<CopyStats> {
    let stats = Arc::new(CopyStats::new());
    let start_time = Instant::now();
    
    info!("Starting streaming parallel copy from {} to {}", src.display(), dst.display());
    
    // Get the device ID of the source root for mount detection
    let root_dev = fs::metadata(src)
        .with_context(|| format!("Failed to get metadata for source: {}", src.display()))?
        .dev();
    
    info!("Source root device ID: {}", root_dev);
    
    // Create destination directory
    fs::create_dir_all(dst)?;
    
    // THREAD POOL UNIFICATION: Use global ResourceManager instead of creating separate pool
    // This prevents oversubscription issues when both fast_copy and ResourceManager are used
    let resource_manager = ResourceManager::global();
    
    // Log filesystem type for debugging (but use unified pool regardless)
    if is_network_filesystem(dst) {
        debug!("Detected network filesystem - using unified thread pool with backpressure");
    } else {
        debug!("Detected local filesystem - using unified thread pool");
    }
    
    // STREAMING OPTIMIZATION: Producer-Consumer Pattern  
    // Channel capacity: buffer to balance memory usage vs latency
    let (task_sender, task_receiver) = unbounded::<CopyTask>();
    let stats_ref = Arc::clone(&stats);
    
    // Clone paths for the producer thread
    let src_path = src.to_path_buf();
    let dst_path = dst.to_path_buf();
    let stats_for_producer = Arc::clone(&stats);
    
    // Spawn producer thread to stream copy tasks
    let producer_handle = std::thread::spawn(move || -> Result<usize> {
        let result = stream_copy_tasks(&src_path, &dst_path, root_dev, task_sender.clone(), &stats_for_producer);
        
        // Close channel to signal completion
        drop(task_sender);
        
        match result {
            Ok(count) => {
                info!("Producer finished: {} tasks queued", count);
                Ok(count)
            }
            Err(e) => {
                warn!("Producer failed: {}", e);
                Err(e)
            }
        }
    });
    
    // Start consumer threads immediately (copying while traversal continues)
    resource_manager.thread_pool.io_pool().install(|| {
        task_receiver.into_iter().par_bridge().for_each(|task| {
            // Create parent directory if needed
            if let Some(parent) = task.dst.parent() {
                let _ = fs::create_dir_all(parent);
            }
            
            // Copy the file with improved error handling
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
    
    // Wait for producer to complete and get task count
    let total_tasks = producer_handle.join()
        .map_err(|_| anyhow::anyhow!("Producer thread panicked"))?
        .unwrap_or(0);
    // producer's stats_for_producer clone is dropped when thread finishes
    
    // CRITICAL: Drop the last Arc clone before try_unwrap
    drop(stats_ref);
    
    let (files, bytes, errors, skipped) = stats.get_summary();
    let elapsed = start_time.elapsed();
    let throughput = if elapsed.as_secs() > 0 {
        bytes / elapsed.as_secs()
    } else {
        bytes
    };
    
    info!(
        "Streaming parallel copy completed in {:.2}s: {} files, {} bytes ({}/s), {} errors, {} skipped (from {} tasks)",
        elapsed.as_secs_f64(),
        files,
        bytes,
        format_bytes(throughput),
        errors,
        skipped,
        total_tasks
    );
    
    // Extract CopyStats from Arc for return
    Arc::try_unwrap(stats)
        .map_err(|_| anyhow::anyhow!("Failed to unwrap CopyStats Arc"))
}

/// Stream copy tasks to channel as they are discovered (memory-efficient producer)
/// This replaces collect_copy_tasks for better memory usage and immediate start
fn stream_copy_tasks(
    src: &Path, 
    dst: &Path, 
    root_dev: u64, 
    sender: Sender<CopyTask>,
    stats: &CopyStats,
) -> Result<usize> {
    let mut task_count = 0;
    stream_copy_tasks_recursive(src, dst, src, &sender, root_dev, &mut task_count, stats)?;
    info!("Task producer completed: {} tasks streamed", task_count);
    Ok(task_count)
}

/// Recursive streaming task producer - immediately sends tasks to channel
fn stream_copy_tasks_recursive(
    current_src: &Path,
    current_dst: &Path,
    src_root: &Path,
    sender: &Sender<CopyTask>,
    root_dev: u64,
    task_count: &mut usize,
    stats: &CopyStats,
) -> Result<()> {
    // Skip directories on different devices (mount points)
    if should_skip_directory(current_src, root_dev) {
        debug!("Skipping mounted directory: {}", current_src.display());
        stats.add_skipped(); // Count mounted directories as skipped
        return Ok(());
    }

    let entries = fs::read_dir(current_src)
        .with_context(|| format!("Failed to read directory: {}", current_src.display()))?;

    for entry in entries {
        let entry = entry?;
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = current_dst.join(&file_name);
        
        let metadata = entry.metadata()
            .with_context(|| format!("Failed to get metadata for: {}", src_path.display()))?;

        if metadata.is_dir() {
            // Recursively stream tasks from subdirectory
            stream_copy_tasks_recursive(&src_path, &dst_path, src_root, sender, root_dev, task_count, stats)?;
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            // Create relative path for logging
            let relative_path = src_path.strip_prefix(src_root)
                .unwrap_or(&src_path)
                .to_path_buf();
            
            let task = CopyTask {
                src: src_path.clone(),
                dst: dst_path.clone(),
                relative_path,
            };
            
            // Send task to workers immediately
            sender.send(task)
                .map_err(|_| anyhow::anyhow!("Failed to send copy task - channel closed"))?;
            
            *task_count += 1;
            
            // Log progress for large directory trees
            if *task_count % 10000 == 0 {
                debug!("Queued {} copy tasks...", task_count);
            }
        } else {
            // Handle special files (devices, sockets, FIFOs, etc.) - track as skipped
            let file_type = metadata.file_type();
            
            // Use Unix-specific file type checking (requires std::os::unix)
            #[cfg(unix)]
            let type_description = {
                use std::os::unix::fs::FileTypeExt;
                if file_type.is_block_device() {
                    "block device"
                } else if file_type.is_char_device() {
                    "character device"
                } else if file_type.is_fifo() {
                    "FIFO/pipe"
                } else if file_type.is_socket() {
                    "socket"
                } else {
                    "unknown special file"
                }
            };
            
            #[cfg(not(unix))]
            let type_description = "special file";
            
            if *task_count < 100 {
                info!("Skipping {}: {}", type_description, src_path.display());
            } else {
                debug!("Skipping {}: {}", type_description, src_path.display());
            }
            
            stats.add_skipped(); // Properly account for skipped special files
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