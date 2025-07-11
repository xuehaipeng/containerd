#!/bin/sh
set -x # Enable command tracing for debugging

# Post-Start Hook Script for Hash-Based Shared Snapshot Session Management
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
log "=== Post-Start Hook: Hash-Based Shared Storage Session Management ==="
log "Using shell: sh (POSIX-compliant)"

# Also try to write to stdout/stderr
echo "POST-START HOOK RUNNING" >&1
echo "POST-START HOOK RUNNING" >&2

# Determine own upperdir from mount info
MY_OWN_UPPERDIR_HOST_PATH=$(awk '/overlay/ && /upperdir=/ { 
  # Find the field containing upperdir=
  for (i=1; i<=NF; i++) { 
    if ($i ~ /upperdir=/) { 
      # Extract everything after upperdir= and before the next comma
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

# Parse the hash-based path structure: /s/{pod_hash}/{snapshot_hash}/fs
MY_OWN_SNAPSHOT_HASH=$(basename "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
MY_POD_HASH=$(basename "$(dirname "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")")
log "Current pod hash: $MY_POD_HASH"
log "Current snapshot hash: $MY_OWN_SNAPSHOT_HASH"

# The base path for all sessions, as seen INSIDE the container
CONTAINER_SESSIONS_PATH="/sessions"
log "Base sessions path (inside container): $CONTAINER_SESSIONS_PATH"

# Look for previous session directories in the same pod hash directory
PREVIOUS_SNAPSHOT_HASH=""
PREVIOUS_SNAPSHOT_PATH=""

if [ -d "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH" ]; then
    log "Scanning for previous sessions in pod hash directory: $MY_POD_HASH"
    
    # Look for the most recent non-current session in our pod hash directory
    LATEST_TIME=0
    for SNAPSHOT_DIR in "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH"/* ; do
        if [ -d "${SNAPSHOT_DIR}/fs" ]; then
            SNAPSHOT_HASH=$(basename "$SNAPSHOT_DIR")
            if [ "$SNAPSHOT_HASH" != "$MY_OWN_SNAPSHOT_HASH" ]; then
                # Check if directory has content
                if [ -n "$(find "${SNAPSHOT_DIR}/fs" -mindepth 1 -print -quit 2>/dev/null)" ]; then
                    # Get the modification time of the directory to find the most recent
                    DIR_TIME=$(stat -c %Y "$SNAPSHOT_DIR" 2>/dev/null || echo 0)
                    if [ "$DIR_TIME" -gt "$LATEST_TIME" ]; then
                        log "Found candidate session: $SNAPSHOT_HASH (mod time: $DIR_TIME)"
                        PREVIOUS_SNAPSHOT_HASH=$SNAPSHOT_HASH
                        PREVIOUS_SNAPSHOT_PATH="${SNAPSHOT_DIR}/fs"
                        LATEST_TIME=$DIR_TIME
                    fi
                fi
            fi
        fi
    done
    
    if [ -n "$PREVIOUS_SNAPSHOT_HASH" ]; then
        log "Selected most recent previous session: $PREVIOUS_SNAPSHOT_HASH"
    fi
else
    log "No pod hash directory found: $CONTAINER_SESSIONS_PATH/$MY_POD_HASH"
fi

# Restore from previous session if found
if [ -n "$PREVIOUS_SNAPSHOT_HASH" ] && [ -n "$PREVIOUS_SNAPSHOT_PATH" ]; then
    log "=== Restoring from previous session: $PREVIOUS_SNAPSHOT_HASH ==="
    log "Source (container path): $PREVIOUS_SNAPSHOT_PATH"
    log "Target: / (container root)"
    
    # Copy data with fallback methods
    if command -v rsync >/dev/null 2>&1; then
        log "Using rsync for data migration... (timeout 5m)"
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
        log "ERROR: Data migration failed (exit code: $COPY_EXIT_CODE)"
        log "Manual intervention may be required."
    fi
else
    log "No previous session found to restore from. Starting with fresh session."
fi

log "=== Cleanup: Removing old sessions (keeping current and most recent previous) ==="
CLEANUP_COUNT=0

if [ -d "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH" ]; then
    for SNAPSHOT_DIR in "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH"/* ; do
        if [ -d "$SNAPSHOT_DIR" ]; then
            SNAPSHOT_HASH=$(basename "$SNAPSHOT_DIR")
            # Keep current session and the one we just restored from
            if [ "$SNAPSHOT_HASH" != "$MY_OWN_SNAPSHOT_HASH" ] && [ "$SNAPSHOT_HASH" != "$PREVIOUS_SNAPSHOT_HASH" ]; then
                log "Removing old session directory: $SNAPSHOT_DIR (timeout 5m)"
                timeout 300 rm -rf "$SNAPSHOT_DIR" >> "$LOG_FILE" 2>&1
                RM_EXIT_CODE=$?
                if [ $RM_EXIT_CODE -ne 0 ]; then
                    log "WARNING: 'rm -rf $SNAPSHOT_DIR' finished with exit code $RM_EXIT_CODE. It may have timed out or failed."
                fi
                CLEANUP_COUNT=$((CLEANUP_COUNT + 1))
            fi
        fi
    done
fi

# Also clean up any legacy structure that might exist
if [ -d "$CONTAINER_SESSIONS_PATH/default" ]; then
    log "Cleaning up legacy session structure..."
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

log "Cleanup complete. Removed $CLEANUP_COUNT old session director(y/ies)."
log "=== Post-Start Hook completed ==="