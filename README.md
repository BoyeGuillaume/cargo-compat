# cargo-compat

A Cargo subcommand/CLI to determine compatible dependency versions (SemVer) by resolving your crates and optionally building/testing to validate.

## Install

- From source (local checkout):
  - Install the binary: `cargo install --path .`
  - Or run without installing: `cargo run -- <command> [options]`

Binary name: `cargo-compat` (usable as `cargo compat ...`).

## Global options

- `--cache-dir <path>`: Override cache directory (default: `$HOME/.cache/cargo-compat`).
- `--cache-age <hours>`: Max age for cached crate info before refetch (default: `48`).
- `-v, --verbose` | `-q, --quiet` | `-s, --silent`: Adjust log verbosity.

## Main commands

- list-dependencies
  - What it does: Prints the direct dependencies (normal/build/dev) for a package or selected workspace members (no resolution).
  - Usage:
  - Single package in current dir: `cargo compat list-dependencies`
  - Explicit path: `cargo compat list-dependencies /path/to/Cargo.toml`
    - Workspace (must select members with glob patterns):
  - `cargo compat list-dependencies --include "crates/*"`

- resolve
  - What it does: Resolves all dependencies via crates.io for a package or selected workspace members, finds compatible versions, prints them, and updates Cargo.toml with the resolved versions. Git dependencies are skipped with a warning.
  - Useful flags:
    - `--cargo-path <path>`: Path to `cargo` to use (default: `cargo`).
    - `--release`: Build in release mode when validating.
    - `--no-test`: Build only, don’t run tests.
    - `-f, --features <feat>`: One or more features to enable during build/test (repeatable).
  - Usage examples:
  - Single package: `cargo compat resolve`
  - Workspace selection: `cargo compat resolve --include "crates/*"`
  - With custom cargo + release build: `cargo compat resolve --release --cargo-path /usr/bin/cargo`

- cache
  - Manage the local cache of crates.io metadata.
  - Subcommands:
    - `cache info`: Show cache location and summary.
    - `cache clean [--full]`: Remove expired entries, or wipe the cache with `--full`.
    - `cache fetch <crate> [<version-req>] [--force]`: Fetch crate info (respecting cache age unless `--force`).
  - Examples:
  - `cargo compat cache info`
  - `cargo compat cache clean`
  - `cargo compat cache clean --full`
  - `cargo compat cache fetch serde ^1`

## Notes

- Workspaces: when pointing at a workspace, you must specify one or more `--include` glob patterns that match package names.
- Output: logs are colorized and include timestamps; tune with `-v | -q | -s`.
- Caching: crate metadata is cached to reduce network calls; see `--cache-dir` and `--cache-age`.

## ⚠️ Please use responsibly

This tool can generate many requests to crates.io and docs.rs during resolution and validation. To avoid unnecessary load and cost:

- Prefer running against small subsets (use `--include` in workspaces).
- Keep a reasonable `--cache-age` and avoid forcing frequent refetches.
- Don't automate runs in tight loops or CI matrices unless necessary.

Be considerate—crates.io and docs.rs are shared community resources.
