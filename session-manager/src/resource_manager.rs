use anyhow::{Context, Result};
use log::debug;

/// Thread pool manager for concurrent operations
pub struct ThreadPoolManager {
    io_pool: rayon::ThreadPool,
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
        
        debug!("Created I/O thread pool with {} threads", num_cpus * 2);
        
        Ok(Self {
            io_pool,
        })
    }
    
    pub fn io_pool(&self) -> &rayon::ThreadPool {
        &self.io_pool
    }
    
    /// Execute I/O operation in dedicated thread pool
    pub fn execute_io<F, R>(&self, operation: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        self.io_pool.install(operation)
    }
    
    /// Execute compute operation in dedicated thread pool (using same pool for simplicity)
    pub fn execute_compute<F, R>(&self, operation: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        self.io_pool.install(operation)
    }
}

impl Default for ThreadPoolManager {
    fn default() -> Self {
        Self::new().expect("Failed to create default thread pool manager")
    }
}

/// Global resource manager instance
static RESOURCE_MANAGER: once_cell::sync::Lazy<ResourceManager> = 
    once_cell::sync::Lazy::new(ResourceManager::default);

pub struct ResourceManager {
    pub thread_pool: ThreadPoolManager,
}

impl ResourceManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            thread_pool: ThreadPoolManager::new()?,
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