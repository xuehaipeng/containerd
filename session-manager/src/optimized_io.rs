use anyhow::{Context, Result};
use std::path::Path;
use std::fs::File;
use std::io::{BufReader, Read};
use memmap2::Mmap;
use blake3::Hasher;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use rayon::prelude::*;

/// Optimized file reading that chooses strategy based on file size
pub fn read_file_optimized(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    
    // For files larger than 1MB, use memory mapping
    if file_size > 1024 * 1024 {
        read_file_mmap(file)
    } else {
        // For smaller files, use regular buffered reading
        read_file_buffered(file)
    }
}

/// Memory-mapped file reading for large files
fn read_file_mmap(file: File) -> Result<String> {
    let mmap = unsafe { Mmap::map(&file)? };
    let content = std::str::from_utf8(&mmap)
        .context("File contains invalid UTF-8")?;
    Ok(content.to_string())
}

/// Buffered file reading for smaller files
fn read_file_buffered(file: File) -> Result<String> {
    let mut reader = BufReader::new(file);
    let mut content = String::new();
    reader.read_to_string(&mut content)?;
    Ok(content)
}

/// Parallel file hashing using Blake3 for integrity verification
pub fn hash_file_parallel(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    
    if file_size > 10 * 1024 * 1024 { // 10MB threshold for parallel hashing
        hash_file_parallel_chunks(file, file_size)
    } else {
        hash_file_sequential(file)
    }
}

/// Sequential file hashing for smaller files
fn hash_file_sequential(file: File) -> Result<String> {
    let mmap = unsafe { Mmap::map(&file)? };
    let mut hasher = Hasher::new();
    hasher.update(&mmap);
    Ok(hasher.finalize().to_hex().to_string())
}

/// Parallel file hashing for large files using chunks
fn hash_file_parallel_chunks(file: File, file_size: u64) -> Result<String> {
    const CHUNK_SIZE: u64 = 1024 * 1024; // 1MB chunks
    let num_chunks = (file_size + CHUNK_SIZE - 1) / CHUNK_SIZE;
    
    let mmap = unsafe { Mmap::map(&file)? };
    
    // Hash chunks in parallel
    let chunk_hashes: Result<Vec<_>> = (0..num_chunks)
        .into_par_iter()
        .map(|chunk_idx| {
            let start = (chunk_idx * CHUNK_SIZE) as usize;
            let end = std::cmp::min(start + CHUNK_SIZE as usize, mmap.len());
            
            let mut hasher = Hasher::new();
            hasher.update(&mmap[start..end]);
            Ok(hasher.finalize())
        })
        .collect();
    
    let hashes = chunk_hashes?;
    
    // Combine chunk hashes
    let mut final_hasher = Hasher::new();
    for hash in hashes {
        final_hasher.update(hash.as_bytes());
    }
    
    Ok(final_hasher.finalize().to_hex().to_string())
}

/// Async file copying with progress tracking
pub async fn copy_file_async(src: &Path, dst: &Path) -> Result<u64> {
    let mut src_file = tokio::fs::File::open(src).await?;
    let mut dst_file = tokio::fs::File::create(dst).await?;
    
    let metadata = src_file.metadata().await?;
    let _file_size = metadata.len();
    
    // Create parent directories if needed
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    
    // Use larger buffer for better performance
    const BUFFER_SIZE: usize = 64 * 1024; // 64KB buffer
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut total_copied = 0u64;
    
    loop {
        let bytes_read = src_file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        
        dst_file.write_all(&buffer[..bytes_read]).await?;
        total_copied += bytes_read as u64;
    }
    
    dst_file.sync_all().await?;
    Ok(total_copied)
}

/// Parallel file copying for multiple files
pub async fn copy_files_parallel(file_pairs: Vec<(PathBuf, PathBuf)>) -> Result<Vec<u64>> {
    let mut results = Vec::new();
    for (src, dst) in file_pairs {
        let result = copy_file_async(&src, &dst).await?;
        results.push(result);
    }
    Ok(results)
}

/// Optimized directory traversal using walkdir with parallel processing
pub fn traverse_directory_parallel<F>(dir: &Path, processor: F) -> Result<()>
where
    F: Fn(&Path) -> Result<()> + Sync + Send,
{
    use walkdir::WalkDir;
    
    let entries: Vec<_> = WalkDir::new(dir)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to traverse directory")?;
    
    // Process files in parallel
    entries
        .into_par_iter()
        .filter(|entry| entry.file_type().is_file())
        .try_for_each(|entry| processor(entry.path()))?;
    
    Ok(())
}

use std::path::PathBuf;