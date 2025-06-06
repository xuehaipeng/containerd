/*
   Copyright The containerd Authors.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

package labels

const (
	// criContainerdPrefix is common prefix for cri-containerd
	criContainerdPrefix = "io.cri-containerd"
	// ImageLabelKey is the label key indicating the image is managed by cri plugin.
	ImageLabelKey = criContainerdPrefix + ".image"
	// ImageLabelValue is the label value indicating the image is managed by cri plugin.
	ImageLabelValue = "managed"
	// PinnedImageLabelKey is the label value indicating the image is pinned.
	PinnedImageLabelKey = criContainerdPrefix + ".pinned"
	// PinnedImageLabelValue is the label value indicating the image is pinned.
	PinnedImageLabelValue = "pinned"
	// ContainerKindLabel is the CRI label key for container kind annotation
	ContainerKindLabel = criContainerdPrefix + ".kind"
	// ContainerKindSandbox is the sandbox container kind label value
	ContainerKindSandbox = "sandbox"
	// ContainerKindContainer is the normal container kind label value
	ContainerKindContainer = "container"
	// ContainerMetadataExtension is an extension name that identify metadata of container in CreateContainerRequest
	ContainerMetadataExtension = criContainerdPrefix + ".container.metadata"
	// SandboxMetadataExtension is an extension name that identify metadata of sandbox in CreateContainerRequest
	SandboxMetadataExtension = criContainerdPrefix + ".sandbox.metadata"
	// SandboxIDLabel is the CRI label key for sandbox ID annotation
	SandboxIDLabel = criContainerdPrefix + ".sandbox.id"
	// ContainerImageNameLabel is the CRI label key for image name annotation
	ContainerImageNameLabel = criContainerdPrefix + ".image.name"
	// CheckpointNameLabel is the CRI label key for checkpoint name annotation
	CheckpointNameLabel = criContainerdPrefix + ".checkpoint.name"
	// CheckpointSourceContainerIDLabel is the CRI label key for checkpoint source container ID annotation
	CheckpointSourceContainerIDLabel = criContainerdPrefix + ".checkpoint.source.id"
	// ContainerImageLayersLabel is the CRI label key for image layers annotation
	ContainerImageLayersLabel = criContainerdPrefix + ".image.layers"
	// SandboxLogDirLabel is the CRI label key for sandbox log directory annotation
	SandboxLogDirLabel = criContainerdPrefix + ".sandbox.logdirectory"
	// SandboxNameLabel is the CRI label key for sandbox name annotation
	SandboxNameLabel = "io.kubernetes.cri.sandbox-name"
	// SandboxNamespaceLabel is the CRI label key for sandbox namespace annotation
	SandboxNamespaceLabel = "io.kubernetes.cri.sandbox-namespace"
	// SandboxUIDLabel is the CRI label key for sandbox UID annotation
	SandboxUIDLabel = "io.kubernetes.cri.sandbox-uid"
	// ContainerNameLabel is the CRI label key for container name annotation
	ContainerNameLabel = "io.kubernetes.cri.container-name"
	// ContainerLogPathLabel is the CRI label key for container log path annotation
	ContainerLogPathLabel = "io.kubernetes.cri.container-log-path"
	// CRI annotations for the runtime spec
	// ContainerTypeLabel is the CRI label key for container type ("sandbox" or "container")
	ContainerTypeLabel = "io.kubernetes.cri.container-type"
	// ContainerTypeSandbox is the sandbox container type label value
	ContainerTypeSandbox = "sandbox"
	// ContainerTypeContainer is the normal container type label value
	ContainerTypeContainer = "container"
	// PodAnnotations is the annotation name for pod annotations passed from CRI
	PodAnnotations = "io.kubernetes.cri.pod-annotations"
	// PodLabels is the annotation name for pod labels passed from CRI
	PodLabels = "io.kubernetes.cri.pod-labels"
	// PodLINUXOverhead is the annotation name for pod linux overhead passed from CRI
	PodLINUXOverhead = "io.kubernetes.cri.pod-linux-overhead"
	// PodWindowsOverhead is the annotation name for pod windows overhead passed from CRI
	PodWindowsOverhead = "io.kubernetes.cri.pod-windows-overhead"
	// Custom snapshotter labels for shared upperdir
	// LabelK8sNamespace is the CRI label key for k8s namespace annotation
	LabelK8sNamespace = "containerd.io/snapshot/k8s-namespace"
	// LabelK8sPodName is the CRI label key for k8s pod name annotation
	LabelK8sPodName = "containerd.io/snapshot/k8s-pod-name"
	// LabelK8sContainerName is the CRI label key for k8s container name annotation
	LabelK8sContainerName = "containerd.io/snapshot/k8s-container-name"
	// LabelSharedDiskPath is the CRI label key for shared disk path annotation
	LabelSharedDiskPath = "containerd.io/snapshot/shared-disk-path"
	// LabelUseSharedStorage is the CRI label key for use shared storage annotation
	LabelUseSharedStorage = "containerd.io/snapshot/use-shared-storage"
)
