# Custom OverlayFS Snapshotter for Shared Upper Directories

## 1. Purpose

This document describes a customization to the standard `overlayfs` snapshotter in containerd. The primary motivation is to better support 'Notebook' workloads commonly used in deep learning scenarios (e.g., model development, data preparation) within a Kubernetes environment.

**Challenges with Standard Approach for 'Notebook' Workloads:**

Notebook workloads are typically single-container pods where users perform iterative development. A common workflow involves:
1.  Starting a Notebook from a base image.
2.  Installing new software packages (e.g., via pip), downloading datasets, and generating model files or intermediate data within the container's writable layer.
3.  To persist this state, users might commit the container's filesystem changes into a new OCI image (e.g., using `nerdctl commit`). This new image is then pushed to a remote registry.
4.  Work is resumed by creating a new pod from this committed image.

This approach, while functional, leads to several issues for Notebooks:
*   **Large Image Sizes**: Continuous addition of packages, data, and generated files results in progressively larger committed OCI images.
*   **OverlayFS Layer Limits**: Frequent commits of large changes can quickly approach or exceed the maximum layer limitation of overlayfs (typically 128 layers) when these images are used as bases for further work, preventing further commits or causing instability.
*   **Inefficient Storage and Transfer**: Pushing and pulling very large images consumes significant network bandwidth and registry storage.
*   **Ephemeral Storage Management and Data Loss**: Standard Kubernetes ephemeral storage limits (`spec.resources.limits.ephemeral-storage`) apply to the container's local writable layer. When exceeded, the pod is evicted, leading to the loss of all uncommitted work in that writable layer.

**Goals of this Customization:**

This `overlayfs` snapshotter customization aims to address these challenges by:

1.  **Redirecting the Live Writable Layer**: The `upperdir` (writable layer) and `workdir` of the Notebook container's snapshot are stored directly on a designated shared file storage system (e.g., a distributed filesystem mounted at `/tecofs-m`). The `lowerdir` (base image layers) remains on the local filesystem for fast startup and efficient image sharing between containers on the same node.
2.  **Leveraging Shared Storage Quotas for Live Sessions**: The target shared storage system often provides mechanisms (e.g., via REST API for directory quotas) to limit the storage consumed by each Notebook's shared `upperdir`.
    *   Crucially, if a container exceeds its quota on the shared storage, it will typically receive "No space left on device" or "Disk quota exceeded" errors *within the container*. This allows the user to manage their space (e.g., delete files) without the pod being abruptly evicted by Kubernetes, thus preserving the current session's state on the shared storage.
3.  **Facilitating More Manageable Image Commits**: While users can still commit their Notebook's state (now residing on the shared `upperdir`) into a new OCI image, the pressure to do so frequently *just to save work-in-progress from ephemeral storage loss* is reduced. The active work is already on more robust, quota-managed shared storage.

The key characteristics of the customized snapshotter are:
- **Local Lower Layers**: Base image layers are pulled and stored locally.
- **Shared Upper Layer for Active Sessions**: The Notebook container's live writable layer (`upperdir`) and `workdir` are created on the configured shared storage path.
- **Dynamic Path Construction**: The path on the shared storage is dynamically constructed using Kubernetes pod metadata: `/<configured_shared_path>/<kubernetes_namespace>/<pod_name>/<container_name_in_pod>/<snapshot_id>/`.

This approach provides a more robust and manageable storage solution for stateful, iterative workloads like Notebooks, especially when fine-grained quota control for the live writable layer and resilience against ephemeral storage eviction are important.

## 2. Configuration

To enable and use this feature, two main configuration steps are required:

### 2.1. Configure containerd's CRI Plugin

The CRI plugin needs to be informed of the base path for the shared storage and the matching rules for which pods to apply the feature to. In containerd v2.1 and later, these options should be placed in the `[plugins."io.containerd.cri.v1.runtime"]` section of your `config.toml`.

```toml
# Example: /etc/containerd/config.toml
version = 3
# ... other global settings ...

[plugins]
  # ... other plugins ...

  [plugins."io.containerd.cri.v1.runtime"]
    # ... other runtime settings ...

    # shared_snapshot_path specifies the base directory on the shared storage.
    # This is required to enable the feature.
    shared_snapshot_path = "/tecofs-m"

    # (Optional) An RE2-compliant regular expression. If set, only pods in a
    # namespace matching this pattern will use the shared snapshot feature.
    # Example: "^kubecube-.*" matches all namespaces starting with "kubecube-".
    # Example: ".*" matches all namespaces.
    shared_snapshot_namespace_regex = "^kubecube-.*"

    # (Optional) An RE2-compliant regular expression. If set, only pods with
    # a name matching this pattern will use the shared snapshot feature.
    # Example: "^nb-.*" matches all pod names starting with "nb-".
    shared_snapshot_pod_name_regex = "^nb-.*"

    [plugins."io.containerd.cri.v1.runtime".containerd]
      snapshotter = "overlayfs"
      # ... other containerd settings ...
```

After modifying the configuration, restart the containerd service:
```bash
sudo systemctl restart containerd
```

### 2.2. (Implicit) Kubernetes Integration - How It's Applied

When Kubernetes requests container creation, the CRI plugin now checks for these settings:
1. It verifies that `shared_snapshot_path` is configured.
2. It checks if the pod's namespace and name match the `shared_snapshot_namespace_regex` and `shared_snapshot_pod_name_regex` rules, respectively. If a rule is not set, it is considered a match.
3. If all configured rules pass, it injects the necessary labels (`containerd.io/snapshot/use-shared-storage: "true"`, etc.) into the snapshot options to activate the shared storage logic for that container.

## 3. How It Works - Snapshotter Modifications

The `plugins/snapshots/overlay/overlay.go` file was modified to interpret these labels:

1.  **Label Recognition**: The snapshotter now checks for the presence of `containerd.io/snapshot/use-shared-storage: "true"` and the other related labels (`k8s-namespace`, `k8s-pod-name`, `k8s-container-name`, `shared-disk-path`) on snapshots.
2.  **Path Determination**:
    *   If these labels are present on an **active** snapshot (a container's writable layer):
        *   The `upperdir` path is constructed as: `LABELS[shared-disk-path]/LABELS[k8s-namespace]/LABELS[k8s-pod-name]/LABELS[k8s-container-name]/<SNAPSHOT_ID>/fs`
        *   The `workdir` path is constructed as: `LABELS[shared-disk-path]/LABELS[k8s-namespace]/LABELS[k8s-pod-name]/LABELS[k8s-container-name]/<SNAPSHOT_ID>/work`
    *   If the labels are not present, or if the snapshot is not an active writable layer (e.g., it's a committed image layer), the snapshotter defaults to its standard local path construction (e.g., under `/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/<SNAPSHOT_ID>/fs`).
3.  **Directory Management**:
    *   **Creation**: During the `Prepare` phase for a matching active snapshot, the snapshotter creates the `fs` (upperdir) and `work` directories directly on the shared storage path.
    *   **Preservation on Deletion**: When a container using the shared snapshot feature is removed, the snapshotter will **preserve the `upperdir` and `workdir` on the shared storage**. The `os.RemoveAll` call for the shared path is intentionally skipped. This is the key mechanism that allows a Notebook's state to persist across restarts. The final cleanup of these directories becomes the responsibility of an external orchestration process that knows when a Notebook instance is permanently deleted.
4.  **Mounts**: The `mounts` operation correctly provides the shared `upperdir` and `workdir` to the `overlayfs` mount options when a shared snapshot is being mounted. The `lowerdir` will always point to local image layers.

## 4. Session Management, Image Committing, and Resuming Notebooks

This customization changes how the live writable layer of a container is stored. This has implications for common Notebook workflows like saving state and resuming sessions.

### 4.1. Live Session State

- For a container launched with this feature active, all changes made during the session (installing packages, creating files, downloading data) are written directly to its `upperdir` on the shared storage: `/<shared_snapshot_path_from_config>/<k8s_namespace>/<pod_name>/<container_name>/<snapshot_id>/fs`.
- This directory is subject to quotas imposed by the shared storage system, providing a "disk full" error rather than pod eviction if the quota is hit.

### 4.2. Committing to a New OCI Image (Creating a Versioned Snapshot)

Users can still create a versioned, portable OCI image from the state of their Notebook's shared `upperdir`:
- Use standard tools like `nerdctl commit <container_id> <new_image_tag>` (or the equivalent containerd API).
- Containerd's commit process operates on the live container's mount.
- Since the live mount for a customized container already uses the `upperdir` on shared storage, the commit operation will read this shared `upperdir` and the local `lowerdirs` (from the base image) to create the new image layer.
- The size of the new layer in the resulting OCI image will correspond to the data in the shared `upperdir`.
- This new OCI image can then be pushed to a registry (e.g., Harbor) and used to start fresh Notebook instances with this committed state.

### 4.3. Resuming a Notebook Session from a Previous Instance

The current plugin implementation assigns a **new, unique snapshot ID** (and therefore a new, empty shared `upperdir`) every time a new container is created by Kubernetes. To resume work from a *previous instance* of the same logical Notebook (identified by K8s Namespace, Pod Name, and Container Name), a data copy mechanism is needed. This is particularly useful for quick restarts or recovering from unexpected pod terminations where the previous shared `upperdir` data is still intact.

**Workflow for Resuming from Previous Instance (within a Post-Start Hook):**

1. **New Pod Instance (`pod_B`)**: A new pod (`pod_B`) is started, intended to resume/replace a previous instance (`pod_A`) of the same logical Notebook. It shares the same K8s Namespace, intended Pod Name, and Container Name.

2. **Plugin Creates New Shared `upperdir` (`P_target_host_path`)**: The customized plugin creates a new, empty shared `upperdir` for `pod_B`:
   ```
   P_target_host_path = /<shared_path_cfg>/<ns>/<pod_name>/<container_name>/<snap_B_id>/fs
   ```

3. **Post-Start Hook Execution**: A script in `pod_B`'s Post-Start hook (or an Init Container) executes with the following logic:

   a. **Determine Own `upperdir`**: The script discovers the host path to its *own* newly created shared `upperdir` (`P_target_host_path`) by parsing `/proc/self/mountinfo` to find the `upperdir=` option for its root (`/`) mount.

   ```bash
   # Example snippet for Post-Start script
   MY_OWN_UPPERDIR_HOST_PATH=$(awk '/ \/ overlay / && /upperdir=/ { for (i=1; i<=NF; i++) { if (match($i, /^upperdir=([^,]+)/, arr)) { print arr[1]; exit } } }' /proc/self/mountinfo)
   if [ -z "$MY_OWN_UPPERDIR_HOST_PATH" ]; then
       echo "ERROR: Could not determine own shared upperdir. Proceeding with empty session." >&2
       exit 0 # Or exit 1 to fail the hook, depending on desired behavior
   fi
   MY_OWN_SNAPSHOT_ID=$(basename "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
   ```

   b. **Identify Potential Previous Session Directory (`P_source_host_path`)**:
      - The script constructs the base path where snapshot directories for this Notebook identity reside:
        ```
        NOTEBOOK_SESSIONS_BASE_HOST_PATH=$(dirname "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
        ```
        (This resolves to `/<shared_path_cfg>/<ns>/<pod_name>/<container_name>/`)
      - It lists subdirectories (potential snapshot IDs) within `NOTEBOOK_SESSIONS_BASE_HOST_PATH`.
      - It searches for a directory that is *not* its own (`$MY_OWN_SNAPSHOT_ID`). If multiple such directories exist (e.g., from multiple previous unclean shutdowns), a strategy is needed (e.g., pick the one with the most recent modification time, or expect only one). For simplicity, this example assumes finding one, or the most recent valid one.

   ```bash
   PREVIOUS_SNAPSHOT_ID=""
   # Simplified: find any other snapshot ID. Robust script would sort by mtime.
   for D_HOST_PATH in "$NOTEBOOK_SESSIONS_BASE_HOST_PATH"/* ; do
       if [ -d "${D_HOST_PATH}/fs" ]; then # Check if it looks like a snapshot dir with an fs subdir
           SNAP_ID=$(basename "$D_HOST_PATH")
           if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
               # Basic: take the first one found. A real script might compare mtimes.
               PREVIOUS_SNAPSHOT_ID=$SNAP_ID 
               break 
           fi
       fi
   done
   ```

   c. **Copy and Cleanup (if previous session found)**:
      - If a `$PREVIOUS_SNAPSHOT_ID` is found:
        ```bash
        P_source_host_path="${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}/fs"
        echo "Previous session found at $P_source_host_path. Restoring to $MY_OWN_UPPERDIR_HOST_PATH ..."
        rsync -avp --delete "${P_source_host_path}/" "${MY_OWN_UPPERDIR_HOST_PATH}/"
        if [ $? -eq 0 ]; then
            echo "Restore successful. Cleaning up old session directory: ${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}"
            rm -rf "${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}"
        else
            echo "ERROR: rsync failed during restore. Old session data NOT cleaned up." >&2
            # Potentially exit 1 to make PostStart hook fail
        fi
        ```
      - If no previous session directory is found, the script logs this and exits gracefully, allowing the Notebook to start with a fresh (empty) shared `upperdir`.

**Assumptions for this Resume Method:**
- The shared storage path is accessible by the Post-Start hook (e.g., mounted into a helper container or the main container has tools like `rsync` and permissions).
- The previous pod instance, if it shut down cleanly, might have already been deleted by the plugin. This method is most effective for resuming after an *unclean* shutdown where the old shared `upperdir` remains, or if an orchestration layer deliberately leaves the old directory for this purpose.
- If multiple old directories exist, the selection logic needs to be robust (e.g., based on timestamps or external metadata).

### 4.4. Explicitly Saving/Backing Up a Session State

For more deliberate, named backups or versioning of Notebook states (distinct from the immediate resume-from-previous):
1.  **Identify Live Shared `upperdir`**: As in 4.3.3.a, the running Notebook's shared `upperdir` path (`/<shared_path_cfg>/.../<current_snap_id>/fs`) is identified.
2.  **Copy to a User-Defined Persistent Location**: Use `rsync` or a similar tool to copy the contents of this live shared `upperdir` to a separate, user-managed directory on the shared storage (e.g., `/tecofs-m/notebook_backups/<user_name>/<notebook_name>/<version_tag>/`).
3.  **Restore from Explicit Backup**: To restore from such a backup, the Post-Start hook in a new Notebook pod would be configured (e.g., via an environment variable) with the path to this specific backup directory and would `rsync` data from there into its newly created shared `upperdir`.

This provides more control over which state to restore if multiple saved versions exist.

### 4.5. Contrast with "Direct UpperDir Manipulation" (from `docs/design_3.md`)

The approach in `docs/design_3.md` involves `rsync`-ing a *local* `upperdir` to shared storage for backup, and then `rsync`-ing it back to a *new local* `upperdir` on restore. While it uses shared storage for transfer, the live container in that design uses local ephemeral storage, thus not benefiting from shared storage quotas for the active session to prevent evictions. The plugin detailed here, by placing the *live* `upperdir` on shared storage, directly addresses the quota management and eviction-avoidance goals for active sessions.

## 5. Building and Testing

### 5.1. Build containerd
After making the code changes described:
1.  Navigate to the root of the containerd source directory.
2.  Build containerd (e.g., using `make`).

### 5.2. Installation
Install your custom-built containerd binary, replacing the existing one on your test node. Ensure systemd unit files or other service configurations point to the correct binary.

### 5.3. Verification Steps
1.  **Configuration Check**:
    *   Ensure `shared_snapshot_path` is correctly set in your containerd config.
    *   Restart containerd and check its logs for any errors related to the CRI plugin or snapshotter initialization.

2.  **Default Behavior (No Shared Path)**:
    *   If `shared_snapshot_path` is *not* set or is empty in `config.toml`, deploy a simple pod.
    *   Verify that the container's snapshot directories (`fs`, `work`) are created in the default local snapshotter root (e.g., `/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/`).
    *   Verify container functionality and cleanup.

3.  **Shared Path Behavior**:
    *   Set `shared_snapshot_path` to your shared storage mount point (e.g., `/tecofs-m`).
    *   Restart containerd.
    *   Deploy a pod. You can use a simple pod definition:
        ```yaml
        apiVersion: v1
        kind: Pod
        metadata:
          name: test-shared-snapshot
          namespace: my-namespace # Or any namespace
        spec:
          containers:
          - name: test-container
            image: docker.io/library/nginx:latest # Or any test image
            command: ["/bin/sh", "-c", "echo 'Hello from shared snapshot' > /usr/share/nginx/html/index.html && sleep 3600"]
        ```
    *   **Inspect Snapshotter**:
        *   Identify the snapshot ID for the container's writable layer. You might find this through `ctr snapshots list` or by inspecting containerd logs (increase verbosity if needed).
        *   Check the shared storage path: `ls -la /<shared_snapshot_path>/my-namespace/test-shared-snapshot/test-container/<snapshot_id>/fs`
        *   You should see the container's writable changes here (e.g., the `index.html` file created by the command above).
        *   Check the local snapshotter root (`/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/`). For the active snapshot ID, you should *not* see `fs` and `work` directories here, though a base directory for the ID might exist as a marker.
    *   **Container Functionality**:
        *   `kubectl exec -it test-shared-snapshot -n my-namespace -- cat /usr/share/nginx/html/index.html` (or equivalent `crictl` command) should show the content written to the shared upperdir.
    *   **Mount Information**:
        *   On the node, try to find the mount point for the container's rootfs (e.g., using `findmnt` or by inspecting `/proc/<container_pid>/mountinfo`).
        *   Verify that the overlay mount options correctly list:
            *   `lowerdir` pointing to local image layers.
            *   `upperdir` pointing to the directory on your shared storage.
            *   `workdir` pointing to the directory on your shared storage.
    *   **Cleanup**:
        *   Delete the pod: `kubectl delete pod test-shared-snapshot -n my-namespace`
        *   Verify that the snapshot directory on the shared storage (`/<shared_snapshot_path>/my-namespace/test-shared-snapshot/test-container/<snapshot_id>/`) is removed.
        *   Verify that any local marker directory for that snapshot ID is also removed.

4.  **Edge Cases & Failure Modes**:
    *   Test behavior if the shared storage becomes unavailable.
    *   Test with different types of images and container configurations.
    *   Test concurrent container creation/deletion.
    *   Test directory quota limits on the shared storage – ensure the container receives "disk full" errors and is not evicted by Kubernetes due to this specific limit.

## 6. Considerations and Potential Issues

*   **Performance**: Storing `upperdir` on shared/networked storage might have performance implications compared to local SSDs, especially for I/O-intensive workloads. The current implementation does not focus on optimizing this.
*   **Shared Storage Reliability**: The availability and reliability of the shared storage are critical. If the shared storage is unavailable, containers relying on it for their `upperdir` will likely fail to start or operate correctly.
*   **Security**:
    *   Ensure appropriate permissions and security measures are in place for the shared storage mount point and the directories created by containerd.
    *   SELinux or AppArmor interactions with shared storage paths might need consideration depending on your environment. The current implementation relies on standard overlayfs behavior and SELinux label handling.
*   **Atomicity and Error Handling**: While standard snapshotter operations are transactional at the metadata level, filesystem operations on shared storage introduce more complex failure scenarios. The current modifications attempt basic cleanup on failure, but robust distributed error handling is a larger topic. The Post-Start hook logic for copying and cleaning up old directories must also be robust.
*   **Snapshot ID Uniqueness**: The design relies on containerd's snapshot IDs for creating unique paths on the shared storage.
*   **Compatibility**: This is a custom modification. It will need to be maintained and potentially updated with new versions of containerd.
*   **Lower Layer Locality**: This solution assumes that `lowerdir` (image layers) are always local to the node, managed by the standard snapshotter mechanisms. If parent image layers themselves were ever to be on shared storage (not the goal of this customization), the logic for resolving `lowerdir` paths in the `mounts` function would need further enhancement.

This customization provides a targeted solution for redirecting container writable layers to shared storage. Thorough testing in your specific environment is crucial before production deployment.
