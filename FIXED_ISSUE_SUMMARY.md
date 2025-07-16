# Fixed Issue: /s/l Directory Deletion During Pod Creation

## Problem Description
When a pod matching shared storage conditions (defined in `/etc/containerd/config.toml`) is created, the `/s/l` directory disappears, breaking container functionality. This directory contains critical layer snapshots (lowerdir) used by the overlay filesystem.

## Root Cause
The issue was caused by a conflict between:
1. **Shared storage snapshots** (stored in `/s/{pod_hash}/{snapshot_hash}/`) 
2. **Layer snapshots** (stored in `/s/l/{layer_id}/fs`)

When creating a shared snapshot, the code attempted to create a "local snapshot ID marker directory" at `/s/l/{snapshot_id}` which conflicted with existing layer snapshots.

## Solution Implemented

### 1. **Path Separation** 
- Shared storage snapshots now use the original containerd path (`/s/d/io.containerd.snapshotter.v1.overlayfs/snapshots/`) for local marker directories
- Layer snapshots continue to use the short paths (`/s/l/`) 
- This prevents any interference between the two systems

### 2. **Multi-Level Protection**
Added comprehensive protection mechanisms:

#### A. **Protection Functions**
- `ensureShortPathsProtection()`: Creates and protects the `/s/l` directory with a marker file
- `forceProtectShortPaths()`: Aggressively recreates the directory if it's missing

#### B. **Protection Points**
Protection is enforced at multiple critical points:
- Snapshotter initialization
- Before/after shared snapshot creation
- Before mounting operations
- Before determining mount options
- After removal operations

### 3. **Enhanced Cleanup Safety**
- Added filtering in `getCleanupDirectories()` to exclude any paths within `/s/l/`
- Enhanced `Cleanup()` method with explicit safeguards
- Added logging for better debugging

### 4. **Marker File Protection**
- Created `.containerd_layer_snapshots` marker file in `/s/l/` 
- Contains clear warning about the directory's importance
- Helps identify the directory's purpose during debugging

## Code Changes

### Modified Files:
- `plugins/snapshots/overlay/overlay.go`

### Key Changes:
1. **Fixed shared snapshot marker directory path** (Line 975)
   ```go
   // OLD: ensureLocalSnapshotIDDir := o.getSnapshotPath(s.ID)
   // NEW: ensureLocalSnapshotIDDir := filepath.Join(o.root, "snapshots", s.ID)
   ```

2. **Added protection function calls** at critical points:
   - Before/after shared snapshot creation
   - Before mounting operations
   - Before determining mount options
   - After removal operations

3. **Enhanced cleanup filtering** (Lines 708-723)
   ```go
   if o.shortBasePaths {
       // Filter out any paths that might be in the short paths directory
       filteredCleanup := []string{}
       for _, dir := range cleanup {
           if strings.HasPrefix(dir, shortPathsDir+"/") || dir == shortPathsDir {
               // Skip deletion of layer content
               continue
           }
           filteredCleanup = append(filteredCleanup, dir)
       }
       cleanup = filteredCleanup
   }
   ```

## Testing Status
- ✅ Code implemented
- ⏳ Ready for deployment and testing
- ⏳ Requires verification in production environment

## Expected Behavior After Fix
1. `/s/l` directory will persist across all container operations
2. Layer snapshots will remain available for container mounting
3. Shared storage snapshots will work without interfering with layer snapshots
4. Container creation for pods matching shared storage conditions will succeed
5. `kubectl exec` and other container operations will work normally

## Verification Steps
1. Deploy the fixed containerd binary
2. Create a pod matching shared storage conditions (e.g., `nb-test-*` in `default` namespace)
3. Verify `/s/l` directory exists and contains layer snapshots
4. Verify container reaches Running state
5. Verify `kubectl exec` works without errors
6. Check containerd logs for protection messages