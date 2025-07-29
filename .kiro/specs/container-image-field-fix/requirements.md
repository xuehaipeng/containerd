# Requirements Document

## Introduction

This feature addresses a critical issue in a customized containerd implementation where some containers show "-" for the image field when using `ctr containers list`, while others correctly display their image reference. The issue occurs specifically in an environment with a shared storage overlayfs snapshotter that includes path optimization features. The goal is to identify the root cause of this metadata inconsistency and implement a fix that ensures all containers consistently display their image reference regardless of creation method.

## Requirements

### Requirement 1

**User Story:** As a system administrator, I want all containers to consistently display their image reference in `ctr containers list`, so that I can properly identify and manage containers regardless of how they were created.

#### Acceptance Criteria

1. WHEN a container is created via the `ctr` tool THEN the container SHALL display its correct image reference in `ctr containers list`
2. WHEN a container is created via Kubernetes/CRI THEN the container SHALL display its correct image reference in `ctr containers list`
3. WHEN containers are created with shared snapshots enabled THEN they SHALL display their image reference consistently with non-shared snapshot containers
4. WHEN containers are created with path optimization features THEN the image field population SHALL not be affected by hash-based path transformations

### Requirement 2

**User Story:** As a developer debugging container issues, I want to understand the exact code path differences between container creation methods, so that I can identify where metadata population diverges.

#### Acceptance Criteria

1. WHEN analyzing container creation flows THEN the system SHALL provide clear tracing of metadata population for both `ctr` tool and CRI plugin paths
2. WHEN examining container metadata structures THEN the system SHALL show exactly where the `Image` field is populated in each creation scenario
3. WHEN investigating shared snapshot integration THEN the system SHALL reveal any conditional logic that might prevent metadata assignment
4. IF metadata population fails THEN the system SHALL provide detailed error information about the failure point

### Requirement 3

**User Story:** As a containerd maintainer, I want to ensure that customizations to the overlay snapshotter don't interfere with core container metadata functionality, so that the system remains reliable and consistent.

#### Acceptance Criteria

1. WHEN the shared storage snapshotter is enabled THEN container metadata population SHALL function identically to the standard overlay snapshotter
2. WHEN path optimization features are active THEN the `r.meta.ImageRef = r.imageID` assignment SHALL execute successfully for all container creation paths
3. WHEN regex-based filtering is applied THEN metadata handling SHALL not be affected by namespace or pod name matching logic
4. WHEN path mapping system is used THEN container metadata retrieval SHALL remain unaffected by hash-based path transformations

### Requirement 4

**User Story:** As a system operator, I want a comprehensive fix that addresses the root cause without breaking existing shared storage functionality, so that I can deploy the solution with confidence.

#### Acceptance Criteria

1. WHEN the fix is implemented THEN all existing shared storage features SHALL continue to function correctly
2. WHEN containers are created after the fix THEN the `Image` field SHALL be populated consistently across all creation methods
3. WHEN testing the fix THEN both new and existing containers SHALL display their image references correctly
4. WHEN the fix is deployed THEN no regression SHALL occur in path optimization, shared storage persistence, or regex-based filtering features

### Requirement 5

**User Story:** As a quality assurance engineer, I want comprehensive test coverage for the fix, so that I can verify the solution works across all supported scenarios.

#### Acceptance Criteria

1. WHEN testing container creation via `ctr` tool THEN the image field SHALL be populated correctly in all test cases
2. WHEN testing container creation via Kubernetes/CRI THEN the image field SHALL be populated correctly with and without shared snapshots
3. WHEN testing with various shared snapshot configurations THEN metadata consistency SHALL be maintained across all configuration combinations
4. WHEN running regression tests THEN all existing functionality SHALL pass without modification