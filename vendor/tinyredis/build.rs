use std::process::Command;

fn main() {
    // Capture git short SHA (falls back to "00000000" if not in a git repo)
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "00000000".to_string());

    // 1 if there are uncommitted changes, 0 otherwise
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| if o.stdout.is_empty() { "0" } else { "1" })
        .unwrap_or("0");

    println!("cargo:rustc-env=GIT_SHA1={sha}");
    println!("cargo:rustc-env=GIT_DIRTY={dirty}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
