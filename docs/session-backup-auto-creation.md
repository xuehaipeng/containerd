# Automatic Session Backup Directory Creation Feature

This document explains how to configure and use the automatic session backup directory creation feature in containerd.

## Overview

The session backup directory creation feature automatically creates backup directories for containers that use shared snapshots. This eliminates the need to manually create backup directory structures for session backup/restore functionality.

## Configuration

Add the `session_backup_base_path` setting to your containerd configuration file:

```toml
[plugins."io.containerd.cri.v1.runtime"]
  # Existing shared snapshot configuration
  shared_snapshot_path = "/shared/nb"
  shared_snapshot_namespace_regex = "kubecube-workspace-.*"
  shared_snapshot_pod_name_regex = "^nb-.*"
  
  # NEW: Automatic session backup directory creation
  session_backup_base_path = "/tecofs/nb-sessions"
```

## How It Works

When a container is created that:
1. Uses shared snapshots (has `containerd.io/snapshot/use-shared-storage=true` label)
2. Matches the configured namespace and pod name regex patterns
3. Has all required Kubernetes metadata (namespace, pod name, container name)

The system will automatically create a backup directory with this structure:
```
{session_backup_base_path}/{namespace}/{pod_name}/{container_name}
```

## Example

For a container with:
- **Namespace**: `kubecube-workspace-214`
- **Pod Name**: `nb-test-0`
- **Container Name**: `inference`
- **Config Setting**: `session_backup_base_path = "/tecofs/nb-sessions"`

The following directory will be automatically created:
```
/tecofs/nb-sessions/kubecube-workspace-214/nb-test-0/inference
```

## Integration with Session Management

This feature works seamlessly with the existing Rust-based session backup/restore system:

1. **Container Creation**: Backup directory is automatically created
2. **postStart Hook**: Session restore reads from the backup directory
3. **preStop Hook**: Session backup writes to the backup directory
4. **Directory Cleanup**: Managed externally (not handled by containerd)

## Kubernetes Volume Mount Example

The automatically created directories can be mounted into containers:

```yaml
spec:
  containers:
  - name: inference
    volumeMounts:
    - name: backup-storage
      mountPath: /etc/backup
      readOnly: false
  volumes:
  - name: backup-storage
    hostPath:
      path: /tecofs/nb-sessions/kubecube-workspace-214/nb-test-0/inference
      type: Directory  # This directory will exist automatically
```

## Error Handling

- **Directory Creation Failure**: Logged as warning, container creation continues
- **Missing Config**: Feature disabled silently if `session_backup_base_path` is not set
- **Invalid Permissions**: Standard filesystem permission errors apply

## Logging

The feature provides detailed logging:

```
INFO: Created session backup directory for container <container_id>: <backup_path>
WARN: Failed to create session backup directory: <backup_path>
```

## Backward Compatibility

- **Existing Configurations**: No changes required, feature is optional
- **Manual Directory Creation**: Still supported, automatic creation skips existing directories
- **Shared Snapshots**: Existing shared snapshot functionality unchanged

## Security Considerations

- **Directory Permissions**: Created with 0755 permissions (readable by all, writable by owner)
- **Path Validation**: Uses standard filepath operations, no additional validation
- **Access Control**: Relies on filesystem permissions and mount configurations