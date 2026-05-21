//! macOS-only vfkit lifecycle integration placeholder.

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires vfkit, a boot image, and host networking setup"]
fn real_vfkit_lifecycle_placeholder() {
    // PER 2 wires deterministic lifecycle seams first. A later PER should turn
    // this placeholder into a real vfkit boot/delete smoke once host assets are
    // standardized.
}
