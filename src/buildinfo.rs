// https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
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
