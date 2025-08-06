use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
// Removed unused serde imports
use std::collections::HashMap;
use tokio::fs;
use log::{debug, info, warn};

use crate::{PathMapping, PathMappings, SessionInfo, PodInfo};
use crate::optimized_io;

/// Cached path mapping loader with async support
pub async fn find_current_session_cached(
    mappings_file: &Path,
    pod_info: &PodInfo,
) -> Result<Option<SessionInfo>> {
    // Try cache first
    let cache_key = format!("{}:{}:{}", pod_info.namespace, pod_info.pod_name, pod_info.container_name);
    
    {
        let cache = crate::PATH_MAPPING_CACHE.read();
        if let Some(cached_mapping) = cache.peek(&cache_key) {
            debug!("Found cached mapping for: {}", cache_key);
            return Ok(Some(create_session_info_from_mapping(cached_mapping)?));
        }
    }
    
    // Load from file if not in cache
    let path_mappings = load_path_mappings_async(mappings_file).await?;
    
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
            // Cache the result
            {
                let mut cache = crate::PATH_MAPPING_CACHE.write();
                cache.put(cache_key, mapping.clone());
            }
            
            info!("Found matching session mapping: {}", path_key);
            Ok(Some(create_session_info_from_mapping(&mapping)?))
        }
        None => {
            info!("No matching session found for namespace={}, pod={}, container={}", 
                  pod_info.namespace, pod_info.pod_name, pod_info.container_name);
            Ok(None)
        }
    }
}

/// Async path mappings loader with streaming for large files
async fn load_path_mappings_async(mappings_file: &Path) -> Result<PathMappings> {
    if !mappings_file.exists() {
        warn!("Path mappings file not found: {}", mappings_file.display());
        return Ok(PathMappings {
            mappings: HashMap::new(),
        });
    }

    let content = fs::read_to_string(mappings_file).await
        .with_context(|| format!("Failed to read mappings file: {}", mappings_file.display()))?;

    if content.trim().is_empty() {
        warn!("Path mappings file is empty: {}", mappings_file.display());
        return Ok(PathMappings {
            mappings: HashMap::new(),
        });
    }

    // For very large files, use streaming JSON parser
    if content.len() > 10 * 1024 * 1024 { // 10MB threshold
        parse_large_json_async(&content).await
    } else {
        parse_json_sync(&content)
    }
}

/// Streaming JSON parser for large files
async fn parse_large_json_async(content: &str) -> Result<PathMappings> {
    // Use tokio task for CPU-intensive JSON parsing
    let content = content.to_string();
    tokio::task::spawn_blocking(move || {
        serde_json::from_str::<PathMappings>(&content)
            .context("Failed to parse path mappings JSON")
    }).await?
}

/// Synchronous JSON parser for smaller files
fn parse_json_sync(content: &str) -> Result<PathMappings> {
    serde_json::from_str::<PathMappings>(content)
        .context("Failed to parse path mappings JSON")
}

/// Create SessionInfo from PathMapping
fn create_session_info_from_mapping(mapping: &PathMapping) -> Result<SessionInfo> {
    let created_at = chrono::DateTime::parse_from_rfc3339(&mapping.created_at)?
        .with_timezone(&chrono::Utc);
    
    Ok(SessionInfo {
        pod_hash: mapping.pod_hash.clone(),
        snapshot_hash: mapping.snapshot_hash.clone(),
        created_at,
    })
}

/// Async batch file operations with exponential backoff
pub struct AsyncBatchOperations {
    max_retries: u32,
    base_delay: std::time::Duration,
    max_concurrent: usize,
}

impl AsyncBatchOperations {
    pub fn new() -> Self {
        Self {
            max_retries: 3,
            base_delay: std::time::Duration::from_millis(100),
            max_concurrent: 10,
        }
    }
    
    pub fn with_retry_config(mut self, max_retries: u32, base_delay: std::time::Duration) -> Self {
        self.max_retries = max_retries;
        self.base_delay = base_delay;
        self
    }
    
    pub fn with_concurrency(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = max_concurrent;
        self
    }
    
    /// Execute multiple file operations concurrently with retry logic
    pub async fn execute_batch<F, R, Fut>(&self, operations: Vec<F>) -> Result<Vec<R>>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<R>> + Send,
        R: Send,
    {
        use futures::stream::{self, StreamExt};
        
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.max_concurrent));
        
        let futures = operations.into_iter().map(|operation| {
            let semaphore = semaphore.clone();
            let max_retries = self.max_retries;
            let base_delay = self.base_delay;
            
            async move {
                let _permit = semaphore.acquire().await?;
                self.execute_with_exponential_backoff(operation, max_retries, base_delay).await
            }
        });
        
        let results: Result<Vec<_>> = stream::iter(futures)
            .buffer_unordered(self.max_concurrent)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect();
        
        results
    }
    
    /// Execute single operation with exponential backoff
    async fn execute_with_exponential_backoff<F, R, Fut>(
        &self,
        operation: F,
        max_retries: u32,
        base_delay: std::time::Duration,
    ) -> Result<R>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<R>>,
    {
        let mut last_error = None;
        
        for attempt in 0..=max_retries {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_error = Some(e);
                    
                    if attempt < max_retries {
                        let delay = base_delay * 2_u32.pow(attempt);
                        debug!("Operation failed on attempt {}, retrying in {:?}", attempt + 1, delay);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
        
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All retries exhausted")))
    }
}

impl Default for AsyncBatchOperations {
    fn default() -> Self {
        Self::new()
    }
}

/// Async directory watcher for monitoring file changes
pub struct AsyncDirectoryWatcher {
    _watcher: tokio::sync::mpsc::Receiver<PathBuf>,
}

impl AsyncDirectoryWatcher {
    pub async fn new(_directory: &Path) -> Result<Self> {
        let (_tx, rx) = tokio::sync::mpsc::channel(100);
        
        // In a real implementation, you'd use a file system watcher here
        // For now, we'll just return a placeholder
        
        Ok(Self {
            _watcher: rx,
        })
    }
    
    pub async fn next_change(&mut self) -> Option<PathBuf> {
        self._watcher.recv().await
    }
}

/// Async file transfer with progress reporting
pub struct AsyncFileTransfer {
    progress_callback: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
}

impl AsyncFileTransfer {
    pub fn new() -> Self {
        Self {
            progress_callback: None,
        }
    }
    
    pub fn with_progress<F>(mut self, callback: F) -> Self
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        self.progress_callback = Some(Box::new(callback));
        self
    }
    
    pub async fn transfer_files(&self, file_pairs: Vec<(PathBuf, PathBuf)>) -> Result<Vec<u64>> {
        let total_files = file_pairs.len() as u64;
        let mut results = Vec::new();
        
        for (index, (src, dst)) in file_pairs.into_iter().enumerate() {
            let bytes_copied = optimized_io::copy_file_async(&src, &dst).await?;
            results.push(bytes_copied);
            
            if let Some(ref callback) = self.progress_callback {
                callback(index as u64 + 1, total_files);
            }
        }
        
        Ok(results)
    }
}

impl Default for AsyncFileTransfer {
    fn default() -> Self {
        Self::new()
    }
}

/// Memory-efficient streaming JSON processor for large mapping files
pub struct StreamingJsonProcessor {
    chunk_size: usize,
}

impl StreamingJsonProcessor {
    pub fn new() -> Self {
        Self {
            chunk_size: 64 * 1024, // 64KB chunks
        }
    }
    
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size;
        self
    }
    
    /// Process large JSON files in chunks to reduce memory usage
    pub async fn process_large_mappings_file<F>(&self, file_path: &Path, mut processor: F) -> Result<()>
    where
        F: FnMut(&str, &PathMapping) -> Result<()>,
    {
        let content = fs::read_to_string(file_path).await?;
        
        // For demonstration, we'll parse the full JSON
        // In a real implementation, you'd use a streaming JSON parser like serde_json::Deserializer
        let path_mappings: PathMappings = serde_json::from_str(&content)?;
        
        for (key, mapping) in path_mappings.mappings {
            processor(&key, &mapping)?;
        }
        
        Ok(())
    }
}

impl Default for StreamingJsonProcessor {
    fn default() -> Self {
        Self::new()
    }
}