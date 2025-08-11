use std::process::Command;

fn main() {
    // Get git commit hash
    let git_hash = Command::new("git")
        .current_dir("../")  // Go up one directory to containerd root
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Get build timestamp
    let build_time = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    // Get git branch
    let git_branch = Command::new("git")
        .current_dir("../")  // Go up one directory to containerd root
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);
    println!("cargo:rustc-env=GIT_BRANCH={}", git_branch);
    
    // Tell cargo to rerun this script if git HEAD changes
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
}