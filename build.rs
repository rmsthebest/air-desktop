//! Records the commit this was built from, for the about page. A source
//! tarball has no git history: the packager can set `AIR_DESKTOP_COMMIT`.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=AIR_DESKTOP_COMMIT");
    // Our own repository only: an unpacked tarball can sit inside another
    // one (an AUR clone), whose commit says nothing about this source.
    let ours = Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join(".git")
        .exists();
    if ours {
        println!("cargo:rerun-if-changed=.git/HEAD");
        println!("cargo:rerun-if-changed=.git/refs/heads");
    }
    let commit = std::env::var("AIR_DESKTOP_COMMIT").ok().or_else(|| {
        if !ours {
            return None;
        }
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
