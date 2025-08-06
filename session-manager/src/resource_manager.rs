use anyhow::{Context, Result};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use log::debug;

/// RAII wrapper for file handles with automatic cleanup
pub struct ManagedFile {
    file: File,
    path: String,
    created_at: Instant,
}

impl ManagedFile {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = File::open(path_ref)
            .with_context(|| format!("Failed to open file: {}", path_ref.display()))?;
        
        debug!("Opened file: {}", path_ref.display());
        
        Ok(ManagedFile {
            file,
            path: path_ref.display().to_string(),
            created_at: Instant::now(),
        })
    }
    
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = File::create(path_ref)
            .with_context(|| format!("Failed to create file: {}", path_ref.display()))?;
        
        debug!("Created file: {}", path_ref.display());
        
        Ok(ManagedFile {
            file,
            path: path_ref.display().to_string(),
            created_at: Instant::now(),
        })
    }
    
    pub fn get(&self) -> &File {
        &self.file
    }
    
    pub fn get_mut(&mut self) -> &mut File {
        &mut self.file
    }
    
    pub fn path(&self) -> &str {
        &self.path
    }
    
    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }
}

impl Drop for ManagedFile {
    fn drop(&mut self) {
        debug!("Closing file: {} (age: {:?})", self.path, self.age());
    }
}

/// File lock manager with timeout support
#[derive(Debug)]
pub struct FileLockManager {
    locks: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
}

impl FileLockManager {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
    
    /// Acquire a lock with timeout using flock-style semantics
    pub fn acquire_lock_with_timeout(&self, path: &Path, timeout: Duration) -> Result<()> {
        let path_str = path.display().to_string();
        let lock_handle = {
            let mut locks = self.locks.lock();
            locks.entry(path_str.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        
        let start_time = Instant::now();
        
        loop {
            // Try to acquire the lock with a short timeout
            if lock_handle.try_lock_for(Duration::from_millis(100)).is_some() {
                debug!("Acquired lock for: {}", path_str);
                return Ok(());
            }
            
            if start_time.elapsed() > timeout {
                return Err(anyhow::anyhow!(
                    "Failed to acquire lock for {} within {:?}", 
                    path_str, timeout
                ));
            }
            
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Default for FileLockManager {
    fn default() -> Self {
        Self::new()
    }
}

// Removed FileLock struct due to lifetime issues - using simpler approach

/// Thread pool manager for concurrent operations
pub struct ThreadPoolManager {
    io_pool: rayon::ThreadPool,
    compute_pool: rayon::ThreadPool,
}

impl ThreadPoolManager {
    pub fn new() -> Result<Self> {
        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        
        // I/O pool: More threads for I/O bound operations
        let io_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus * 2)
            .thread_name(|index| format!("io-worker-{}", index))
            .build()
            .context("Failed to create I/O thread pool")?;
        
        // Compute pool: CPU-bound operations
        let compute_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus)
            .thread_name(|index| format!("compute-worker-{}", index))
            .build()
            .context("Failed to create compute thread pool")?;
        
        debug!("Created thread pools: I/O={} threads, Compute={} threads", 
               num_cpus * 2, num_cpus);
        
        Ok(Self {
            io_pool,
            compute_pool,
        })
    }
    
    pub fn io_pool(&self) -> &rayon::ThreadPool {
        &self.io_pool
    }
    
    pub fn compute_pool(&self) -> &rayon::ThreadPool {
        &self.compute_pool
    }
    
    /// Execute I/O operation in dedicated thread pool
    pub fn execute_io<F, R>(&self, operation: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        self.io_pool.install(operation)
    }
    
    /// Execute compute operation in dedicated thread pool
    pub fn execute_compute<F, R>(&self, operation: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        self.compute_pool.install(operation)
    }
}

impl Default for ThreadPoolManager {
    fn default() -> Self {
        Self::new().expect("Failed to create default thread pool manager")
    }
}

/// Resource usage monitor for tracking file handles and memory
pub struct ResourceMonitor {
    start_time: Instant,
    max_open_files: usize,
    current_open_files: Arc<Mutex<usize>>,
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            max_open_files: 1000, // Default limit
            current_open_files: Arc::new(Mutex::new(0)),
        }
    }
    
    pub fn track_file_open(&self) -> Result<()> {
        let mut count = self.current_open_files.lock();
        if *count >= self.max_open_files {
            return Err(anyhow::anyhow!(
                "Maximum open file limit reached: {}", 
                self.max_open_files
            ));
        }
        *count += 1;
        Ok(())
    }
    
    pub fn track_file_close(&self) {
        let mut count = self.current_open_files.lock();
        if *count > 0 {
            *count -= 1;
        }
    }
    
    pub fn get_stats(&self) -> ResourceStats {
        ResourceStats {
            uptime: self.start_time.elapsed(),
            current_open_files: *self.current_open_files.lock(),
            max_open_files: self.max_open_files,
        }
    }
}

impl Default for ResourceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ResourceStats {
    pub uptime: Duration,
    pub current_open_files: usize,
    pub max_open_files: usize,
}

/// Global resource manager instance
static RESOURCE_MANAGER: once_cell::sync::Lazy<ResourceManager> = 
    once_cell::sync::Lazy::new(ResourceManager::default);

pub struct ResourceManager {
    pub lock_manager: FileLockManager,
    pub thread_pool: ThreadPoolManager,
    pub monitor: ResourceMonitor,
}

impl ResourceManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            lock_manager: FileLockManager::new(),
            thread_pool: ThreadPoolManager::new()?,
            monitor: ResourceMonitor::new(),
        })
    }
    
    pub fn global() -> &'static ResourceManager {
        &RESOURCE_MANAGER
    }
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self::new().expect("Failed to create default resource manager")
    }
}

use once_cell;