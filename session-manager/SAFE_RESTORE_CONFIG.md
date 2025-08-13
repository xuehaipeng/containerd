# Safe Session Restore Configuration

## Problem Statement
When containers experience CrashLoopBackoff, the original session-restore implementation could lead to data loss:
- Session files were moved/deleted from backup during restoration
- Crashes during restoration left backup directory partially depleted
- Subsequent restarts had incomplete backup data

## Solution: Skip Cleanup Mode

The session-restore binary now supports a `--skip-cleanup` flag that:
1. **Copies files instead of moving them** - preserves backup data
2. **Never deletes backup files during postStart** - prevents data loss
3. **Only allows cleanup during controlled shutdown** (preStop hook)

## Updated Kubernetes Configuration

### StatefulSet PostStart Hook (Safe Mode)
```yaml
lifecycle:
  postStart:
    exec:
      command:
      - /etc/scripts/session-restore
      - --mappings-file
      - /etc/path-mappings.json
      - --sessions-path
      - /etc/sessions
      - --backup-path
      - /etc/backup
      - --namespace
      - kubecube-workspace-5
      - --pod-name
      - nb-uazjrqhvrb-0
      - --container-name
      - nb-uazjrqhvrb
      - --skip-cleanup    # NEW: Preserve backup files
```

### Main Container Command Synchronization
Update your `notebook_optimize_script.sh` to wait for restoration:

```bash
#!/bin/bash
# Wait for session restoration to complete
echo "Waiting for session restoration..."
while [ ! -f /tmp/session-restore-complete ]; do
  sleep 1
done
echo "Session restoration complete, starting application..."

# Continue with original startup logic
# ...
```

## Behavior Comparison

### Original (Unsafe) Behavior:
- postStart: Move/delete files from backup → Container rootfs
- If crash: Backup directory partially depleted
- Next restart: Less data available for restoration
- Result: **Progressive data loss**

### New Safe Behavior:
- postStart: Copy files from backup → Container rootfs (backup preserved)
- If crash: Backup directory remains intact
- Next restart: Full data still available for restoration
- Result: **No data loss**

## Storage Considerations

Since backup files are no longer deleted during postStart:
- **Storage usage**: Temporarily doubled (backup + restored copies)
- **Cleanup timing**: Only during preStop (controlled shutdown)
- **Trade-off**: Extra storage for data safety

## Migration Guide

1. **Build new binaries**:
   ```bash
   cd session-manager
   cargo build --release
   ```

2. **Deploy binaries to cluster nodes**:
   ```bash
   cp target/release/session-restore /tecofs/nb/scripts/
   cp target/release/session-backup /tecofs/nb/scripts/
   ```

3. **Update StatefulSet** to add `--skip-cleanup` flag

4. **Update main container script** to wait for `/tmp/session-restore-complete`

## Monitoring

Check logs for new behavior:
```bash
# Look for these log messages
"Backup cleanup disabled - files will be preserved for crash recovery"
"Skipping backup cleanup - preserving backup files for crash recovery"
"Skip cleanup enabled - using copy instead of move"
```

## Rollback

To revert to original behavior, simply remove the `--skip-cleanup` flag from the postStart command.

## Future Improvements

Consider implementing:
1. **Deduplication**: Track already-restored files to avoid re-copying
2. **Incremental restore**: Only copy changed files
3. **Backup versioning**: Keep multiple backup generations
4. **Automatic cleanup**: Clean old backups after container stability period