use anyhow::Result;
use session_manager::direct_restore::DirectRestoreEngine;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

/// Integration test for backup cleanup validation functionality
fn main() -> Result<()> {
    env_logger::init();
    
    println!("Testing backup cleanup validation functionality...");
    
    // Create temporary directory for testing
    let temp_dir = TempDir::new()?;
    let backup_dir = temp_dir.path().join("backup");
    let target_dir = temp_dir.path().join("target");
    
    fs::create_dir_all(&backup_dir)?;
    fs::create_dir_all(&target_dir)?;
    
    // Create test files
    let test_files = vec![
        ("file1.txt", "Content of file 1"),
        ("file2.txt", "Content of file 2"),
        ("file3.txt", "Content of file 3"),
    ];
    
    let mut backup_files = Vec::new();
    let mut target_files = Vec::new();
    
    for (filename, content) in &test_files {
        let backup_file = backup_dir.join(filename);
        let target_file = target_dir.join(filename);
        
        // Create backup file
        File::create(&backup_file)?.write_all(content.as_bytes())?;
        
        // Create matching target file
        File::create(&target_file)?.write_all(content.as_bytes())?;
        
        backup_files.push(backup_file);
        target_files.push(target_file);
    }
    
    // Test the backup cleanup validation
    let engine = DirectRestoreEngine::new(false, 300);
    
    println!("Running comprehensive backup cleanup validation...");
    let validation_result = engine.validate_backup_cleanup_safety(&backup_files, &target_files)?;
    
    println!("Validation Results:");
    println!("  Total files: {}", validation_result.total_files);
    println!("  Validated files: {}", validation_result.validated_files);
    println!("  Failed validations: {}", validation_result.failed_validations.len());
    println!("  Safety warnings: {}", validation_result.safety_warnings.len());
    
    if !validation_result.failed_validations.is_empty() {
        println!("Validation failures:");
        for failure in &validation_result.failed_validations {
            println!("  - {}: {}", failure.backup_file.display(), failure.error);
        }
    }
    
    if !validation_result.safety_warnings.is_empty() {
        println!("Safety warnings:");
        for warning in &validation_result.safety_warnings {
            println!("  - {} ({}): {}", warning.file_path.display(), warning.severity, warning.message);
        }
    }
    
    // Test batch cleanup with rollback
    println!("\nTesting batch cleanup with rollback capability...");
    let cleanup_result = engine.cleanup_backup_files_with_rollback(&backup_files, &target_files)?;
    
    println!("Cleanup Results:");
    println!("  Total files: {}", cleanup_result.total_files);
    println!("  Successful cleanups: {}", cleanup_result.successful_cleanups);
    println!("  Failed cleanups: {}", cleanup_result.failed_cleanups);
    println!("  Rollback operations: {}", cleanup_result.rollback_operations);
    
    for detail in &cleanup_result.cleanup_details {
        println!("  - {}: {} ({})", detail.backup_file.display(), detail.status, detail.message);
    }
    
    // Verify that backup files were cleaned up
    println!("\nVerifying cleanup results...");
    for backup_file in &backup_files {
        if backup_file.exists() {
            println!("  WARNING: Backup file still exists: {}", backup_file.display());
        } else {
            println!("  ✓ Backup file successfully cleaned: {}", backup_file.display());
        }
    }
    
    // Verify that target files still exist
    for target_file in &target_files {
        if target_file.exists() {
            println!("  ✓ Target file preserved: {}", target_file.display());
        } else {
            println!("  ERROR: Target file missing: {}", target_file.display());
        }
    }
    
    println!("\nBackup cleanup validation test completed successfully!");
    Ok(())
}