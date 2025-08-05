#!/bin/bash
# Session restore script for preStop hook
# Restores container session data from shared backup storage to local storage

set -euo pipefail

# Default values
PATH_MAPPINGS_FILE="/etc/path-mappings.json"
LOCAL_SESSIONS_PATH="/etc/sessions"  # Mounted from /shared/nb
BACKUP_STORAGE_PATH="/etc/backup"    # Mounted from /tecofs/nb-sessions/<namespace>/<pod_name>/<container_name>
NAMESPACE="${CURRENT_NAMESPACE:-${NAMESPACE:-default}}"
POD_NAME="${HOSTNAME:-${POD_NAME:-nb-test-0}}"
CONTAINER_NAME="${CURRENT_CONTAINER_NAME:-${CONTAINER_NAME:-inference}}"
TIMEOUT="${TIMEOUT:-300}"
LOG_FILE="/tmp/session-restore.log"
DRY_RUN=false

# Logging function
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') $1" | tee -a "$LOG_FILE"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --mappings-file)
            PATH_MAPPINGS_FILE="$2"
            shift 2
            ;;
        --local-sessions-path)
            LOCAL_SESSIONS_PATH="$2"
            shift 2
            ;;
        --backup-storage-path)
            BACKUP_STORAGE_PATH="$2"
            shift 2
            ;;
        --namespace)
            NAMESPACE="$2"
            shift 2
            ;;
        --pod-name)
            POD_NAME="$2"
            shift 2
            ;;
        --container-name)
            CONTAINER_NAME="$2"
            shift 2
            ;;
        --timeout)
            TIMEOUT="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        *)
            log "Unknown option: $1"
            exit 1
            ;;
    esac
done

log "=== Session Restore Started ==="
log "Path mappings file: $PATH_MAPPINGS_FILE"
log "Local sessions path: $LOCAL_SESSIONS_PATH"
log "Backup storage path: $BACKUP_STORAGE_PATH"
log "Namespace: $NAMESPACE"
log "Pod name: $POD_NAME"
log "Container name: $CONTAINER_NAME"
log "Timeout: $TIMEOUT seconds"
log "Dry run: $DRY_RUN"

# Validate required parameters
if [[ -z "$NAMESPACE" ]] || [[ -z "$POD_NAME" ]] || [[ -z "$CONTAINER_NAME" ]]; then
    log "ERROR: Missing required parameters (namespace, pod-name, container-name)"
    exit 1
fi

# Check if jq is available for JSON parsing
if ! command -v jq >/dev/null 2>&1; then
    log "ERROR: jq is required for JSON parsing but not found"
    exit 1
fi

# Check if path mappings file exists
if [[ ! -f "$PATH_MAPPINGS_FILE" ]]; then
    log "WARNING: Path mappings file not found: $PATH_MAPPINGS_FILE"
    log "=== Session Restore Completed (No Path Mappings) ==="
    exit 0
fi

# Parse path mappings to find current session (newest by created_at)
log "Parsing path mappings file to find current session..."
SESSION_INFO=$(jq -r --arg ns "$NAMESPACE" --arg pod "$POD_NAME" --arg container "$CONTAINER_NAME" '
    .mappings | to_entries[] | 
    select(.value.namespace == $ns and .value.pod_name == $pod and .value.container_name == $container) | 
    .key + " " + .value.created_at' "$PATH_MAPPINGS_FILE" 2>/dev/null | sort -k2 -r | head -n 1)

if [[ -z "$SESSION_INFO" ]]; then
    log "WARNING: No session found in path mappings for namespace=$NAMESPACE, pod=$POD_NAME, container=$CONTAINER_NAME"
    log "=== Session Restore Completed (No Session Found) ==="
    exit 0
fi

# Extract session key (pod_hash/snapshot_hash)
SESSION_KEY=$(echo "$SESSION_INFO" | cut -d' ' -f1)
POD_HASH=$(echo "$SESSION_KEY" | cut -d'/' -f1)
SNAPSHOT_HASH=$(echo "$SESSION_KEY" | cut -d'/' -f2)

log "Found current session: pod_hash=$POD_HASH, snapshot_hash=$SNAPSHOT_HASH"

# Construct local session path (this is the overlayfs upperdir)
LOCAL_SESSION_DIR="$LOCAL_SESSIONS_PATH/$POD_HASH/$SNAPSHOT_HASH/fs"

# But we need to restore to the actual container root directory
CONTAINER_ROOT_DIR="/"

log "Local session directory (overlayfs upperdir): $LOCAL_SESSION_DIR"
log "Container root directory: $CONTAINER_ROOT_DIR"
log "Backup storage directory: $BACKUP_STORAGE_PATH"

# Debug: Show contents of path mappings file for this container
log "Debug: Path mappings for this container:"
jq -r --arg ns "$NAMESPACE" --arg pod "$POD_NAME" --arg container "$CONTAINER_NAME" '
    .mappings | to_entries[] | 
    select(.value.namespace == $ns and .value.pod_name == $pod and .value.container_name == $container)' "$PATH_MAPPINGS_FILE" 2>/dev/null | tee -a "$LOG_FILE" || true

# Check if backup storage directory exists and has content
if [[ ! -d "$BACKUP_STORAGE_PATH" ]]; then
    log "WARNING: Backup storage directory does not exist: $BACKUP_STORAGE_PATH"
    log "=== Session Restore Completed (No Backup Data) ==="
    exit 0
fi

if [[ -z "$(ls -A "$BACKUP_STORAGE_PATH" 2>/dev/null)" ]]; then
    log "WARNING: Backup storage directory is empty: $BACKUP_STORAGE_PATH"
    log "=== Session Restore Completed (Empty Backup Data) ==="
    exit 0
fi

# Ensure local session directory exists
if [[ "$DRY_RUN" == false ]]; then
    mkdir -p "$LOCAL_SESSION_DIR"
    if [[ $? -ne 0 ]]; then
        log "ERROR: Failed to create local session directory: $LOCAL_SESSION_DIR"
        exit 1
    fi
else
    log "DRY RUN: Would create local session directory: $LOCAL_SESSION_DIR"
fi

# Debug: Show container root directory status
if [[ -d "$CONTAINER_ROOT_DIR" ]]; then
    log "Debug: Container root directory exists"
    log "Debug: Container root directory contents before restore:"
    ls -la "$CONTAINER_ROOT_DIR" 2>&1 | tee -a "$LOG_FILE" || true
else
    log "Debug: Container root directory does not exist"
fi

# Debug: Show backup storage directory contents
if [[ -d "$BACKUP_STORAGE_PATH" ]]; then
    log "Debug: Backup storage directory contents:"
    ls -la "$BACKUP_STORAGE_PATH" 2>&1 | tee -a "$LOG_FILE" || true
else
    log "Debug: Backup storage directory does not exist"
fi

# Copy session data from backup storage with timeout
log "Starting restore of session data from $BACKUP_STORAGE_PATH to $CONTAINER_ROOT_DIR..."

if [[ "$DRY_RUN" == false ]]; then
    if command -v timeout >/dev/null 2>&1 && command -v rsync >/dev/null 2>&1; then
        # Use rsync with options to handle all file types including hidden files, large files, and empty directories
        # --recursive: recurse into directories
        # --links: copy symlinks as symlinks
        # --perms: preserve permissions
        # --times: preserve times
        # --group: preserve group
        # --owner: preserve owner (super-user only)
        # --devices: preserve device files (super-user only)
        # --specials: preserve special files
        # --hard-links: preserve hard links
        # --delete: delete extraneous files from dest dirs
        # --ignore-errors: delete even if I/O errors occur
        timeout "$TIMEOUT" rsync -a --delete --ignore-errors "$BACKUP_STORAGE_PATH/" "$CONTAINER_ROOT_DIR/" 2>&1 | tee -a "$LOG_FILE"
        RESULT=${PIPESTATUS[0]}
    elif command -v rsync >/dev/null 2>&1; then
        rsync -a --delete --ignore-errors "$BACKUP_STORAGE_PATH/" "$CONTAINER_ROOT_DIR/" 2>&1 | tee -a "$LOG_FILE"
        RESULT=${PIPESTATUS[0]}
    else
        # Fallback to tar if rsync is not available
        log "Rsync not available, using tar for restore"
        # Use tar with options to properly handle all files including hidden ones
        log "Debug: Current working directory before tar: $(pwd)"
        log "Debug: BACKUP_STORAGE_PATH: $BACKUP_STORAGE_PATH"
        log "Debug: CONTAINER_ROOT_DIR: $CONTAINER_ROOT_DIR"
        
        # Check if source directory exists and has content
        if [[ -d "$BACKUP_STORAGE_PATH" ]]; then
            log "Debug: Source directory exists"
            log "Debug: Source directory contents:"
            ls -la "$BACKUP_STORAGE_PATH" 2>&1 | tee -a "$LOG_FILE" || true
        else
            log "Debug: Source directory does not exist: $BACKUP_STORAGE_PATH"
        fi
        
        # Use tar with proper options to preserve everything and handle existing files
        # -c: create archive
        # -f -: write to stdout
        # --exclude: exclude any temporary tar files
        # -p: preserve permissions
        # -h: follow symlinks
        # --xattrs: preserve extended attributes
        # --overwrite: overwrite existing files
        # --keep-old-files: keep existing files and don't overwrite (alternative approach)
        # .: current directory (all files)
        (cd "$BACKUP_STORAGE_PATH" && tar -cf - --exclude=".*.tar" -p --xattrs .) | (cd "$CONTAINER_ROOT_DIR" && tar -xf - -p --xattrs --overwrite) 2>&1 | tee -a "$LOG_FILE"
        RESULT=${PIPESTATUS[0]}
        log "Debug: Tar command completed with exit code: $RESULT"
    fi
else
    log "DRY RUN: Would copy data from $BACKUP_STORAGE_PATH to $CONTAINER_ROOT_DIR"
    RESULT=0
fi

if [[ $RESULT -eq 0 ]]; then
    log "Session restore completed successfully"
elif [[ $RESULT -eq 124 ]]; then
    log "ERROR: Session restore timed out after $TIMEOUT seconds"
    exit 1
else
    log "WARNING: Session restore completed with some errors (exit code: $RESULT)"
    # Don't exit with error for partial success - some files might be read-only
fi

# Debug: Show container root directory contents after restore
if [[ -d "$CONTAINER_ROOT_DIR" ]]; then
    log "Debug: Container root directory contents after restore:"
    ls -la "$CONTAINER_ROOT_DIR" 2>&1 | tee -a "$LOG_FILE" || true
    log "Debug: Container root user directory contents after restore:"
    ls -la "$CONTAINER_ROOT_DIR/root/" 2>&1 | tee -a "$LOG_FILE" || true
else
    log "Debug: Container root directory does not exist after restore"
fi

log "=== Session Restore Completed ==="
exit 0