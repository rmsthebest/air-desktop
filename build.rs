//! Records the commit this was built from, for the about page. A source
//! tarball has no git history: the packager can set `AIR_DESKTOP_COMMIT`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=AIR_DESKTOP_COMMIT");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    let commit = std::env::var("AIR_DESKTOP_COMMIT").ok().or_else(|| {
        let output = Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
    });
    println!(
        "cargo:rustc-env=AIR_DESKTOP_BUILD_COMMIT={}",
        commit.unwrap_or_default()
    );
}
