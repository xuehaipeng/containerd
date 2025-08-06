# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Development Commands

### Building and Development

Remember that this computer is a Macbook with M3 chip (arm64 based), and we will deploy the compiled customized containerd to a remote Ubuntu 22.04 Linux with x86_64 architecture, the remote host can be accessed by `ssh -p 65056 root@10.8.20.1 <COMMAND_TO_EXEC>`

```bash
# Build all binaries
export GOOS=linux
export GOARCH=amd64
make binaries

# Build with static linking
make STATIC=1 binaries
```

### Testing
```bash
# Run unit tests (non-root)
make test

# Run root-requiring tests
make root-test

# Run integration tests
make integration

# Run CRI integration tests
make cri-integration

# Run specific test with root privileges
sudo go test -v -run "TestName" ./path/to/package -test.root

# Run tests with coverage
make coverage

# Clean up test debris
make clean-test
```

### Development Setup
```bash
# Install development tools
script/setup/install-dev-tools

# Install dependencies for CRI development
make install-deps

# Install additional CRI tools
make install-critools

# Clean build artifacts
make clean
```

## Architecture Overview

### Core Components Architecture

Containerd follows a **layered plugin architecture** with these key layers:

1. **Client Layer** (`client/`): High-level APIs for container operations
2. **Core Services** (`core/`): Service interfaces (content, snapshots, runtime, containers, images)
3. **Plugin Layer** (`plugins/`): Service implementations and extensions
4. **API Layer** (`api/`): gRPC/TTRPC protocol definitions
5. **Runtime Layer** (`core/runtime/`): Container lifecycle management

### Plugin System Patterns

All plugins follow this registration pattern:
```go
func init() {
    registry.Register(&plugin.Registration{
        Type: plugins.PluginType,
        ID:   "plugin-id",
        Requires: []plugin.Type{plugins.DependencyPlugin},
        InitFn: func(ic *plugin.InitContext) (interface{}, error) {
            // Plugin initialization
            return implementation, nil
        },
    })
}
```

### Key Service Interfaces

- **Content Service** (`core/content/`): Content-addressable storage with Provider/Ingester/Manager interfaces
- **Snapshots Service** (`core/snapshots/`): Layered filesystem management with Snapshotter interface
- **Container Service** (`core/containers/`): Container metadata management with Store interface
- **Runtime Service** (`core/runtime/`): Task creation and process lifecycle management
- **Image Service** (`core/images/`): Image metadata and manipulation
- **Transfer Service** (`core/transfer/`): Image pull/push operations with registry integration

### Critical Operation Flows

**Image Pull**: Client → Transfer service → Remote resolver → Content store → Image store → Snapshotter

**Container Creation**: Client → Container store → Snapshotter (prepare layer) → Runtime plugin

**Task Execution**: Container → Runtime plugin → Shim process → Runtime (runc) → Event system

### Directory Structure

- `/cmd/`: Main binaries (`containerd`, `ctr`, `containerd-shim-runc-v2`)
- `/core/`: Core service interfaces and implementations
- `/plugins/`: Plugin implementations (content, snapshots, services, CRI)
- `/client/`: Client library and high-level APIs
- `/api/`: Protocol buffer definitions for services and types
- `/internal/`: Internal utilities and helpers (not for external use)
- `/pkg/`: Reusable packages for external consumption

### Key Files for Plugin Development

- `plugins/types.go`: Plugin type definitions
- `plugins/*/plugin.go`: Plugin registration and initialization
- `core/*/interface.go`: Core service interfaces
- `cmd/containerd/builtins/`: Built-in plugin registrations

### Build Tags

Common build tags to be aware of:
- `no_cri`: Disable CRI plugin
- `no_btrfs`: Disable Btrfs snapshotter
- `no_devmapper`: Disable device mapper snapshotter
- `no_zfs`: Disable ZFS snapshotter
- `no_systemd`: Disable systemd integration

### Development Guidelines

- Follow standard Go formatting (`make check` enforces this)
- Use tabs for protobuf file indentation
- Generate protobuf files with `make protos` after changes
- Plugin implementations should be in `/plugins/` directory
- Core interfaces should be in `/core/` directory
- Use the plugin registry pattern for extensibility
- Event-driven architecture for state changes and monitoring

### Testing Guidelines

- Unit tests should not require root privileges unless testing root-specific functionality
- Integration tests should use the `/integration/` directory
- CRI-specific tests are in `/plugins/cri/` and run with `make cri-integration`
- Use `testutil.RequiresRoot` for tests requiring root privileges
- Test containers and cleanup are handled by the test framework

## Current Branch Context: `short-path-fix`

This branch focuses on optimizing shared snapshots and path mapping for containerd. Recent changes include:

### ✅ Completed Features:

1. **Shared Storage Support**: Enhanced the overlay snapshotter to allocate upperdir on shared storage while keeping lowerdir on local filesystems for containers matching specific conditions defined in /etc/containerd/config.toml file.

2. **Hash-based Path Optimization**: Implemented hash-based directory paths to significantly reduce mount option lengths:
   - Pod identifiers are hashed to create short directory names (8 characters)
   - Snapshot IDs are hashed for unique path generation
   - Maintains path uniqueness while drastically reducing length
   - Successfully tested with containers having 50+ layers

3. **Path Mapping System**: Created a comprehensive path mapping system (`path_mapping.go`) that:
   - Maps hash-based paths back to original Kubernetes identifiers
   - Persists mappings to JSON files for debugging and recovery
   - Provides lookup functionality for troubleshooting
   - Verified working: Path mappings correctly stored in `/s/.path-mappings.json`

4. **Short Path Structure**: Uses `/s/l/` for snapshot paths instead of long containerd paths:
   - Short paths: `/s/l/143/fs` instead of `/s/d/io.containerd.snapshotter.v1.overlayfs/snapshots/143/fs`
   - Shared storage: `/s/{pod_hash}/{snapshot_hash}/fs` for container state

5. **Robust Parent Path Resolution**: Handles transitions between path configurations:
   - Tries current path method first, then fallback to opposite method
   - Handles existing snapshots during configuration changes

6. **Configuration Support**: Added plugin configuration options:
   - `short_base_paths` configuration flag
   - Integration with existing overlay plugin architecture
   - Backward compatibility with existing configurations

7. **Automatic Session Backup Directory Creation**: Integrated automatic creation of session backup directories:
   - New config option: `session_backup_base_path` in CRI runtime configuration
   - Automatically creates directory structure: `{session_backup_base_path}/{namespace}/{pod_name}/{container_name}`
   - Only applies to containers using shared snapshots that match namespace/pod name regex patterns
   - Seamlessly integrates with existing Rust-based session backup/restore system
   - Handles pod restarts gracefully - preserves existing backup directories and session data

### 🔧 Technical Implementation:

- **Files Modified**: `plugins/snapshots/overlay/overlay.go`, `plugins/snapshots/overlay/plugin/plugin.go`, `internal/cri/config/config.go`, `internal/cri/server/container_create.go`
- **Files Added**: `plugins/snapshots/overlay/path_mapping.go`
- **Key Functions**: `getSharedPathBase()`, `hashString()`, `determineUpperPath()`, `determineWorkPath()`
- **Path Strategy**: Uses hash-based short directory names and proper shared storage base calculation
- **Session Backup Integration**: Automatic directory creation in `container_create.go` using `os.MkdirAll()` for idempotent operation

### 🛡️ Critical Safeguards:

The implementation includes several critical safeguards to prevent accidental data loss:

1. **Short Paths Protection**: The `/s/l` directory containing layer snapshots is protected from accidental deletion during cleanup operations
2. **Path Conflict Detection**: Prevents conflicts between shared storage paths and containerd's internal structure
3. **Migration Safety**: Graceful handling of transitions between path configurations with fallback mechanisms
4. **Atomic Operations**: Uses atomic moves and temporary files for critical filesystem operations

### 📋 Configuration Example:

To enable short base paths, add the following to your containerd configuration:

```toml
[plugins."io.containerd.snapshotter.v1.overlayfs"]
  short_base_paths = true
```

For shared storage snapshots and automatic session backup directory creation:

```toml
[plugins."io.containerd.cri.v1.runtime"]
  # Shared snapshot configuration
  shared_snapshot_path = "/shared/nb"
  shared_snapshot_namespace_regex = "kubecube-workspace-.*"
  shared_snapshot_pod_name_regex = "^nb-.*"
  
  # Automatic session backup directory creation
  session_backup_base_path = "/tecofs/nb-sessions"
```

For containers using shared snapshots, they must be labeled with:
```toml
labels = [
  "containerd.io/snapshot/use-shared-storage=true",
  "containerd.io/snapshot/shared-disk-path=/shared/storage/path",
  "containerd.io/snapshot/k8s-namespace=default",
  "containerd.io/snapshot/k8s-pod-name=my-pod",
  "containerd.io/snapshot/k8s-container-name=my-container"
]
```

When both shared snapshots and session backup are configured, backup directories are automatically created as:
`/tecofs/nb-sessions/{namespace}/{pod_name}/{container_name}`

**Pod Restart Behavior**: The automatic directory creation uses `os.MkdirAll()` which gracefully handles existing directories, preserving any previous session data during pod restarts.

## Session Backup and Restore Functionality

### Architecture Overview

The session management functionality has been completely rewritten in **Rust** to provide a robust, production-ready solution for session backup and restore in containerd environments. The new architecture addresses all previous issues with shell scripts and introduces advanced features:

1. **Local Session Storage**: Container session data (upperdir) is stored on local filesystems with XFS project quotas for ephemeral storage limiting (handled by the image-server project)

2. **Path Mappings**: JSON file that maps container identifiers (namespace/pod_name/container_name) to session directories using hash-based paths ({pod_hash}/{snapshot_hash})

3. **Backup Storage**: Shared storage with simple directory structure ({namespace}/{pod_name}/{container_name}) used for backup/restore operations

4. **Kubernetes Lifecycle Hooks**: 
   - **postStart Hook**: Restores session data using **direct container root restoration**
   - **preStop Hook**: Backs up session data from local storage to shared backup storage

### Implementation

The implementation has been completely rewritten in **Rust** with the following components:

#### 1. **Rust Binaries** (`session-manager/`)

**Project Structure**:
```
session-manager/
├── src/
│   ├── lib.rs                    # Core library with common functions
│   ├── direct_restore.rs         # DirectRestoreEngine library module
│   ├── session-backup.rs         # Session backup binary
│   └── session-restore.rs        # Session restore binary (direct approach)
├── Cargo.toml                    # Dependencies and binary configuration
└── build-compatible.sh           # Build script for musl static binaries
```

**Key Dependencies**:
- **Core**: `anyhow`, `clap`, `log`, `env_logger`, `serde`, `chrono`
- **Async/Performance**: `tokio`, `rayon`, `futures`, `async-stream`
- **Optimization**: `blake3`, `lru`, `memmap2`, `parking_lot`, `once_cell`
- **File Operations**: `walkdir`, `which`, `filetime`, `tempfile`

#### 2. **Session Backup Binary** (`session-backup.rs`)

**Features**:
- **Optimized Async Operations**: Uses Tokio runtime for async file operations
- **Mount Bypass**: Automatically detects and excludes mounted paths during backup
- **Multiple Transfer Methods**: rsync (primary) with tar fallback
- **Comprehensive Error Handling**: Graceful handling of busy/read-only files
- **Performance Optimizations**: Parallel processing and resource management

**Key Capabilities**:
```rust
// Enhanced backup with mount bypass
transfer_data_with_mount_bypass(&source, &target, timeout, bypass_mounts)

// Async session discovery
find_current_session_async(&mappings_file, &pod_info).await
```

#### 3. **Session Restore Binary** (`session-restore.rs`)

**Revolutionary Direct Container Root Restoration**:
- **Direct Restoration**: Files are restored directly to container filesystem paths (`/root`, `/home`, etc.)
- **No OverlayFS Dependencies**: Eliminates timing issues with OverlayFS mounting
- **Automatic Cleanup**: Successfully restored files are automatically cleaned from backup storage
- **Robust Error Handling**: Gracefully skips busy/read-only files with detailed logging

**Key Features**:
```rust
pub struct DirectRestoreEngine {
    pub dry_run: bool,
    pub timeout: u64,
    pub max_retries: u32,
    pub retry_delay: Duration,
}

// Direct container root restoration
restore_engine.restore_to_container_root(&backup_path)
```

#### 4. **Core Library** (`lib.rs`)

**Advanced Features**:
- **LRU Caching**: Global LRU cache for path mappings with 1000-entry capacity
- **Resource Management**: Thread pools for I/O and compute operations
- **Optimized File Operations**: Memory-mapped files, Blake3 hashing, parallel processing
- **Mount Detection**: Automatic detection and handling of mounted filesystems
- **Security Validation**: Path traversal protection and security checks

**Performance Optimizations**:
```rust
// Global LRU cache for path mappings
static PATH_MAPPING_CACHE: Lazy<Arc<RwLock<LruCache<String, PathMapping>>>>

// Parallel file operations
transfer_data_parallel(source, target, timeout).await

// Optimized file integrity verification
verify_file_integrity(file1, file2)
```

#### 5. **DirectRestoreEngine** (`direct_restore.rs`)

**Advanced Restoration Features**:
- **Parallel Processing**: Uses Rayon for parallel file processing
- **Comprehensive Validation**: Pre-cleanup validation with rollback capability
- **Batch Operations**: Batch cleanup with automatic rollback on failure
- **Safety Checks**: Disk space validation, content verification, system file detection
- **Retry Logic**: Configurable retry mechanisms for transient errors

**Error Classification**:
```rust
pub enum CopyResult {
    Success,
    Skipped(String),    // File busy, read-only, permission denied
    Failed(String),     // Unrecoverable errors
}
```

### Building and Deployment

#### **Build Commands**:
```bash
# Standard build
cd session-manager
cargo build --release

# Static musl build (recommended for containers)
./build-compatible.sh

# Binaries created at:
# - target/release/session-backup
# - target/release/session-restore
# - target/compatible/session-backup (static)
# - target/compatible/session-restore (static)
```

#### **Container Integration**:
```dockerfile
# Copy static binaries (no GLIBC dependencies)
COPY session-manager/target/compatible/session-backup /usr/local/bin/
COPY session-manager/target/compatible/session-restore /usr/local/bin/
RUN chmod +x /usr/local/bin/session-backup /usr/local/bin/session-restore
```

### Usage Examples

#### **Session Backup** (preStop hook):
```bash
./session-backup \
  --mappings-file /etc/path-mappings.json \
  --sessions-path /etc/sessions \
  --backup-path /etc/backup \
  --namespace default \
  --pod-name nb-test-0 \
  --container-name inference \
  --timeout 900 \
  --bypass-mounts true
```

#### **Session Restore** (postStart hook):
```bash
./session-restore \
  --backup-path /etc/backup \
  --timeout 900 \
  --dry-run false
```

### Configuration

The Rust implementation supports extensive configuration:

- **Command-line arguments**: Full CLI with help and validation
- **Environment variables**: Automatic fallback for container environments
- **Timeout configuration**: Configurable timeouts for all operations
- **Logging levels**: Debug, info, warn, error with file-based logging
- **Dry-run mode**: Safe testing without actual file operations

### Monitoring and Observability

- **Detailed Metrics**: File counts, success rates, error details, operation duration
- **Comprehensive Logging**: All operations logged to `/tmp/session-{backup|restore}-{timestamp}.log`
- **Error Classification**: Detailed categorization of skipped vs failed operations
- **Performance Tracking**: Operation timing and resource usage

### Volume Mounts Required

The Rust binaries require the following volume mounts in Kubernetes:

- **Path mappings**: `/etc/path-mappings.json` (read-only)
- **Local sessions**: `/etc/sessions` mapped from `/shared/nb`
- **Backup storage**: `/etc/backup` mapped from `/tecofs/nb-sessions/{namespace}/{pod_name}/{container_name}`

### Testing and Validation

The implementation includes comprehensive testing:

- **Unit tests**: Individual function testing with `cargo test`
- **Integration tests**: End-to-end workflow testing
- **Performance benchmarks**: Operation timing and resource usage
- **Error simulation**: Testing with busy files, read-only filesystems, permission issues
