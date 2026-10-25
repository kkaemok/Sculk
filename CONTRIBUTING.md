# Contributing

Run the README checks before proposing changes. Keep fixtures synthetic or
irreversibly anonymized; never include account tokens or chat histories.
Every claimed command/event needs documented source evidence and a compatibility
status. Keep live tests opt-in and out of public CI. Do not copy restrictively
licensed sources. Contributions are under MIT OR Apache-2.0.

Fuzz decoder: install cargo-fuzz and a nightly toolchain, then
`cargo +nightly fuzz run decode -- -max_len=1048620` from the repository root.
The fuzz workspace is separate; ordinary tests also run 512 cases each of two
property tests. A long fuzz campaign remains a separate validation step.

Security/dependency review: consider cargo-audit for RustSec advisories and
cargo-deny for license/source policy before release. Neither substitutes for
protocol review. Never publish crates or releases as part of a routine change.

Before publication, verify registry name availability, package contents with
`cargo package --list`, dependency license notices, current advisories, semver,
and crate-level docs. This repository has not been published by this assignment.
