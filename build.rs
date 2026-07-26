use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}

fn main() {
    let git_hash = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(hash) if !hash.is_empty() => {
            // Mark builds made from a modified working tree, so a binary can
            // never claim a commit it was not actually built from. Only tracked
            // changes count: an untracked file cannot affect the build unless
            // something tracked references it, which is itself a tracked change.
            //
            // `None` here means the check failed rather than that the tree is
            // clean, so leave the hash unsuffixed rather than guess either way.
            match git(&["status", "--porcelain", "--untracked-files=no"]) {
                Some(status) if !status.is_empty() => format!("{}-dirty", hash),
                _ => hash,
            }
        }
        _ => "unknown".to_string(),
    };

    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    // The dirty flag depends on the source tree, not just on HEAD, so rerun
    // when anything that can change it changes. Without the source entries
    // cargo would recompile the crate without re-running this script, leaving a
    // stale hash on exactly the edit-then-build path this is meant to catch.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=build.rs");
    // HEAD and refs catch commits and checkouts; index catches staging.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}
