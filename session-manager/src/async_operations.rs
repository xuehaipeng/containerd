use anyhow::{Context, Result};
use std::path::Path;
use std::collections::HashMap;
use tokio::fs;
use log::{debug, info, warn};

use crate::{PathMapping, PathMappings, SessionInfo, PodInfo};

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

    // For very large files, use async JSON parsing
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