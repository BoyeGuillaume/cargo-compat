//! Helpers for reading Cargo.toml manifests and Cargo.lock files, and modeling packages.
use std::path::{Path, PathBuf};

use cargo_util_schemas::manifest::{InheritableField, TomlManifest, TomlWorkspace};
use glob::Pattern;
use log::{debug, error, warn};
use semver::Version;
use serde::{Deserialize, Serialize, de::Error};
use toml::Table;

use crate::crates::Dependency;

pub fn read_cargo_manifest(path: &Path) -> Result<TomlManifest, crate::error::Error> {
    let mut path = path.to_path_buf();

    // Attempt to read the Cargo.toml file
    if path.is_dir() {
        path.push("Cargo.toml");
    }

    // Try to read the file and to parse it
    debug!("Reading Cargo manifest at: {}", path.to_string_lossy());
    let cargo_toml_content =
        std::fs::read_to_string(&path).map_err(|e| crate::error::Error::FileSystemError {
            path: path.to_string_lossy().to_string(),
            error: e.kind(),
        })?;

    // Parse the Cargo.toml content
    toml::from_str(&cargo_toml_content).map_err(|e| crate::error::Error::CargoManifestParseError {
        path: path.to_string_lossy().to_string(),
        error: e,
    })
}

/// A normalized view of a Cargo package with resolved dependencies.
#[derive(Debug, Clone)]
pub struct CargoPackage {
    pub manifest_path: PathBuf,
    pub version: Version,
    pub name: String,
    pub dependencies: Vec<Dependency>,
    pub build_dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
}

impl CargoPackage {
    pub fn from_target(
        manifest_path: &Path,
        manifest: TomlManifest,
        workspace: Option<&TomlWorkspace>,
    ) -> Result<Option<Self>, crate::error::Error> {
        if manifest.package.is_none() {
            return Ok(None);
        }

        let package = manifest.package.unwrap();
        if package.name.is_none() {
            return Ok(None);
        }

        let package_name = package.name.unwrap().to_string();
        let version = package.version.map(|v| match v {
            InheritableField::Value(v) => Ok(v),
            InheritableField::Inherit(_) => {
                if workspace.is_none() {
                    error!(
                        "Package {} is trying to inherit version from workspace, but no workspace is defined",
                        package_name
                    );
                    return Err(crate::error::Error::Other("Cannot inherit version from workspace".into()));
                }

                todo!()
            }
        }).unwrap_or(Ok(Version::new(0, 1, 0)))?;

        let dependencies = manifest
            .dependencies
            .unwrap_or_default()
            .iter()
            .map(|(name, dep)| Dependency::from_cargo_toml(name, dep, workspace))
            .collect::<Result<Vec<_>, _>>()?;

        let build_dependencies = manifest
            .build_dependencies
            .unwrap_or_default()
            .iter()
            .map(|(name, dep)| Dependency::from_cargo_toml(name, dep, workspace))
            .collect::<Result<Vec<_>, _>>()?;

        let dev_dependencies = manifest
            .dev_dependencies
            .unwrap_or_default()
            .iter()
            .map(|(name, dep)| Dependency::from_cargo_toml(name, dep, workspace))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(Self {
            manifest_path: manifest_path.to_path_buf(),
            version,
            name: package_name,
            dependencies,
            build_dependencies,
            dev_dependencies,
        }))
    }
}

/// Either a single package or a collection of packages from a workspace.
#[derive(Debug, Clone)]
pub enum Cargo {
    Single(CargoPackage),
    Workspace(Vec<CargoPackage>),
}

impl Cargo {
    pub fn from_path(path: &Path) -> Result<Self, crate::error::Error> {
        let path = path.to_path_buf();
        let main_manifest = read_cargo_manifest(&path)?;

        if main_manifest.workspace.is_none() {
            let package = CargoPackage::from_target(&path, main_manifest, None)?;

            if package.is_none() {
                error!(
                    "No package found in Cargo manifest at: {}",
                    path.to_string_lossy()
                );
                return Err("No package found in Cargo manifest".into());
            }

            return Ok(Cargo::Single(package.unwrap()));
        }

        // It's a workspace, read all member manifests
        let workspace = main_manifest.workspace.as_ref().unwrap();
        let positive_matchers = workspace
            .members
            .iter()
            .flat_map(|x| x.iter())
            .map(|s| Pattern::new(s).unwrap())
            .collect::<Vec<_>>();
        let negative_matchers = workspace
            .exclude
            .iter()
            .flat_map(|x| x.iter())
            .map(|s| Pattern::new(s).unwrap())
            .collect::<Vec<_>>();
        let mut packages = vec![];

        debug!(
            "Workspace positive matchers: {:?}, negative matchers: {:?}",
            positive_matchers
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>(),
            negative_matchers
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
        );

        // Read all Cargo.toml files in the workspace
        let pattern = path.join("**/Cargo.toml");
        for entry in glob::glob(pattern.to_str().unwrap()).unwrap() {
            if entry.is_err() {
                warn!("Failed to read entry in workspace: {:?}", entry);
            }
            let entry_path = entry.unwrap();

            // Skip the manifest of the main workspace
            if entry_path == path.join("Cargo.toml") {
                continue;
            }

            // Determine the relative path without the last Cargo.toml component
            let relative_path = entry_path.strip_prefix(&path).unwrap().parent().unwrap();
            let relative_path_str = relative_path.to_string_lossy();
            let is_included = positive_matchers
                .iter()
                .any(|p| p.matches(&relative_path_str));
            let is_excluded = negative_matchers
                .iter()
                .any(|p| p.matches(&relative_path_str));
            debug!(
                "Workspace member {}: is_included={}, is_excluded={}",
                relative_path_str, is_included, is_excluded
            );

            if is_included && !is_excluded {
                let member_manifest = read_cargo_manifest(&entry_path)?;

                if member_manifest.workspace.is_some() {
                    error!(
                        "Nested workspaces are not supported: {}",
                        entry_path.to_string_lossy()
                    );
                    return Err("Nested workspaces are not supported".into());
                }

                // Parse the member package
                let package =
                    CargoPackage::from_target(&entry_path, member_manifest, Some(workspace))?;
                if package.is_none() {
                    warn!(
                        "No package found in workspace member manifest at: {}",
                        entry_path.to_string_lossy()
                    );
                    continue;
                }

                // Add the package to the workspace
                packages.push(package.unwrap());
            }
        }

        Ok(Cargo::Workspace(packages))
    }
}

/// Package entries parsed from Cargo.lock
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CargoLockPackages {
    pub name: String,
    pub version: Version,
}

/// Minimal representation of a Cargo.lock file containing the packages array.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CargoLockFile {
    pub packages: Vec<CargoLockPackages>,
}

impl CargoLockFile {
    pub fn read_from_path(path: &Path) -> Result<Self, crate::error::Error> {
        debug!("Reading Cargo lock file at: {}", path.to_string_lossy());
        let lock_content =
            std::fs::read_to_string(path).map_err(|e| crate::error::Error::FileSystemError {
                path: path.to_string_lossy().to_string(),
                error: e.kind(),
            })?;
        let lock_content: Table = toml::from_str(&lock_content).map_err(|e| {
            crate::error::Error::CargoLockParseError {
                path: path.to_string_lossy().to_string(),
                error: e,
            }
        })?;

        let mut packages = vec![];

        // Parse the packages from the lock content
        for package in lock_content
            .get("package")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
        {
            let name = package
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::error::Error::CargoLockParseError {
                    path: path.to_string_lossy().to_string(),
                    error: toml::de::Error::custom("Missing 'name' field in package entry"),
                })?;

            let version_str = package
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::error::Error::CargoLockParseError {
                    path: path.to_string_lossy().to_string(),
                    error: toml::de::Error::custom("Missing 'version' field in package entry"),
                })?;
            let version = Version::parse(version_str).map_err(|e| {
                crate::error::Error::CargoLockParseError {
                    path: path.to_string_lossy().to_string(),
                    error: toml::de::Error::custom(format!(
                        "Invalid version '{}' in package '{}': {}",
                        version_str, name, e
                    )),
                }
            })?;

            debug!("Parsed package from Cargo.lock: {} {}", name, version);
            let cargo_lock_package = CargoLockPackages {
                name: name.to_string(),
                version,
            };
            packages.push(cargo_lock_package);
        }

        Ok(CargoLockFile { packages })
    }
}

// pub fn read_cargo(path: &Path)
