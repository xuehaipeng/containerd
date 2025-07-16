#!/bin/sh
set -x # Enable command tracing for debugging

# Post-Start Hook Script for Hash-Based Shared Snapshot Session Management
# This script uses the path mappings file to discover current session info

LOG_FILE="/tmp/poststart.log"
LOCK_FILE="/tmp/poststart.lock"
PATH_MAPPINGS_FILE="/path-mappings.json"

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

# Get current pod information from environment or defaults
CURRENT_NAMESPACE="${CURRENT_NAMESPACE:-default}"
CURRENT_POD_NAME="${HOSTNAME:-nb-test-0}"
CURRENT_CONTAINER_NAME="${CURRENT_CONTAINER_NAME:-inference}"

log "Current pod info: namespace=$CURRENT_NAMESPACE, pod=$CURRENT_POD_NAME, container=$CURRENT_CONTAINER_NAME"

# Read path mappings file to find our current session
if [ ! -f "$PATH_MAPPINGS_FILE" ]; then
    log "ERROR: Path mappings file not found: $PATH_MAPPINGS_FILE"
    log "Starting with fresh session."
    exit 0
fi

log "Reading path mappings from: $PATH_MAPPINGS_FILE"

# Extract our current session info using awk (more portable than jq)
CURRENT_MAPPING=$(awk -v ns="$CURRENT_NAMESPACE" -v pod="$CURRENT_POD_NAME" -v container="$CURRENT_CONTAINER_NAME" '
BEGIN { 
    found = 0
    latest_time = ""
    latest_path = ""
    latest_pod_hash = ""
    latest_snapshot_hash = ""
}
/"mappings"/ { in_mappings = 1; next }
in_mappings && /"[^"]+": {/ {
    # Extract the path key (e.g., "6fb76255/7ed8f0f3")
    gsub(/.*"/, "", $0)
    gsub(/": {.*/, "", $0)
    current_path = $0
    in_entry = 1
    next
}
in_entry && /"namespace":/ {
    gsub(/.*"namespace": "/, "", $0)
    gsub(/".*/, "", $0)
    entry_namespace = $0
}
in_entry && /"pod_name":/ {
    gsub(/.*"pod_name": "/, "", $0)
    gsub(/".*/, "", $0)
    entry_pod = $0
}
in_entry && /"container_name":/ {
    gsub(/.*"container_name": "/, "", $0)
    gsub(/".*/, "", $0)
    entry_container = $0
}
in_entry && /"created_at":/ {
    gsub(/.*"created_at": "/, "", $0)
    gsub(/".*/, "", $0)
    entry_time = $0
}
in_entry && /}/ {
    # End of entry, check if it matches our pod
    if (entry_namespace == ns && entry_pod == pod && entry_container == container) {
        if (entry_time > latest_time) {
            latest_time = entry_time
            latest_path = current_path
            split(current_path, parts, "/")
            latest_pod_hash = parts[1]
            latest_snapshot_hash = parts[2]
            found = 1
        }
    }
    in_entry = 0
    entry_namespace = ""
    entry_pod = ""
    entry_container = ""
    entry_time = ""
}
END {
    if (found) {
        print latest_pod_hash ":" latest_snapshot_hash ":" latest_path
    }
}
' "$PATH_MAPPINGS_FILE")

if [ -z "$CURRENT_MAPPING" ]; then
    log "ERROR: Could not find current session in path mappings file"
    log "Starting with fresh session."
    exit 0
fi

# Parse the result
MY_POD_HASH=$(echo "$CURRENT_MAPPING" | cut -d: -f1)
MY_OWN_SNAPSHOT_HASH=$(echo "$CURRENT_MAPPING" | cut -d: -f2)
CURRENT_PATH=$(echo "$CURRENT_MAPPING" | cut -d: -f3)

log "Found current session: pod_hash=$MY_POD_HASH, snapshot_hash=$MY_OWN_SNAPSHOT_HASH, path=$CURRENT_PATH"

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