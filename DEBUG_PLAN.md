# Debug Plan for Container Name Resolution Issue

## Current Issue
The pod `nb-test-teco-0` with container `pytorch` is failing with:
- `CreateContainerError` 
- `context deadline exceeded`
- `name "pytorch_nb-test-teco-0_default_..." is reserved`

## Root Cause Analysis
The issue is not actually with container name resolution - the container name `pytorch` is correctly identified. The real issue is that the snapshot creation process is taking too long or getting stuck, likely due to the `/s/l` directory deletion problem we've been fixing.

## What We've Fixed
1. **Path Conflict Resolution**: Fixed the conflict between shared storage snapshots and layer snapshots
2. **Directory Protection**: Added comprehensive protection for the `/s/l` directory
3. **Enhanced Logging**: Added debug logging to track snapshot creation process

## Next Steps

### 1. Deploy the Fixed Binary
The current code has the fix but needs to be deployed:
```bash
# Build the fixed binary
export GOOS=linux
export GOARCH=amd64
make binaries

# Deploy to remote server
scp bin/containerd root@10.8.20.220:/tmp/containerd-fixed
ssh root@10.8.20.220 "systemctl stop containerd && cp /tmp/containerd-fixed /usr/bin/containerd && systemctl start containerd"
```

### 2. Check Containerd Logs
After deployment, monitor the containerd logs to see the debug output:
```bash
ssh root@10.8.20.220 "journalctl -u containerd -f"
```

### 3. Test Container Creation
Try creating the pod again and look for these log messages:
- `isSharedSnapshot: returning true/false for labels`
- `getSharedPathBase: Labels check - sharedDiskPath=...`
- `CRITICAL: Short paths directory was deleted, force recreating`
- `Short paths directory protection ensured`

### 4. Verify Directory Structure
Check that the `/s/l` directory persists:
```bash
ssh root@10.8.20.220 "ls -la /s/"
ssh root@10.8.20.220 "ls -la /s/l/"
```

### 5. If Still Failing
If the container still fails to create, the issue might be:
- The labels are not being passed correctly from CRI to the snapshotter
- There's a timing issue in the snapshot creation process
- The container name is empty or not being set correctly

## Expected Behavior After Fix
1. The `/s/l` directory should exist and contain layer snapshots
2. The container should create successfully without timeout
3. The shared storage paths should be created correctly
4. The container should reach Running state

## Debugging Commands
```bash
# Check if /s/l directory exists
ls -la /s/l/

# Check containerd logs for snapshot creation
journalctl -u containerd -f | grep -i snapshot

# Check for our debug messages
journalctl -u containerd -f | grep -i "isSharedSnapshot\|getSharedPathBase\|CRITICAL"

# Check container creation process
crictl ps -a | grep pytorch

# Check if the container name is being resolved
kubectl logs -n kube-system -l app=containerd-debug
```

## Key Files Modified
- `plugins/snapshots/overlay/overlay.go` - Main fix
- `FIXED_ISSUE_SUMMARY.md` - Documentation of the fix
- `DEBUG_PLAN.md` - This debugging plan