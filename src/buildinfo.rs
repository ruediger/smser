// https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Short commit the binary was built from, with a `-dirty` suffix when the
/// working tree had uncommitted tracked changes at build time.
pub fn git_hash() -> &'static str {
    env!("GIT_HASH")
}

/// Whether this binary was built from a modified working tree, meaning its
/// commit hash does not fully describe the source it came from.
pub fn is_dirty() -> bool {
    git_hash().ends_with("-dirty")
}

/// Returns version with git hash, e.g. "0.1.0 (abc1234)"
pub fn version_full() -> String {
    format!("{} ({})", version(), git_hash())
}

pub fn name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

pub fn repository() -> &'static str {
    env!("CARGO_PKG_REPOSITORY")
}

pub fn homepage() -> &'static str {
    env!("CARGO_PKG_HOMEPAGE")
}

pub fn description() -> &'static str {
    env!("CARGO_PKG_DESCRIPTION")
}
