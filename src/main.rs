use std::{collections::BTreeMap, path::PathBuf};

use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use log::{debug, info, warn};
use semver::VersionReq;

use crate::{
    cache::CrateCache,
    cargo::{Cargo, CargoPackage},
    crates::Crate,
};
pub mod cache;
pub mod cargo;
pub mod crates;
pub mod error;
pub mod resolver;

#[derive(Parser)]
#[command(author, version, author, about, long_about = None)]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Command,

    /// Cache directory to use for storing downloaded crate information and other data
    /// Defaults to $HOME/.cache/rust-version-searcher
    /// Use --cache-dir <path> to specify
    #[clap(long)]
    pub cache_dir: Option<String>,

    /// Age limit for cached crate information in hours. Defaults to 48 hours.
    /// Use --cache-age <hours> to specify
    #[clap(long, default_value_t = 48)]
    pub cache_age: u32,

    /// Whether to display verbose logging information
    /// Use --verbose or -v to enable
    #[clap(short, long)]
    pub verbose: bool,

    /// Quiet mode, suppress non-error output
    /// Use --quiet or -q to enable
    #[clap(short, long)]
    pub quiet: bool,

    /// Silent mode, suppress all output including errors
    /// Use --silent or -s to enable
    #[clap(short, long)]
    pub silent: bool,
}

#[derive(Subcommand)]
pub enum CacheCommand {
    /// Clean the cache directory by removing expired entries
    Clean {
        /// If set, removes the entire cache directory instead of just expired entries
        #[clap(long)]
        full: bool,
    },

    /// Display information about the current cache
    Info,

    /// Manually fetch a package and display information about it
    Fetch {
        /// Name of the crate to fetch
        crate_name: String,

        /// Additionally specify a Version requirement to filter the fetched versions
        requirement: Option<VersionReq>,

        /// Force re-fetching the crate information even if it is present in the cache
        #[clap(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum Command {
    /// Cache related commands
    #[clap(subcommand)]
    Cache(CacheCommand),

    /// List all of the dependencies of the specified Cargo package or workspace (without resolving them)
    ListDependencies {
        /// Path to the Cargo.toml file or workspace directory, defaults to current directory
        path: Option<String>,

        /// When reading a workspace, include only packages matching these glob patterns (can be used multiple times)
        /// Example: --include "crates/*" --include "tools/**"
        #[clap(long)]
        include: Vec<String>,
    },

    /// Resolve all dependencies of the specified Cargo package or workspace
    ///
    /// This will fetch information about all dependencies from crates.io then it will attempt to search for a compatible version
    /// based on the version requirements specified in the Cargo.toml file(s).
    ///
    /// Note: Git-based dependencies are not supported and will be skipped with a warning.
    ///
    Resolve {
        /// Path to the Cargo.toml file or workspace directory, defaults to current directory
        path: Option<String>,

        /// When reading a workspace, include only packages matching these glob patterns (can be used multiple times)
        /// Example: --include "crates/*" --include "tools/**"
        #[clap(long)]
        include: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    let args = Arguments::parse();
    setup_logger(&args);

    match &args.command {
        Command::Cache(cache_command) => {
            do_cache_command(cache_command, &args).await;
        }
        Command::ListDependencies { path, include } => {
            let targets = read_cargo_from_path_with_includes(&path, &include);

            for package in targets {
                println!("Package: {} (version: {})", package.name, package.version);
                println!("Manifest path: {}", package.manifest_path.display());
                println!("Dependencies:");
                for dep in &package.dependencies {
                    println!(
                        "  - {} {}{}{}",
                        dep.crate_name,
                        dep.required_version,
                        if dep.optional { " (optional)" } else { "" },
                        if dep.git { " (git)" } else { "" }
                    );
                }

                println!("Build Dependencies:");
                for dep in &package.build_dependencies {
                    println!(
                        "  - {} {}{}{}",
                        dep.crate_name,
                        dep.required_version,
                        if dep.optional { " (optional)" } else { "" },
                        if dep.git { " (git)" } else { "" }
                    );
                }

                println!("Dev Dependencies:");
                for dep in &package.dev_dependencies {
                    println!(
                        "  - {} {}{}{}",
                        dep.crate_name,
                        dep.required_version,
                        if dep.optional { " (optional)" } else { "" },
                        if dep.git { " (git)" } else { "" }
                    );
                }

                println!();
            }
        }
        Command::Resolve { path, include } => {
            do_resolve_command(&args, path, include).await;
        }
    }
}

async fn do_resolve_command(args: &Arguments, path: &Option<String>, include: &Vec<String>) {
    let targets = read_cargo_from_path_with_includes(path, include);

    // Read the cache
    let cache_paths = find_cache_path(&args.cache_dir);

    // Provide a list of all dependencies that must be resolved
    let mut all_dependencies = Vec::new();
    for package in &targets {
        for dep in &package.dependencies {
            if dep.git {
                warn!(
                    "Git dependency {} in package {} is not supported and will be skipped",
                    dep.crate_name, package.name
                );
                continue;
            }

            all_dependencies.push(dep.crate_name.clone());
        }
    }

    let package_informations = cached_resolver(args, cache_paths, all_dependencies).await;
}

async fn cached_resolver(
    args: &Arguments,
    cache_paths: CachePaths,
    all_dependencies: Vec<String>,
) -> BTreeMap<String, Crate> {
    // Load the cache
    let mut cache = CrateCache::load_from_path(&cache_paths.crate_cache).unwrap_or_else(|e| {
        warn!("Failed to load cache: {e}, starting with empty cache");
        CrateCache::default()
    });

    // Retrieve packages, fetching missing ones
    let packages_map = cache
        .retrives_packages_fetch(
            &all_dependencies
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            Duration::hours(args.cache_age as i64),
        )
        .await
        .unwrap_or_else(|e| {
            log::error!("Failed to retrieve packages: {}", e);
            // Write back the cache before exiting
            cache
                .save_to_path(&cache_paths.crate_cache)
                .unwrap_or_else(|e| {
                    log::warn!(
                        "Failed to save cache to {}: {}",
                        cache_paths.crate_cache.display(),
                        e
                    );
                });
            std::process::exit(1);
        });

    // Write back the cache
    cache
        .save_to_path(&cache_paths.crate_cache)
        .unwrap_or_else(|e| {
            log::warn!(
                "Failed to save cache to {}: {}",
                cache_paths.crate_cache.display(),
                e
            );
        });

    packages_map
}

async fn do_cache_command(command: &CacheCommand, args: &Arguments) {
    let cache_age_limit = Duration::hours(args.cache_age as i64);

    match command {
        CacheCommand::Clean { full } => {
            let cache_paths = find_cache_path(&args.cache_dir);
            if !cache_paths.base_cache_dir.is_dir() {
                info!(
                    "Cache directory {} does not exist, nothing to clean",
                    cache_paths.base_cache_dir.display()
                );
                return;
            }

            if *full {
                info!(
                    "Removing entire cache directory: {}",
                    cache_paths.base_cache_dir.display()
                );
                match std::fs::remove_dir_all(&cache_paths.base_cache_dir) {
                    Ok(_) => info!("Cache directory removed successfully"),
                    Err(e) => log::error!(
                        "Failed to remove cache directory {}: {}",
                        cache_paths.base_cache_dir.display(),
                        e
                    ),
                }
            } else {
                info!(
                    "Cleaning expired cache entries older than {} hours in {}",
                    args.cache_age,
                    cache_paths.base_cache_dir.display()
                );

                match CrateCache::load_from_path(&cache_paths.crate_cache) {
                    Ok(mut cache) => {
                        let initial_count = cache.entries.len();

                        cache.filter_expired_entries(cache_age_limit);

                        let removed_count = initial_count - cache.entries.len();
                        info!(
                            "Removed {} expired cache entries ({} total entries remaining)",
                            removed_count,
                            cache.entries.len()
                        );

                        if let Err(e) = cache.save_to_path(&cache_paths.crate_cache) {
                            log::error!("Failed to save cleaned cache: {}", e);
                            std::process::exit(1);
                        }
                    }
                    Err(e) => warn!(
                        "Failed to load cache from {}: {}, nothing to clean",
                        cache_paths.crate_cache.display(),
                        e
                    ),
                }
            }
        }

        CacheCommand::Info => {
            // Load the cache and display information
            let cache_paths = find_cache_path(&args.cache_dir);
            println!("Cache directory: {}", cache_paths.base_cache_dir.display());
            println!("Crate cache file: {}", cache_paths.crate_cache.display());

            // Load the cache
            let cache = match CrateCache::load_from_path(&cache_paths.crate_cache) {
                Ok(c) => c,
                Err(e) => {
                    log::error!(
                        "Failed to load cache from {}: {}",
                        cache_paths.crate_cache.display(),
                        e
                    );
                    std::process::exit(1);
                }
            };

            println!("Total cached crates: {}", cache.entries.len());
            for (crate_name, entry) in &cache.entries {
                let age = Utc::now() - entry.last_fetched_at;
                println!(
                    "- {}: last fetched at {} (age: {} hours)",
                    crate_name,
                    entry.last_fetched_at,
                    age.num_hours()
                );
            }
        }
        CacheCommand::Fetch {
            crate_name,
            requirement,
            force,
        } => {
            let cache_paths = find_cache_path(&args.cache_dir);
            let requirement = requirement.clone().unwrap_or_default();

            // Load the cache
            let mut cache =
                CrateCache::load_from_path(&cache_paths.crate_cache).unwrap_or_else(|e| {
                    warn!("Failed to load cache: {e}, starting with empty cache");
                    CrateCache::default()
                });

            // Fetch the crate
            let age_limit = if *force {
                Duration::hours(0)
            } else {
                cache_age_limit
            };

            let information = cache
                .retrives_packages_fetch(&[crate_name.as_ref()], age_limit)
                .await
                .unwrap_or_else(|e| {
                    log::error!("Failed to fetch crate {}: {}", crate_name, e);
                    std::process::exit(1);
                })
                .remove(crate_name)
                .unwrap();

            println!("Crate: {}", information.name);
            println!(
                "Description: {}",
                information.description.unwrap_or_default()
            );
            println!("Created at: {}", information.created_at);
            println!("Updated at: {}", information.updated_at);
            println!("A total of {} versions found", information.versions.len());
            println!("Versions:");
            for version in &information.versions {
                if requirement.matches(&version.version) {
                    println!(
                        "- {} (published at {}){}",
                        version.version,
                        version.created_at,
                        if version.yanked { " (yanked)" } else { "" }
                    )
                }
            }
        }
    }
}

fn setup_logger(args: &Arguments) {
    let level = if args.silent {
        log::LevelFilter::Off
    } else if args.quiet {
        log::LevelFilter::Error
    } else if args.verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}][{}] {}",
                record.level(),
                record.target(),
                message
            ))
        })
        .level(level)
        .chain(std::io::stdout())
        .apply()
        .unwrap();
}

struct CachePaths {
    base_cache_dir: PathBuf,
    crate_cache: PathBuf,
}

fn find_cache_path(cache_dir: &Option<String>) -> CachePaths {
    let base_cache_dir = cache_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|home| {
                    std::path::PathBuf::from(home)
                        .join(".cache")
                        .join("rust-version-searcher")
                })
                .unwrap_or_else(|_| {
                    warn!("HOME environment variable not set, using current directory for cache");
                    std::path::PathBuf::from(".rust-version-searcher-cache")
                })
        });
    debug!("Using base cache directory: {}", base_cache_dir.display());

    CachePaths {
        base_cache_dir: base_cache_dir.clone(),
        crate_cache: base_cache_dir.join("crate_cache.cbor"),
    }
}

fn read_cargo_from_path(path: &Option<String>) -> Cargo {
    let path = path
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    match Cargo::from_path(&path) {
        Ok(cargo) => cargo,
        Err(e) => {
            log::error!("Error reading Cargo manifest: {}", e);
            std::process::exit(1);
        }
    }
}

fn read_cargo_from_path_with_includes(
    path: &Option<String>,
    includes: &[String],
) -> Vec<CargoPackage> {
    let cargo = read_cargo_from_path(path);

    // Match include patterns when using libraries
    match cargo {
        Cargo::Single(cargo_package) => {
            if !includes.is_empty() {
                warn!("Include patterns are ignored when processing a single package");
            }

            vec![cargo_package]
        }
        Cargo::Workspace(cargo_packages) => {
            if includes.is_empty() {
                log::error!(
                    "No include patterns specified for workspace. Workspace processing requires at least one --include pattern."
                );
                std::process::exit(1);
            }

            let include_patterns = includes
                .iter()
                .map(|p| glob::Pattern::new(p).unwrap())
                .collect::<Vec<_>>();

            let targets = cargo_packages
                .iter()
                .filter(|pkg| {
                    include_patterns
                        .iter()
                        .any(|pat| pat.matches(&pkg.name.as_ref()))
                })
                .cloned()
                .collect::<Vec<_>>();

            if targets.is_empty() {
                log::error!(
                    "No packages in the workspace matched the provided include patterns: {:?}. Available packages: {:?}",
                    includes,
                    cargo_packages
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<_>>()
                );
                std::process::exit(1);
            }

            targets
        }
    }
}
