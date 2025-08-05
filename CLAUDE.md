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

### 🔧 Technical Implementation:

- **Files Modified**: `plugins/snapshots/overlay/overlay.go`, `plugins/snapshots/overlay/plugin/plugin.go`
- **Files Added**: `plugins/snapshots/overlay/path_mapping.go`
- **Key Functions**: `getSharedPathBase()`, `hashString()`, `determineUpperPath()`, `determineWorkPath()`
- **Path Strategy**: Uses hash-based short directory names and proper shared storage base calculation

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

For shared storage snapshots, containers must be labeled with:
```toml
labels = [
  "containerd.io/snapshot/use-shared-storage=true",
  "containerd.io/snapshot/shared-disk-path=/shared/storage/path",
  "containerd.io/snapshot/k8s-namespace=default",
  "containerd.io/snapshot/k8s-pod-name=my-pod",
  "containerd.io/snapshot/k8s-container-name=my-container"
]
```

## Session Backup and Restore Functionality

### Architecture Overview

The session management functionality has been refactored to address limitations with OverlayFS on shared storage systems. The new architecture works as follows:

1. **Local Session Storage**: Container session data (upperdir) is stored on local filesystems with XFS project quotas for ephemeral storage limiting (handled by the image-server project)

2. **Path Mappings**: JSON file that maps container identifiers (namespace/pod_name/container_name) to session directories using hash-based paths ({pod_hash}/{snapshot_hash})

3. **Backup Storage**: Shared storage with simple directory structure ({namespace}/{pod_name}/{container_name}) used only for backup/restore operations

4. **Kubernetes Lifecycle Hooks**: 
   - **postStart Hook**: Restores session data from shared backup storage to local storage
   - **preStop Hook**: Backs up session data from local storage to shared backup storage

### Implementation

The refactoring has been completed with the following components:

1. **Enhanced Shell Scripts**: 
   - `session-backup.sh`: Backup script for postStart hook with JSON parsing and robust file handling
   - `session-restore.sh`: Restore script for preStop hook with JSON parsing and robust file handling

2. **Kubernetes Configuration**: Updated StatefulSet specifications with proper volume mounts for path mappings, local sessions, and backup storage

3. **JSON Parsing**: Scripts use `jq` to parse path mappings and identify the correct session directory based on namespace/pod_name/container_name

4. **Robust File Handling**: Scripts handle all file types including hidden files, large files, and empty directories using rsync with appropriate options

### Key Features

1. **Precise Session Identification**: Scripts parse JSON path mappings to find the current session by identifying the newest entry (by created_at timestamp) for the specific container

2. **Complete File Handling**: 
   - Uses rsync with options to handle all file types
   - Fallback to tar if rsync is not available
   - Proper handling of read-only files (warnings but no failures)
   - Support for hidden files, large files, and empty directories

3. **Error Handling**: 
   - Comprehensive logging
   - Timeout support
   - Graceful handling of partial failures
   - Dry-run mode for testing

4. **Flexible Configuration**: All paths and parameters are configurable via command-line arguments

### Usage

The hook scripts can be used in Kubernetes StatefulSet configurations as shown in `test/test-session-backup-restore.yaml`. The scripts require the following volume mounts:

- Path mappings file (`/etc/path-mappings.json`)
- Local session directories (`/etc/sessions` mapped from `/shared/nb`)
- Backup storage directory (`/etc/backup` mapped from `/tecofs/nb-sessions/{namespace}/{pod_name}/{container_name}`)

See `README.session-backup-restore.md` for detailed documentation and usage examples.