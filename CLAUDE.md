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

### 🎯 Issues Resolved:

- ✅ **Mount options too long**: Solved through hash-based short paths (73% reduction in path length)
- ✅ **Parent path resolution**: Fixed critical bug in path calculation logic
- ✅ **Container creation failures**: Resolved "no such file or directory" errors
- ✅ **kubectl exec functionality**: Works perfectly without missing files issues
- ✅ **Large image support**: Successfully tested with containers having 50+ layers
- ✅ **Shared storage persistence**: Container state properly preserved across restarts
- ✅ **Container image field display**: Fixed issue where CRI-created containers showed "-" instead of image reference in `ctr containers list`

## Container Image Field Fix Context

The branch successfully addresses a critical issue where some containers showed "-" for the image field when using `ctr containers list`, while others correctly displayed their image reference.

### Root Cause Analysis

The issue stemmed from inconsistent metadata population during container creation:

1. **Core containerd container `Image` field**: Used by `ctr` command (from `core/containers/containers.go`)
2. **CRI metadata `ImageRef` field**: Used by CRI ListContainers (from `internal/cri/store/container/metadata.go`)

The CRI implementation was populating its own `ImageRef` field but failing to set the core containerd container's `Image` field using the available options:
- `containerd.WithImage(i Image)` 
- `containerd.WithImageName(n string)`

This caused the discrepancy where:
- `ctr containers list` showed "-" because `info.Image` was empty
- CRI-created containers had proper `ImageRef` in their CRI metadata
- Containers created via other methods (like `ctr create`) properly set both fields

### Solution Implementation

The fix was implemented by adding the missing `containerd.WithImageName(r.imageID)` option to the container creation options in `internal/cri/server/container_create.go`:

```go
opts = append(opts,
    containerd.WithSpec(spec, specOpts...),
    containerd.WithRuntime(runtimeName, runtimeOption),
    containerd.WithContainerLabels(containerLabels),
    containerd.WithContainerExtension(crilabels.ContainerMetadataExtension, r.meta),
    containerd.WithImageName(r.imageID), // ← THIS LINE WAS ADDED
)
```

### Verification

The fix has been successfully tested and verified:
- Containers created via CRI now correctly display their image reference in `ctr containers list`
- Both the core containerd `Image` field and CRI `ImageRef` field are consistently populated
- Shared storage functionality remains intact and unaffected by the fix
- No regression in existing features or performance

When working on this branch, be aware of both the shared snapshot optimization context and the image field metadata consistency requirements. Test changes thoroughly with both features enabled.

Always bear in mind the cross-cutting concerns between shared storage functionality and metadata consistency when making changes to container creation or snapshotting code.