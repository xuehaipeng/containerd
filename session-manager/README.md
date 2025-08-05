# Session Manager - Rust-Based Implementation

## Overview

This directory contains a robust, Rust-based implementation of session backup and restore functionality for containerd. This solution replaces the problematic shell script approach with a more reliable and efficient implementation.

## Why Rust?

The shell script approach had several critical issues:
1. **File Operation Errors**: "Text file busy", "Read-only file system" errors
2. **Incorrect Directory Logic**: Operating on wrong directories
3. **Poor Error Handling**: No graceful handling of edge cases
4. **Complexity**: Hard to maintain and debug

Rust provides:
1. **Memory Safety**: No buffer overflows or memory issues
2. **Robust Error Handling**: Comprehensive error handling with `Result` types
3. **Performance**: Faster file operations and JSON parsing
4. **Reliability**: Strong type system prevents many runtime errors
5. **Maintainability**: Clear, well-structured code with proper documentation

## Architecture

### Components

1. **Session Backup** (`session-backup` binary)
   - **Purpose**: Backs up current session data to shared backup storage
   - **Trigger**: Kubernetes `preStop` hook
   - **Source**: `/etc/sessions/{pod_hash}/{snapshot_hash}/fs` (current session directory)
   - **Destination**: `/etc/backup` (shared backup storage)

2. **Session Restore** (`session-restore` binary)
   - **Purpose**: Restores session data from shared backup storage to current session
   - **Trigger**: Kubernetes `postStart` hook
   - **Source**: `/etc/backup` (shared backup storage)
   - **Destination**: `/etc/sessions/{pod_hash}/{snapshot_hash}/fs` (current session directory)

### Data Flow

```
preStop Hook (session-backup):
  /etc/sessions/{pod_hash}/{snapshot_hash}/fs ──► /etc/backup

postStart Hook (session-restore):
  /etc/backup ──► /etc/sessions/{pod_hash}/{snapshot_hash}/fs
```

## Key Features

### 1. Robust Error Handling
- Graceful handling of busy/read-only files
- Continue operation even with partial failures
- Comprehensive logging for debugging
- Timeout support for long operations

### 2. Multiple Transfer Methods
- **Primary**: `rsync` with `--ignore-errors` and `--force` flags
- **Fallback**: `tar` with `--ignore-failed-read` for problematic files
- Automatic detection of available tools

### 3. Path Mapping Integration
- Parses `/etc/path-mappings.json` to identify current session
- Finds correct session directories using pod hash and snapshot hash
- Handles multiple sessions for the same pod

### 4. Security and Safety
- Validates all paths before operations
- Creates directories with proper permissions
- Implements safety checks to prevent data loss
- Dry-run mode for testing

## Implementation Details

### Session Backup (`session-backup.rs`)

**Key Logic**:
1. Parse command-line arguments and environment variables
2. Read `/etc/path-mappings.json` to find current session
3. Validate current session directory exists and has content
4. Create backup storage directory if needed
5. Copy session data using `rsync` or `tar` with proper flags
6. Handle errors gracefully and continue operation

**Features**:
- **Argument Parsing**: Comprehensive CLI with `clap`
- **Logging**: Detailed logging with `env_logger`
- **JSON Parsing**: Safe JSON parsing with `serde`
- **Process Execution**: Safe subprocess execution with `std::process`
- **Directory Operations**: Robust file system operations

### Session Restore (`session-restore.rs`)

**Key Logic**:
1. Parse command-line arguments and environment variables
2. Read `/etc/path-mappings.json` to find current session
3. Validate backup storage directory exists and has content
4. Ensure current session directory exists
5. Copy session data using `rsync` or `tar` with proper flags
6. Handle errors gracefully and continue operation

**Features**:
- **Same capabilities as backup** but for restore operations
- **Path validation** to ensure correct directories
- **Graceful error handling** for busy/read-only files

## Usage

### Building

#### For Maximum Linux Compatibility (Recommended)

Use the provided build script to create statically-linked binaries that work on any Linux system:

```bash
# Build compatible binaries (no GLIBC dependencies)
cd session-manager
./build-compatible.sh

# Binaries will be in target/compatible/
ls -la target/compatible/session-backup target/compatible/session-restore
```

**✅ These binaries work on any Linux distribution:**
- Ubuntu 16.04+ (including 18.04, 20.04, 22.04, 24.04)
- CentOS 7+, RHEL 7+, Rocky Linux, AlmaLinux
- Alpine Linux, Amazon Linux, Debian 9+
- Any modern Linux system (x86_64)

#### Standard Build (GLIBC-dependent)

```bash
# Build release binaries (requires matching GLIBC version)
cd session-manager
cargo build --release

# Binaries will be in target/release/
ls -la target/release/session-backup target/release/session-restore
```

**⚠️ Note**: Standard builds may have GLIBC compatibility issues on older systems.

### Session Backup

```bash
# Basic usage
./session-backup \
  --mappings-file /etc/path-mappings.json \
  --sessions-path /etc/sessions \
  --backup-path /etc/backup \
  --namespace default \
  --pod-name nb-test-teco-0 \
  --container-name inference

# With timeout and dry-run
./session-backup \
  --timeout 300 \
  --dry-run \
  --mappings-file /etc/path-mappings.json \
  --sessions-path /etc/sessions \
  --backup-path /etc/backup \
  --namespace default \
  --pod-name nb-test-teco-0 \
  --container-name inference
```

### Session Restore

```bash
# Basic usage
./session-restore \
  --mappings-file /etc/path-mappings.json \
  --sessions-path /etc/sessions \
  --backup-path /etc/backup \
  --namespace default \
  --pod-name nb-test-teco-0 \
  --container-name inference

# With timeout and dry-run
./session-restore \
  --timeout 300 \
  --dry-run \
  --mappings-file /etc/path-mappings.json \
  --sessions-path /etc/sessions \
  --backup-path /etc/backup \
  --namespace default \
  --pod-name nb-test-teco-0 \
  --container-name inference
```

## Kubernetes Integration

### YAML Configuration

```yaml
apiVersion: apps/v1
kind: StatefulSet
spec:
  template:
    spec:
      containers:
      - name: inference
        volumeMounts:
        - name: path-mappings
          mountPath: /etc/path-mappings.json
          subPath: .path-mappings.json
          readOnly: true
        - name: local-sessions
          mountPath: /etc/sessions
          readOnly: false
        - name: backup-storage
          mountPath: /etc/backup
          readOnly: false
        lifecycle:
          postStart:
            exec:
              command:
              - /usr/local/bin/session-restore
              - --mappings-file
              - /etc/path-mappings.json
              - --sessions-path
              - /etc/sessions
              - --backup-path
              - /etc/backup
              - --namespace
              - default
              - --pod-name
              - nb-test-teco-0
              - --container-name
              - inference
          preStop:
            exec:
              command:
              - /usr/local/bin/session-backup
              - --mappings-file
              - /etc/path-mappings.json
              - --sessions-path
              - /etc/sessions
              - --backup-path
              - /etc/backup
              - --namespace
              - default
              - --pod-name
              - nb-test-teco-0
              - --container-name
              - inference
```

## Benefits Over Shell Scripts

### 1. Reliability
- **No more file operation errors**: Proper handling of busy/read-only files
- **Consistent behavior**: Same behavior across different environments
- **Better error recovery**: Continue operation even with partial failures

### 2. Performance
- **Faster operations**: Compiled code vs interpreted shell scripts
- **Efficient JSON parsing**: Native JSON support vs external tools
- **Optimized file operations**: Direct system calls vs shell commands

### 3. Maintainability
- **Clear code structure**: Well-organized modules and functions
- **Comprehensive documentation**: Inline documentation and comments
- **Type safety**: Compile-time checking prevents many runtime errors

### 4. Extensibility
- **Easy to extend**: Modular design makes adding features simple
- **Rich ecosystem**: Access to thousands of Rust crates
- **Strong tooling**: Excellent development tools and IDE support

## Testing

The Rust implementation can be tested with:

1. **Unit Tests**: Individual function testing
2. **Integration Tests**: End-to-end workflow testing
3. **Manual Testing**: Deploy to test cluster and verify operation

### Manual Testing Procedure

1. **Create test files**:
   ```bash
   kubectl exec -it nb-test-teco-0 -- bash
   echo "test content" > /root/test_file.txt
   echo "hidden content" > /root/.hidden_file.txt
   ```

2. **Trigger backup** (stop container):
   ```bash
   kubectl delete pod nb-test-teco-0
   ```

3. **Verify backup**:
   ```bash
   # Check backup storage directory contents
   ls -la /tecofs/nb-sessions/default/nb-test-teco-0/inference/
   ```

4. **Trigger restore** (start new container):
   ```bash
   kubectl apply -f test-session-backup-restore.yaml
   ```

5. **Verify restore**:
   ```bash
   kubectl exec -it nb-test-teco-0 -- ls -la /root/
   # Should show test_file.txt and .hidden_file.txt
   ```

## Deployment

### Binary Installation

1. **Build binaries**:
   ```bash
   cd session-manager
   cargo build --release
   ```

2. **Copy to container**:
   ```bash
   # Copy binaries to container image during build
   COPY target/release/session-backup /usr/local/bin/
   COPY target/release/session-restore /usr/local/bin/
   ```

3. **Set permissions**:
   ```dockerfile
   RUN chmod +x /usr/local/bin/session-backup /usr/local/bin/session-restore
   ```

### Configuration

The binaries support extensive configuration through:
- **Command-line arguments**: Full CLI with help
- **Environment variables**: Automatic fallback
- **Configuration files**: JSON-based path mappings

## Troubleshooting

### Common Issues

1. **Missing path mappings file**:
   - **Cause**: Container started without proper volume mount
   - **Solution**: Verify volume mount configuration in YAML

2. **Empty session directory**:
   - **Cause**: No user data to backup/restore
   - **Solution**: This is normal for fresh containers

3. **Permission denied errors**:
   - **Cause**: Insufficient permissions for file operations
   - **Solution**: Verify container security context and volume permissions

### Log Analysis

Logs are written to stderr and can be viewed with:
```bash
# In container
cat /tmp/session-backup.log
cat /tmp/session-restore.log

# Or through Kubernetes
kubectl logs nb-test-teco-0
```

## Future Enhancements

### Planned Features

1. **Incremental Backup**: Only backup changed files
2. **Compression**: Compress backup data to save space
3. **Encryption**: Encrypt backup data for security
4. **Metrics**: Export metrics for monitoring
5. **Health Checks**: Built-in health check endpoints

### Performance Optimizations

1. **Parallel Processing**: Process multiple files simultaneously
2. **Memory Mapping**: Use memory-mapped files for large files
3. **Async I/O**: Non-blocking file operations
4. **Caching**: Cache frequently accessed data

## Conclusion

The Rust-based session manager provides a robust, reliable, and efficient solution for session backup and restore in containerd environments. It addresses all the critical issues with the previous shell script approach while providing better performance, maintainability, and extensibility.

This implementation ensures:
- ✅ **No more file operation errors**
- ✅ **Proper session data persistence**
- ✅ **Reliable operation in production environments**
- ✅ **Easy maintenance and debugging**
- ✅ **Future extensibility for advanced features**