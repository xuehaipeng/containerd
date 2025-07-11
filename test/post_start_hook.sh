#!/bin/sh
set -x # Enable command tracing for debugging

# Post-Start Hook Script for Shared Snapshot Session Management
# This script handles migration of data from previous sessions to the current session

LOG_FILE="/tmp/poststart.log"
LOCK_FILE="/tmp/poststart.lock"

# Function to log messages
log() {
    echo "$1" >> "$LOG_FILE" 2>&1
}

# Check if another instance is running
if [ -f "$LOCK_FILE" ]; then
    log "Another instance is running. Exiting."
    exit 0
fi

# Create lock file
echo "$$" > "$LOCK_FILE"

# Cleanup function
cleanup() {
    rm -f "$LOCK_FILE"
}

# Set up trap to ensure lock file is removed
trap cleanup EXIT

# Initialize log
echo "POST-START HOOK EXECUTED at $(date)" > "$LOG_FILE" 2>&1
log "=== Post-Start Hook: Checking for previous session data ==="
log "Using shell: sh (POSIX-compliant)"

# Also try to write to stdout/stderr
echo "POST-START HOOK RUNNING" >&1
echo "POST-START HOOK RUNNING" >&2

# Determine own upperdir from mount info
# Use a more portable approach that works with sh
MY_OWN_UPPERDIR_HOST_PATH=$(awk '/overlay/ && /upperdir=/ { 
  # Find the field containing upperdir=
  for (i=1; i<=NF; i++) { 
    if ($i ~ /upperdir=/) { 
      # Extract everything after upperdir= and before the next comma
      # Use gsub for better POSIX compatibility
      temp = $i
      gsub(/.*upperdir=/, "", temp)
      gsub(/,.*/, "", temp)
      if (temp != "") {
        print temp
        exit
      }
    } 
  } 
}' /proc/self/mountinfo)

log "Raw mount info:"
cat /proc/self/mountinfo | grep overlay >> "$LOG_FILE" 2>&1
log "Extracted upperdir: '$MY_OWN_UPPERDIR_HOST_PATH'"

if [ -z "$MY_OWN_UPPERDIR_HOST_PATH" ]; then
    log "ERROR: Could not determine own shared upperdir. Proceeding with empty session."
    exit 0
fi

log "Current upperdir: $MY_OWN_UPPERDIR_HOST_PATH"
MY_OWN_SNAPSHOT_ID=$(basename "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
log "Current snapshot ID: $MY_OWN_SNAPSHOT_ID"

# The base path for all sessions, as seen INSIDE the container
CONTAINER_SESSIONS_PATH="/sessions"
log "Base sessions path (inside container): $CONTAINER_SESSIONS_PATH"

# Look for previous session directories
PREVIOUS_SNAPSHOT_ID=""
PREVIOUS_SNAPSHOT_PATH=""

if [ -d "$CONTAINER_SESSIONS_PATH" ]; then
    log "Scanning for a non-empty previous session to restore..."
    # Handle both old flat structure and new hash-based nested structure
    
    # First, try to find sessions in hash-based subdirectories (new structure)
    for HASH_DIR in "$CONTAINER_SESSIONS_PATH"/* ; do
        if [ -d "$HASH_DIR" ] && [ "$(basename "$HASH_DIR")" != "default" ]; then
            # This looks like a hash-based directory, scan for sessions inside it
            for D_CONTAINER_PATH in "$HASH_DIR"/* ; do
                if [ -d "${D_CONTAINER_PATH}/fs" ]; then
                    SNAP_ID=$(basename "$D_CONTAINER_PATH")
                    if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
                        # Check if directory has content
                        if [ -n "$(find "${D_CONTAINER_PATH}/fs" -mindepth 1 -print -quit 2>/dev/null)" ]; then
                            log "Found candidate non-empty session in hash structure: $SNAP_ID. Will use this one unless a newer one is found."
                            PREVIOUS_SNAPSHOT_ID=$SNAP_ID
                            PREVIOUS_SNAPSHOT_PATH="${D_CONTAINER_PATH}/fs"
                        fi
                    fi
                fi
            done
        fi
    done
    
    # If no hash-based session found, try the old flat structure (for backward compatibility)
    if [ -z "$PREVIOUS_SNAPSHOT_ID" ]; then
    for D_CONTAINER_PATH in "$CONTAINER_SESSIONS_PATH"/* ; do
        if [ -d "${D_CONTAINER_PATH}/fs" ]; then
            SNAP_ID=$(basename "$D_CONTAINER_PATH")
            if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
                    # Check if directory has content
                if [ -n "$(find "${D_CONTAINER_PATH}/fs" -mindepth 1 -print -quit 2>/dev/null)" ]; then
                        log "Found candidate non-empty session in flat structure: $SNAP_ID. Will use this one unless a newer one is found."
                    PREVIOUS_SNAPSHOT_ID=$SNAP_ID
                    PREVIOUS_SNAPSHOT_PATH="${D_CONTAINER_PATH}/fs"
                fi
            fi
        fi
    done
    fi
    
    # Also check the old named structure under /sessions/default/ (legacy support)
    if [ -d "$CONTAINER_SESSIONS_PATH/default" ]; then
        for POD_DIR in "$CONTAINER_SESSIONS_PATH/default"/* ; do
            if [ -d "$POD_DIR" ] && [ -d "${POD_DIR}/fs" ]; then
                # Check if directory has content
                if [ -n "$(find "${POD_DIR}/fs" -mindepth 1 -print -quit 2>/dev/null)" ]; then
                    POD_NAME=$(basename "$POD_DIR")
                    log "Found candidate non-empty session in legacy structure: $POD_NAME"
                    PREVIOUS_SNAPSHOT_ID="legacy-$POD_NAME"
                    PREVIOUS_SNAPSHOT_PATH="${POD_DIR}/fs"
                fi
            fi
        done
    fi
fi

# Restore from previous session if found
if [ -n "$PREVIOUS_SNAPSHOT_ID" ] && [ -n "$PREVIOUS_SNAPSHOT_PATH" ]; then
    log "=== Restoring from latest found session: $PREVIOUS_SNAPSHOT_ID ==="
    log "Source (container path): $PREVIOUS_SNAPSHOT_PATH"
    # The target for the copy is the container's root filesystem ('/'), which corresponds to the new upperdir.
    log "Target: / (container root)"
    
    # Copy data with fallback methods
    if command -v rsync >/dev/null 2>&1; then
        log "Using rsync for data migration... (timeout 5m)"
        # Note: We are copying from a container path to a host path. This works because
        # the container's root filesystem is an overlay mount that includes the target upperdir.
        timeout 300 rsync -avp --delete "${PREVIOUS_SNAPSHOT_PATH}/" "/" >> "$LOG_FILE" 2>&1
        COPY_EXIT_CODE=$?
    else
        log "rsync not available, using cp... (timeout 5m)"
        timeout 300 cp -rp "${PREVIOUS_SNAPSHOT_PATH}/." "/" >> "$LOG_FILE" 2>&1
        COPY_EXIT_CODE=$?
    fi
    
    if [ $COPY_EXIT_CODE -eq 0 ]; then
        log "=== Data migration successful ==="
    else
        log "ERROR: Data migration failed (exit code: $COPY_EXIT_CODE). Old session data NOT cleaned up."
        log "Manual intervention may be required."
    fi
else
    log "No non-empty previous session found to restore from. Starting with fresh session."
fi

log "=== Cleaning up ALL other session directories ==="
CLEANUP_COUNT=0
if [ -d "$CONTAINER_SESSIONS_PATH" ]; then
    # Clean up sessions in hash-based subdirectories (new structure)
    for HASH_DIR in "$CONTAINER_SESSIONS_PATH"/* ; do
        if [ -d "$HASH_DIR" ] && [ "$(basename "$HASH_DIR")" != "default" ]; then
            # This looks like a hash-based directory, clean up sessions inside it
            for D_CONTAINER_PATH in "$HASH_DIR"/* ; do
        if [ -d "$D_CONTAINER_PATH" ]; then
            SNAP_ID=$(basename "$D_CONTAINER_PATH")
            if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
                        log "Removing old/stale session directory in hash structure: $D_CONTAINER_PATH (timeout 5m)"
                timeout 300 rm -rf "$D_CONTAINER_PATH" >> "$LOG_FILE" 2>&1
                RM_EXIT_CODE=$?
                if [ $RM_EXIT_CODE -ne 0 ]; then
                    log "WARNING: 'rm -rf $D_CONTAINER_PATH' finished with exit code $RM_EXIT_CODE. It may have timed out or failed."
                fi
                CLEANUP_COUNT=$((CLEANUP_COUNT + 1))
            fi
        fi
    done
        fi
    done
    
    # Clean up sessions in old flat structure (for backward compatibility)
    for D_CONTAINER_PATH in "$CONTAINER_SESSIONS_PATH"/* ; do
        if [ -d "$D_CONTAINER_PATH" ]; then
            DIR_NAME=$(basename "$D_CONTAINER_PATH")
            # Skip hash directories (already processed above) and special directories
            if [ "$DIR_NAME" != "default" ] && [ ${#DIR_NAME} -ne 8 ]; then
                SNAP_ID="$DIR_NAME"
                if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
                    log "Removing old/stale session directory in flat structure: $D_CONTAINER_PATH (timeout 5m)"
                    timeout 300 rm -rf "$D_CONTAINER_PATH" >> "$LOG_FILE" 2>&1
                    RM_EXIT_CODE=$?
                    if [ $RM_EXIT_CODE -ne 0 ]; then
                        log "WARNING: 'rm -rf $D_CONTAINER_PATH' finished with exit code $RM_EXIT_CODE. It may have timed out or failed."
                    fi
                    CLEANUP_COUNT=$((CLEANUP_COUNT + 1))
                fi
            fi
        fi
    done
    
    # Clean up old named structure under /sessions/default/ (legacy support)
    if [ -d "$CONTAINER_SESSIONS_PATH/default" ]; then
        for POD_DIR in "$CONTAINER_SESSIONS_PATH/default"/* ; do
            if [ -d "$POD_DIR" ]; then
                POD_NAME=$(basename "$POD_DIR")
                log "Removing legacy session directory: $POD_DIR (timeout 5m)"
                timeout 300 rm -rf "$POD_DIR" >> "$LOG_FILE" 2>&1
                RM_EXIT_CODE=$?
                if [ $RM_EXIT_CODE -ne 0 ]; then
                    log "WARNING: 'rm -rf $POD_DIR' finished with exit code $RM_EXIT_CODE. It may have timed out or failed."
                fi
                CLEANUP_COUNT=$((CLEANUP_COUNT + 1))
            fi
        done
    fi
fi
log "Cleanup complete. Removed $CLEANUP_COUNT old session director(y/ies)."

log "=== Post-Start Hook completed ==="