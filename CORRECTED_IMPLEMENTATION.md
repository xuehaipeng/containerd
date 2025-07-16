# Corrected Implementation - Layer Snapshots Migration

## Problem Summary
The pod was running but `kubectl exec` failed because:
- **Layer snapshots** were in `/s/d/io.containerd.snapshotter.v1.overlayfs/snapshots/` (original location)
- **Mount expected** layer snapshots in `/s/l/` (short paths location)
- **Container upperdir** was correctly created in `/s/4e44d3e8/00328ce5/` (shared storage)

## Root Cause
When `short_base_paths = true` is enabled, ALL snapshots should use short paths (`/s/l/`) for consistency with the mount logic. But existing layer snapshots were created before this setting was enabled, so they remained in the original location.

## Solution: Migration Approach
Instead of using symbolic links (which don't work with overlayfs), implemented **automatic migration** of existing snapshots to the correct location.

### Implementation Details

#### 1. **Migration on Startup** 
- `migrateExistingSnapshots()` runs during snapshotter initialization
- Moves existing snapshots from `/s/d/io.containerd.snapshotter.v1.overlayfs/snapshots/` to `/s/l/`
- Uses `os.Rename()` for atomic move operation
- Logs migration progress for debugging

#### 2. **Snapshot Path Logic**
- **Layer snapshots** (without K8s labels): Created in `/s/l/{snapshot_id}/`
- **Container snapshots** (with shared storage labels): Use `/s/{pod_hash}/{snapshot_hash}/` for upperdir
- **All snapshots**: Use short paths when `shortBasePaths = true`

#### 3. **Shared Storage Conditions**
Based on `/etc/containerd/config.toml`:
```toml
shared_snapshot_path = "/s"
shared_snapshot_namespace_regex = "default"
shared_snapshot_pod_name_regex = "^nb-.*"
```

Pods matching these conditions get:
- `upperdir=/s/{pod_hash}/{snapshot_hash}/fs`
- `workdir=/s/{pod_hash}/{snapshot_hash}/work`

Where:
- `pod_hash` = first 8 chars of SHA256(`namespace/pod_name/container_name`)
- `snapshot_hash` = first 8 chars of SHA256(`snapshot_id`)

## Code Changes

### Modified Functions:
1. **`ensureShortPathsExist()`** - Added migration call
2. **`migrateExistingSnapshots()`** - New function to move existing snapshots
3. **`getSnapshotPath()`** - Added debug logging
4. **Removed symlink approach** - Cleaned up all symlink-related code

### Migration Logic:
```go
// Move snapshots from original to short path
if err := os.Rename(originalPath, shortPath); err != nil {
    log.L.WithError(err).Warnf("Failed to migrate snapshot %s", snapshotID)
} else {
    log.L.Infof("Migrated snapshot %s from %s to %s", snapshotID, originalPath, shortPath)
}
```

## Expected Behavior After Fix

### On Containerd Startup:
1. Migration runs and moves existing snapshots to `/s/l/`
2. Logs show: `Migrated snapshot 273 from /s/d/.../snapshots/273 to /s/l/273`

### Container Mount:
```
lowerdir=/s/l/273/fs:/s/l/271/fs:...:/s/l/67/fs  <- Layer snapshots (migrated)
upperdir=/s/4e44d3e8/00328ce5/fs                 <- Container upperdir (shared storage)
workdir=/s/4e44d3e8/00328ce5/work                <- Container workdir (shared storage)
```

### File System Access:
- Layer snapshots accessible at `/s/l/273/fs`, `/s/l/271/fs`, etc.
- Container writes go to `/s/4e44d3e8/00328ce5/fs`
- `kubectl exec` works correctly with complete filesystem

## Testing Plan

1. **Deploy Updated Binary**
   ```bash
   # The migration will run automatically on startup
   systemctl stop containerd
   cp /tmp/containerd-fixed /usr/bin/containerd
   systemctl start containerd
   ```

2. **Verify Migration**
   ```bash
   # Check migration logs
   journalctl -u containerd -f | grep -i "migrated snapshot"
   
   # Verify snapshots are in correct location
   ls -la /s/l/
   ```

3. **Test Container Functionality**
   ```bash
   # Test container creation and execution
   kubectl apply -f test-pod.yaml
   kubectl exec -it test-pod -- ls /bin
   ```

## Safety Considerations

- **Atomic Operation**: Uses `os.Rename()` which is atomic on same filesystem
- **Idempotent**: Migration skips snapshots already in correct location
- **Non-destructive**: Only moves files, doesn't delete anything
- **Logged**: All migration activity is logged for debugging

## Backward Compatibility

- **Existing containers** continue to work during migration
- **No downtime** required for migration
- **Gradual migration** happens only for snapshots that need it
- **Future snapshots** are created in correct location automatically

This approach ensures that overlayfs can properly access all layer snapshots while maintaining the shared storage functionality for matching pods.