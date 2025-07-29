# Implementation Plan

- [ ] 1. Add containerd.WithImageName option to container creation opts
  - Locate the opts array building section in createContainer function around line 490
  - Add `containerd.WithImageName(r.imageID)` to the opts array after WithContainerExtension
  - Ensure the imageID is properly populated before this assignment
  - Test that the core containerd Image field is now set correctly
  - _Requirements: 1.1, 1.2, 3.2_

- [ ] 2. Add validation to verify both Image fields are populated
  - Add validation after containerd.NewContainer call to check core Image field
  - Add validation to ensure CRI ImageRef field is still populated
  - Add debug logging to show both field values for troubleshooting
  - Create test to verify both fields are consistently set
  - _Requirements: 1.1, 1.2, 2.1, 2.2_

- [ ] 3. Investigate alternative WithImage approach using containerdImage
  - Evaluate using `containerd.WithImage(*r.containerdImage)` instead of WithImageName
  - Test which approach provides more consistent image reference information
  - Ensure the chosen approach works with shared snapshots and path optimization
  - Document the rationale for the chosen approach
  - _Requirements: 1.1, 1.2, 3.1, 3.2_

- [ ] 4. Handle edge cases where imageID might be empty or invalid
  - Add validation to ensure imageID is not empty before using WithImageName
  - Add fallback logic to use containerdImage.Name() if imageID is problematic
  - Ensure error handling doesn't bypass the Image field population
  - Test with various image reference formats and edge cases
  - _Requirements: 1.1, 1.2, 4.1_

- [ ] 5. Create comprehensive unit tests for both Image field population
  - Write test to verify core containerd Image field is set via WithImageName
  - Write test to verify CRI ImageRef field continues to be set correctly
  - Write test to verify both fields show the same image reference
  - Write test for shared snapshot scenarios with both fields populated
  - Write test for error scenarios ensuring both fields are handled correctly
  - _Requirements: 5.1, 5.2, 5.3, 5.4_

- [ ] 6. Implement integration tests comparing ctr and CRI container listing
  - Create test that creates containers via CRI and verifies ctr command shows image correctly
  - Create test that creates containers via ctr and verifies CRI ListContainers shows image correctly
  - Create test comparing image field display consistency between both creation methods
  - Test with shared snapshots enabled and disabled configurations
  - _Requirements: 5.1, 5.2, 5.3, 5.4_

- [ ] 7. Verify no regression in existing shared storage functionality
  - Run existing shared storage tests to ensure WithImageName doesn't break functionality
  - Test path optimization features continue working with the additional container option
  - Test regex-based filtering continues working with the new Image field population
  - Test shared storage persistence continues working with both Image fields populated
  - _Requirements: 4.1, 4.2, 4.3, 4.4_

- [ ] 8. Add comprehensive logging for debugging image field population
  - Add debug logging to show when core containerd Image field is set
  - Add debug logging to show when CRI ImageRef field is set
  - Add logging to track any discrepancies between the two fields
  - Add logging to help diagnose future image field population issues
  - _Requirements: 2.1, 2.2, 2.3, 2.4_

- [ ] 9. Test with various image reference formats and registries
  - Test with fully qualified image names (registry.example.com/image:tag)
  - Test with Docker Hub images (library/ubuntu:latest)
  - Test with local images and custom registries
  - Test with image digests instead of tags
  - Ensure both Image fields handle all formats consistently
  - _Requirements: 1.1, 1.2, 1.3, 1.4_

- [ ] 10. Implement final validation and cleanup
  - Remove any redundant or obsolete image field population code
  - Ensure all logging is appropriate for production use
  - Verify all test cases pass consistently with the fix
  - Create documentation explaining the dual Image field architecture
  - _Requirements: 4.1, 4.2, 4.3, 4.4_