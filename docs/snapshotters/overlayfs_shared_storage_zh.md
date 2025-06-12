# 自定义 OverlayFS 快照程序以支持共享 Upper 目录

## 1. 目的

本文档描述了对 containerd 中标准 `overlayfs` 快照程序的一项定制。主要目的是在 Kubernetes 环境中更好地支持深度学习等场景下常用的 "Notebook" 类工作负载（例如模型开发、数据准备）。

**标准方法在 "Notebook" 工作负载中面临的挑战：**

Notebook 工作负载通常是单容器 Pod，用户在其中进行迭代式开发。常见的工作流程包括：
1.  从基础镜像启动 Notebook。
2.  在容器的可写层中安装新的软件包（例如通过 pip）、下载数据集、生成模型文件或中间数据。
3.  为了持久化这些状态，用户可能会将容器的文件系统更改提交（commit）到一个新的 OCI 镜像（例如使用 `nerdctl commit`）。然后将这个新镜像推送到远程镜像仓库。
4.  通过从这个提交后的镜像创建新 Pod 来恢复工作。

这种方法虽然可行，但对于 Notebook 场景会带来几个问题：
*   **镜像体积过大**：持续添加软件包、数据和生成的文件会导致提交的 OCI 镜像越来越大。
*   **OverlayFS 层数限制**：当这些大体积镜像被用作后续工作的基础时，频繁提交大的变更很容易达到或超过 overlayfs 的最大层数限制（通常是 128 层），从而阻止进一步提交或导致不稳定。
*   **低效的存储和传输**：推送和拉取非常大的镜像会消耗大量的网络带宽和镜像仓库存储空间。
*   **临时存储管理和数据丢失**：标准的 Kubernetes 临时存储限制 (`spec.resources.limits.ephemeral-storage`) 应用于容器的本地可写层。一旦超出限制，Pod 会被驱逐，导致该可写层中所有未提交的工作丢失。

**此定制的目标：**

此 `overlayfs` 快照程序定制旨在通过以下方式解决这些挑战：

1.  **重定向实时可写层**：Notebook 容器快照的 `upperdir`（可写层）和 `workdir` 直接存储在指定的共享文件存储系统上（例如，挂载在 `/tecofs-m` 的分布式文件系统）。`lowerdir`（基础镜像层）保留在本地文件系统上，以实现快速启动和同一节点上容器间镜像层的有效共享。
2.  **利用共享存储配额管理实时会话**：目标共享存储系统通常提供机制（例如通过 REST API 实现目录配额）来限制每个 Notebook 共享 `upperdir` 消耗的存储空间。
    *   关键在于，如果容器在共享存储上超出了其配额，它通常会在容器内部收到 "设备上没有剩余空间" 或 "超出磁盘配额" 的错误。这允许用户管理其空间（例如删除文件），而不会因为超出 Kubernetes 临时存储限制导致 Pod 被突然驱逐，从而保留了当前会话在共享存储上的状态。
3.  **促进更易于管理的镜像提交**：虽然用户仍然可以将其 Notebook 的状态（现在位于共享 `upperdir` 上）提交到新的 OCI 镜像中，但为了*仅仅避免因临时存储丢失而保存进展中工作*的频繁提交压力减小了。活动的工作已经存储在更健壮、有配额管理的共享存储上。

定制快照程序的主要特点是：
- **本地底层镜像**：基础镜像层被拉取并存储在本地。
- **活动会话的共享上层**：Notebook 容器的实时可写层 (`upperdir`) 和 `workdir` 在配置的共享存储路径上创建。
- **动态路径构建**：共享存储上的路径使用 Kubernetes Pod 元数据动态构建：`/<配置的共享路径>/<kubernetes_namespace>/<pod_name>/<container_name_in_pod>/<snapshot_id>/`。

这种方法为像 Notebook 这样的有状态、迭代式工作负载提供了一个更稳健和可管理的存储解决方案，特别是当对实时可写层的细粒度配额控制和防止临时存储驱逐非常重要时。

## 2. 配置

要启用和使用此功能，需要两个主要的配置步骤：

### 2.1. 配置 containerd 的 CRI 插件

CRI 插件需要知道共享存储的基础路径以及用于决定哪些 Pod 应用此功能的匹配规则。在 containerd v2.1 及更高版本中，这些选项应放置在 `config.toml` 文件的 `[plugins."io.containerd.cri.v1.runtime"]` 部分。

```toml
# 示例: /etc/containerd/config.toml
version = 3
# ... 其他全局设置 ...

[plugins]
  # ... 其他插件 ...

  [plugins."io.containerd.cri.v1.runtime"]
    # ... 其他运行时设置 ...

    # shared_snapshot_path 指定共享存储上的基础目录。
    # 这是启用此功能的必需项。
    shared_snapshot_path = "/tecofs-m"

    # (可选) 一个 RE2 兼容的正则表达式。如果设置，只有命名空间
    # 匹配此模式的 Pod 才会使用共享快照功能。
    # 示例: "^kubecube-.*" 匹配所有以 "kubecube-" 开头的命名空间。
    # 示例: ".*" 匹配所有命名空间。
    shared_snapshot_namespace_regex = "^kubecube-.*"

    # (可选) 一个 RE2 兼容的正则表达式。如果设置，只有 Pod 名称
    # 匹配此模式的 Pod 才会使用共享快照功能。
    # 示例: "^nb-.*" 匹配所有以 "nb-" 开头的 Pod 名称。
    shared_snapshot_pod_name_regex = "^nb-.*"

    [plugins."io.containerd.cri.v1.runtime".containerd]
      snapshotter = "overlayfs"
      # ... 其他 containerd 设置 ...
```

修改配置后，重启 containerd 服务：
```bash
sudo systemctl restart containerd
```

### 2.2. (隐式) Kubernetes 集成 - 如何应用

当 Kubernetes 请求创建容器时，CRI 插件现在会检查这些设置：
1. 它验证 `shared_snapshot_path` 是否已配置。
2. 它检查 Pod 的命名空间和名称是否分别与 `shared_snapshot_namespace_regex` 和 `shared_snapshot_pod_name_regex` 规则匹配。如果某个规则未设置，则认为该规则匹配。
3. 如果所有已配置的规则都通过，它会将必要的标签（`containerd.io/snapshot/use-shared-storage: "true"` 等）注入到快照选项中，以激活该容器的共享存储逻辑。

## 3. 工作原理 - 快照程序修改

`plugins/snapshots/overlay/overlay.go` 文件被修改以解释这些标签：

1.  **标签识别**：快照程序现在检查快照上是否存在 `containerd.io/snapshot/use-shared-storage: "true"` 以及其他相关标签（`k8s-namespace`、`k8s-pod-name`、`k8s-container-name`、`shared-disk-path`）。
2.  **路径确定**：
    *   如果这些标签存在于**活动**快照（容器的可写层）上：
        *   `upperdir` 路径构建为：`LABELS[shared-disk-path]/LABELS[k8s-namespace]/LABELS[k8s-pod-name]/LABELS[k8s-container-name]/<SNAPSHOT_ID>/fs`
        *   `workdir` 路径构建为：`LABELS[shared-disk-path]/LABELS[k8s-namespace]/LABELS[k8s-pod-name]/LABELS[k8s-container-name]/<SNAPSHOT_ID>/work`
    *   如果标签不存在，或者快照不是活动的可写层（例如，它是已提交的镜像层），快照程序将默认使用其标准的本地路径构建方式（例如，在 `/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/<SNAPSHOT_ID>/fs` 下）。
3.  **目录管理**：
    *   **创建**：在为匹配的活动快照执行 `Prepare` 阶段时，快照程序会直接在共享存储路径上创建 `fs` (`upperdir`) 和 `work` 目录。
    *   **删除时保留**：当使用共享快照功能的容器被移除时，快照程序将**在共享存储上保留其 `upperdir` 和 `workdir`**。我们特意跳过了对共享路径的 `os.RemoveAll` 调用。这是允许 Notebook 状态在重启后仍然存在的关键机制。这些目录的最终清理工作由一个外部编排过程负责，该过程知道 Notebook 实例何时被永久删除。
4.  **挂载**：当挂载共享快照时，`mounts` 操作会正确地为 `overlayfs` 挂载选项提供共享的 `upperdir` 和 `workdir`。`lowerdir` 将始终指向本地镜像层。

## 4. 会话管理、镜像提交和恢复 Notebook

此定制改变了容器实时可写层的存储方式。这对常见的 Notebook 工作流（如保存状态和恢复会话）有影响。

### 4.1. 实时会话状态

- 对于使用此功能启动的容器，会话期间所做的所有更改（安装软件包、创建文件、下载数据）都直接写入其在共享存储上的 `upperdir`：`/<来自配置的shared_snapshot_path>/<k8s_namespace>/<pod_name>/<container_name>/<snapshot_id>/fs`。
- 此目录受共享存储系统施加的配额限制，如果达到配额，将产生"磁盘已满"错误，而不是 Pod 被驱逐。

### 4.2. 提交到新的 OCI 镜像 (创建版本化快照)

用户仍然可以从其 Notebook 共享 `upperdir` 的状态创建版本化的、可移植的 OCI 镜像：
- 使用标准工具，如 `nerdctl commit <container_id> <new_image_tag>`（或等效的 containerd API）。
- Containerd 的提交过程作用于实时容器的挂载点。
- 由于定制容器的实时挂载已经使用了共享存储上的 `upperdir`，因此提交操作将读取此共享 `upperdir` 和本地 `lowerdirs`（来自基础镜像）以创建新的镜像层。
- 生成的 OCI 镜像中新层的大小将对应于共享 `upperdir` 中的数据。
- 这个新的 OCI 镜像随后可以被推送到镜像仓库（例如 Harbor），并用于启动具有此已提交状态的全新 Notebook 实例。

### 4.3. 从先前实例恢复 Notebook 会话

当前的插件实现会在 Kubernetes 每次创建新容器时分配一个**新的、唯一的快照 ID**（因此也会有一个新的、空的共享 `upperdir`）。要从同一逻辑 Notebook（由 K8s 命名空间、Pod 名称和容器名称标识）的*先前实例*恢复工作，需要一种数据复制机制。这对于快速重启或从意外的 Pod 终止中恢复（此时先前共享 `upperdir` 的数据仍然完好）特别有用。

**从先前实例恢复的工作流程 (在 Post-Start 钩子中)：**

1.  **新 Pod 实例 (`pod_B`)**：启动一个新的 Pod (`pod_B`)，旨在恢复/替换同一逻辑 Notebook 的先前实例 (`pod_A`)。它共享相同的 K8s 命名空间、预期的 Pod 名称和容器名称。
2.  **插件创建新的共享 `upperdir` (`P_target_host_path`)**：定制插件为 `pod_B` 创建一个新的、空的共享 `upperdir`：
    `P_target_host_path = /<共享路径配置>/<ns>/<pod_name>/<container_name>/<snap_B_id>/fs`
3.  **Post-Start 钩子执行**：`pod_B` 的 Post-Start 钩子（或 Init 容器）中的脚本执行以下逻辑：
    a.  **确定自身的 `upperdir`**：脚本通过解析 `/proc/self/mountinfo` 来查找其根 (`/`) 挂载的 `upperdir=` 选项，从而发现其*自己*新建的共享 `upperdir` 的主机路径 (`P_target_host_path`)。
        ```bash
        # Post-Start 脚本示例片段
        MY_OWN_UPPERDIR_HOST_PATH=$(awk '/ \/ overlay / && /upperdir=/ { for (i=1; i<=NF; i++) { if (match($i, /^upperdir=([^,]+)/, arr)) { print arr[1]; exit } } }' /proc/self/mountinfo)
        if [ -z "$MY_OWN_UPPERDIR_HOST_PATH" ]; then
            echo "错误：无法确定自身的共享 upperdir。将以空会话继续。" >&2
            exit 0 # 或 exit 1 以使钩子失败，具体取决于期望的行为
        fi
        MY_OWN_SNAPSHOT_ID=$(basename "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
        ```
    b.  **识别潜在的先前会话目录 (`P_source_host_path`)**：
        *   脚本构建此 Notebook 身份的快照目录所在的基础路径：
          `NOTEBOOK_SESSIONS_BASE_HOST_PATH=$(dirname "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")`
          (这将解析为 `/<共享路径配置>/<ns>/<pod_name>/<container_name>/`)
        *   它列出 `NOTEBOOK_SESSIONS_BASE_HOST_PATH` 中的子目录（潜在的快照 ID）。
        *   它搜索一个*不是*其自身 (`$MY_OWN_SNAPSHOT_ID`) 的目录。如果存在多个这样的目录（例如，由于多次不正常的关闭），则需要一个策略（例如，选择修改时间最新的那个，或期望只有一个）。为简单起见，此示例假设找到一个，或最近的一个有效目录。
            ```bash
            PREVIOUS_SNAPSHOT_ID=""
            # 简化：找到任何其他快照 ID。健壮的脚本会按 mtime 排序。
            for D_HOST_PATH in "$NOTEBOOK_SESSIONS_BASE_HOST_PATH"/* ; do
                if [ -d "${D_HOST_PATH}/fs" ]; then # 检查它是否像一个带有 fs 子目录的快照目录
                    SNAP_ID=$(basename "$D_HOST_PATH")
                    if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
                        # 基本方法：取找到的第一个。真实的脚本可能会比较 mtime。
                        PREVIOUS_SNAPSHOT_ID=$SNAP_ID
                        break
                    fi
                fi
            done
            ```
    c.  **复制和清理 (如果找到先前会话)**：
        *   如果找到了 `$PREVIOUS_SNAPSHOT_ID`：
            ```bash
            P_source_host_path="${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}/fs"
            echo "在 $P_source_host_path 找到先前会话。正在恢复到 $MY_OWN_UPPERDIR_HOST_PATH ..."
            rsync -avp --delete "${P_source_host_path}/" "${MY_OWN_UPPERDIR_HOST_PATH}/"
            if [ $? -eq 0 ]; then
                echo "恢复成功。正在清理旧会话目录：${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}"
                rm -rf "${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}"
            else
                echo "错误：恢复期间 rsync 失败。未清理旧会话数据。" >&2
                # 可能 exit 1 使 PostStart 钩子失败
            fi
            ```
        *   如果没有找到先前的会话目录，脚本会记录此情况并正常退出，允许 Notebook 以全新的（空的）共享 `upperdir` 启动。

**此恢复方法的假设：**
- Post-Start 钩子可以访问共享存储路径（例如，挂载到辅助容器中，或者主容器具有 `rsync` 等工具和权限）。
- 先前的 Pod 实例，如果它干净地关闭，可能已经被插件删除了。这种方法对于在*不正常*关闭后（旧的共享 `upperdir` 仍然存在）或编排层有意为此目的保留旧目录的情况下恢复最为有效。
- 如果存在多个旧目录，选择逻辑需要健壮（例如，基于时间戳或外部元数据）。

### 4.4. 显式保存/备份会话状态

对于更刻意、命名的 Notebook 状态备份或版本控制（不同于立即从先前状态恢复）：
1.  **识别实时共享 `upperdir`**：如 4.3.3.a 中所述，识别正在运行的 Notebook 的共享 `upperdir` 路径 (`/<共享路径配置>/.../<current_snap_id>/fs`)。
2.  **复制到用户定义的持久位置**：使用 `rsync` 或类似工具将此实时共享 `upperdir` 的内容复制到共享存储上的一个单独的、用户管理的目录中（例如，`/tecofs-m/notebook_backups/<user_name>/<notebook_name>/<version_tag>/`）。
3.  **从显式备份恢复**：要从此类备份恢复，新 Notebook Pod 中的 Post-Start 钩子将被配置（例如，通过环境变量）为指向此特定备份目录的路径，并将数据从那里 `rsync`到其新创建的共享 `upperdir` 中。

如果存在多个已保存版本，这提供了对恢复哪个状态的更多控制。

### 4.5. 与 "直接 UpperDir 操作" (来自 `docs/design_3.md`) 的对比

`docs/design_3.md` 中的方法涉及将*本地* `upperdir` `rsync` 到共享存储进行备份，然后在恢复时将其 `rsync` 回到*新的本地* `upperdir`。虽然它使用共享存储进行传输，但该设计中的实时容器使用本地临时存储，因此无法从活动会话的共享存储配额中受益以防止驱逐。本文档中详述的插件通过将*实时* `upperdir` 放置在共享存储上，直接解决了活动会话的配额管理和避免驱逐的目标。

## 5. 构建和测试

### 5.1. 构建 containerd
在进行了所述的代码更改后：
1.  导航到 containerd 源代码的根目录。
2.  构建 containerd (例如，使用 `make`)。

### 5.2. 安装
安装您自定义构建的 containerd 二进制文件，替换测试节点上现有的二进制文件。确保 systemd 单元文件或其他服务配置指向正确的二进制文件。

### 5.3. 验证步骤
1.  **配置检查**：
    *   确保在 containerd 配置中正确设置了 `shared_snapshot_path`。
    *   重启 containerd 并检查其日志中是否有与 CRI 插件或快照程序初始化相关的任何错误。

2.  **默认行为 (无共享路径)**：
    *   如果在 `config.toml` 中*未*设置 `shared_snapshot_path` 或为空，则部署一个简单的 Pod。
    *   验证容器的快照目录 (`fs`, `work`) 是否在默认的本地快照程序根目录中创建（例如，`/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/`）。
    *   验证容器功能和清理。

3.  **共享路径行为**：
    *   将 `shared_snapshot_path` 设置为您的共享存储挂载点（例如，`/tecofs-m`）。
    *   重启 containerd。
    *   部署一个 Pod。您可以使用一个简单的 Pod 定义：
        ```yaml
        apiVersion: v1
        kind: Pod
        metadata:
          name: test-shared-snapshot
          namespace: my-namespace # 或任何命名空间
        spec:
          containers:
          - name: test-container
            image: docker.io/library/nginx:latest # 或任何测试镜像
            command: ["/bin/sh", "-c", "echo 'Hello from shared snapshot' > /usr/share/nginx/html/index.html && sleep 3600"]
        ```
    *   **检查快照程序**：
        *   识别容器可写层的快照 ID。您可以通过 `ctr snapshots list` 或检查 containerd 日志（如果需要，增加详细程度）来找到它。
        *   检查共享存储路径：`ls -la /<shared_snapshot_path>/my-namespace/test-shared-snapshot/test-container/<snapshot_id>/fs`
        *   您应该在此处看到容器的可写更改（例如，上面命令创建的 `index.html` 文件）。
        *   检查本地快照程序根目录 (`/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/`)。对于活动快照 ID，您应该*不*在此处看到 `fs` 和 `work` 目录，尽管可能存在该 ID 的基础目录作为标记。
    *   **容器功能**：
        *   `kubectl exec -it test-shared-snapshot -n my-namespace -- cat /usr/share/nginx/html/index.html` (或等效的 `crictl` 命令) 应显示写入共享 upperdir 的内容。
    *   **挂载信息**：
        *   在节点上，尝试找到容器根文件系统的挂载点（例如，使用 `findmnt` 或检查 `/proc/<container_pid>/mountinfo`）。
        *   验证 overlay 挂载选项是否正确列出：
            *   `lowerdir` 指向本地镜像层。
            *   `upperdir` 指向共享存储上的目录。
            *   `workdir` 指向共享存储上的目录。
    *   **清理**：
        *   删除 Pod：`kubectl delete pod test-shared-snapshot -n my-namespace`
        *   验证共享存储上的快照目录 (`/<shared_snapshot_path>/my-namespace/test-shared-snapshot/test-container/<snapshot_id>/`) 是否已删除。
        *   验证该快照 ID 的任何本地标记目录是否也已删除。

4.  **边缘情况和故障模式**：
    *   测试共享存储不可用时的行为。
    *   使用不同类型的镜像和容器配置进行测试。
    *   测试并发容器创建/删除。
    *   测试共享存储上的目录配额限制——确保容器收到"磁盘已满"错误，并且不会因此特定限制而被 Kubernetes 驱逐。

## 6. 注意事项和潜在问题

*   **性能**：与本地 SSD 相比，将 `upperdir` 存储在共享/网络存储上可能会对 I/O 密集型工作负载产生性能影响。当前的实现并未侧重于优化这一点。
*   **共享存储可靠性**：共享存储的可用性和可靠性至关重要。如果共享存储不可用，依赖其 `upperdir` 的容器很可能无法启动或正常运行。
*   **安全**：
    *   确保为共享存储挂载点和 containerd 创建的目录设置适当的权限和安全措施。
*   **原子性和错误处理**：虽然标准快照程序操作在元数据级别是事务性的，但共享存储上的文件系统操作引入了更复杂的故障场景。当前的修改尝试在失败时进行基本清理，但健壮的分布式错误处理是一个更大的课题。用于复制和清理旧目录的 Post-Start 钩子逻辑也必须健壮。
*   **快照 ID 唯一性**：该设计依赖于 containerd 的快照 ID 在共享存储上创建唯一路径。
*   **兼容性**：这是一项自定义修改。它需要随着 containerd 新版本的发布进行维护和可能的更新。
*   **底层镜像层位置**：此解决方案假定 `lowerdir`（镜像层）始终位于节点本地，由标准快照程序机制管理。如果父镜像层本身曾经位于共享存储上（这不是此定制的目标），则 `mounts` 函数中解析 `lowerdir` 路径的逻辑将需要进一步增强。

此定制为将容器可写层重定向到共享存储提供了一个有针对性的解决方案。在生产部署之前，在您的特定环境中进行彻底测试至关重要。 

- [ ] 