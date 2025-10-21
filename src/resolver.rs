//! Core algorithm for selecting the most permissive semver requirements that still validate.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::atomic::AtomicUsize,
};

use either::Either;
use log::{debug, info, warn};
use semver::{Comparator, Prerelease, Version, VersionReq};

use crate::{
    cargo::CargoPackage,
    crates::Crate,
    error::Error,
    validator::{BuildOptions, Check, RepoValidator, TestOptions},
};

/// Resolves dependency version requirements by testing candidate versions against the repository.
pub struct Resolver {
    pub targets: Vec<CargoPackage>,
    pub path: PathBuf,
    pub package_informations: BTreeMap<String, Crate>,
    pub validator: Box<dyn RepoValidator>,
    pub build_opts: BuildOptions,
    pub test_opts: Option<TestOptions>,

    packages_requirements: BTreeMap<String, VersionReq>,
    packages: BTreeMap<String, Version>,
}

impl Resolver {
    /// Create a new resolver for a set of targets and available crate metadata.
    pub fn new(
        targets: Vec<CargoPackage>,
        path: PathBuf,
        package_informations: BTreeMap<String, Crate>,
        validator: Box<dyn RepoValidator>,
        build_opts: BuildOptions,
        test_opts: Option<TestOptions>,
    ) -> Self {
        Resolver {
            targets,
            path,
            package_informations,
            validator,
            build_opts,
            test_opts,
            packages_requirements: BTreeMap::new(),
            packages: BTreeMap::new(),
        }
    }

    /// Pre-populate selections using Cargo.lock when possible, otherwise pick latest matching versions.
    pub fn populate_default(&mut self) -> Result<(), Error> {
        // First read the Cargo.lock file
        let cargo_lock_path = self.path.join("Cargo.lock");
        let cargo_lock_file = crate::cargo::CargoLockFile::read_from_path(&cargo_lock_path)
            .inspect_err(|err| {
                warn!("Failed to read Cargo.lock: {err}");
            })
            .ok();

        // Secondly, find all of the dependencies we need to resolve
        for target in &self.targets {
            for dependency in &target.dependencies {
                if dependency.git {
                    warn!(
                        "Git packages are not supported. Ignoring package: {}",
                        dependency.crate_name
                    );
                    continue;
                }

                self.packages_requirements.insert(
                    dependency.crate_name.clone(),
                    dependency.required_version.clone(),
                );
            }
        }

        // Now, try to resolve each package using the Cargo.lock file
        if let Some(lock_file) = cargo_lock_file {
            for (pkg_name, version_req) in &self.packages_requirements {
                if let Some(lock_pkg) = lock_file
                    .packages
                    .iter()
                    .find(|p| &p.name == pkg_name && version_req.matches(&p.version))
                {
                    debug!(
                        "Resolved package '{}' to version '{}' using Cargo.lock",
                        pkg_name, lock_pkg.version
                    );

                    self.packages
                        .insert(pkg_name.clone(), lock_pkg.version.clone());
                }
            }
        }

        // Finally display all unresolved packages, pick the latest version available
        for (pkg_name, version_req) in &self.packages_requirements {
            if !self.packages.contains_key(pkg_name)
                && let Some(krate) = self.package_informations.get(pkg_name)
                && let Some(latest_version) = krate
                    .versions
                    .iter()
                    .filter(|v| version_req.matches(&v.version))
                    .max_by_key(|a| a.version.clone())
            {
                debug!(
                    "Package '{}' not found in Cargo.lock. Selected latest version '{}' from crates.io",
                    pkg_name, latest_version.version
                );
            }
        }

        Ok(())
    }

    /// Run the resolution process and return the final semver requirements by crate name.
    pub fn resolve(&mut self) -> Result<&BTreeMap<String, VersionReq>, Error> {
        // First of all search for a configuration that works
        // We assume the default configuration is the one that works
        for (package_name, crate_info) in self.package_informations.iter() {
            let version = &self.packages[package_name];

            let version = crate_info.versions.iter().find(|v| &v.version == version);
            if version.is_none() || version.unwrap().yanked {
                warn!(
                    "The selected version '{}' for package '{}' is invalid or yanked.",
                    version.unwrap().version,
                    package_name
                );

                // Find the latest non-yanked version
                let non_yanked_version = crate_info
                    .versions
                    .iter()
                    .filter(|v| !v.yanked)
                    .filter(|v| {
                        let req = self.packages_requirements.get(package_name).unwrap();
                        req.matches(&v.version)
                    })
                    .max_by_key(|v| v.version.clone())
                    .or_else(|| crate_info.versions.iter().filter(|v| !v.yanked).next_back())
                    .ok_or_else(|| {
                        crate::error::Error::Other(
                            format!("No available versions for package '{}'", package_name).into(),
                        )
                    })?;

                self.packages
                    .insert(package_name.clone(), non_yanked_version.version.clone());
                info!(
                    "Selected non-yanked version '{}' for package '{}'",
                    non_yanked_version.version, package_name
                );
            }
        }

        let check = if let Some(test_opts) = &self.test_opts {
            Check::RunTest {
                build_opts: &self.build_opts,
                test_opts,
            }
        } else {
            Check::Build {
                build_opts: &self.build_opts,
            }
        };

        self.validator.set_dependencies(self.packages.clone());
        self.validator.run_check(check).map_err(|e| match e {
            Either::Left(validation_error) => {
                log::error!(
                    "Cannot resolve packages because default configuration is invalid: {:?}",
                    validation_error
                );
                crate::error::Error::Other(
                    format!("Validation error: {:?}", validation_error).into(),
                )
            }
            Either::Right(err) => err,
        })?;

        // Finally perform the resolution
        for (package_name, package_information) in self.package_informations.iter() {
            let version = self.packages[package_name].clone();

            let version_req = resolve_package(
                package_name,
                version.clone(),
                package_information,
                self.validator.as_mut(),
                check,
            )?;

            self.packages_requirements
                .insert(package_name.clone(), version_req);
        }

        Ok(&self.packages_requirements)
    }

    /// Clean any temporary files or processes created by the validator.
    pub fn clean(&mut self) {
        self.validator.clean();
    }

    /// Persist resolution output back to the repository (e.g., via cargo-edit add commands).
    pub fn write_cargo_toml_with_resolved_versions(&mut self) -> Result<(), Error> {
        for (package_name, version) in &self.packages_requirements {
            self.validator
                .set_dependency_req(package_name.clone(), version.clone());
        }

        Ok(())
    }
}

fn resolve_package(
    package_name: &str,
    version: Version,
    package_information: &Crate,
    validator: &mut dyn RepoValidator,
    check: Check,
) -> Result<VersionReq, Error> {
    // Acording to semver semantics, patch versions can be updated freely when using caret requirements
    // We need to minimize the number of comparisons as they are very expensive
    // A package with 300 versions will need 2log2(300) ~= 18 comparisons in the worst case to find the correct version bounds
    //
    // To minimize the number of comparisons, we therefore perform binary search on the major.minor versions first. Once we found
    // a sequence major1.minor1.0 to major2.minor2.0 we then check that major1.minor1.last_patch also compiles, and similarly for major2.minor2.last_patch
    // If this fails, we perform binary search on the patch versions between major1.minor1.last_patch and major2.minor2.last_patch
    //
    // Similarly we can do the same for the major versions, in other words we binary search in a subset
    let all_versions: Vec<Version> = package_information
        .versions
        .iter()
        .filter(|v| !v.yanked)
        .map(|v| v.version.clone())
        .collect();

    let comparison_count = AtomicUsize::new(0);
    let mut old_check: BTreeMap<Version, bool> = BTreeMap::new();

    let mut validator_fn = |version: &Version| {
        if old_check.contains_key(version) {
            return Ok(*old_check.get(version).unwrap());
        }

        comparison_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        std::thread::sleep(std::time::Duration::from_millis(500)); // Throttle comparisons to avoid overwhelming the system

        validator.set_dependency(package_name.to_string(), version.clone());
        match validator.run_check(check) {
            Err(Either::Left(_)) => {
                old_check.insert(version.clone(), false);
                info!(
                    "Checking package '{}' with version '{}'...FAIL",
                    package_name, version
                );
                Ok(false)
            }
            Err(Either::Right(e)) => Err(e),
            Ok(()) => {
                old_check.insert(version.clone(), true);
                info!(
                    "Checking package '{}' with version '{}'...OK",
                    package_name, version
                );
                Ok(true)
            }
        }
    };

    let output_req = binary_search_bounds(&version, all_versions, &mut validator_fn)?;

    // Determine number of comparisons
    let total_comparisons = comparison_count.load(std::sync::atomic::Ordering::Acquire);
    info!(
        "Resolved package '{}' to version requirement '{}' using {} comparisons ({} matching versions)",
        package_name,
        output_req,
        total_comparisons,
        package_information
            .versions
            .iter()
            .filter(|v| !v.yanked && output_req.matches(&v.version))
            .count()
    );

    // Set dependency back to default
    validator.set_dependency(package_name.to_string(), version);
    Ok(output_req)
}

fn binary_search_bounds(
    initial_version: &Version,
    mut versions: Vec<Version>,
    validator: &mut impl FnMut(&Version) -> Result<bool, Error>,
) -> Result<VersionReq, Error> {
    // First filter out versions that do not match the requirement and remove duplicates
    versions.sort();

    // Find the index of the initial version
    let mut left_invalid = None;
    let mut left_valid = versions
        .iter()
        .position(|v| *v == *initial_version)
        .unwrap();
    let mut right_valid = left_valid;
    let mut right_invalid = None;

    // Binary search on the left side
    loop {
        match left_invalid {
            Some(invalid_index) => {
                let mid_index = (invalid_index + left_valid) / 2;
                if mid_index == left_valid || mid_index == invalid_index {
                    break;
                }

                let is_valid = validator(&versions[mid_index])?;
                if is_valid {
                    left_valid = mid_index;
                } else {
                    left_invalid = Some(mid_index);
                }
            }
            None => {
                let is_valid = validator(&versions[0])?;
                if is_valid {
                    break; // Not left-invalid
                } else {
                    left_invalid = Some(0);
                }
            }
        }
    }

    // Binary search on the right side
    loop {
        match right_invalid {
            Some(invalid_index) => {
                let mid_index = (invalid_index + right_valid) / 2;
                if mid_index == right_valid || mid_index == invalid_index {
                    break;
                }

                let is_valid = validator(&versions[mid_index])?;
                if is_valid {
                    right_valid = mid_index;
                } else {
                    right_invalid = Some(mid_index);
                }
            }
            None => {
                let is_valid = validator(&versions[versions.len() - 1])?;
                if is_valid {
                    break; // Not right-invalid
                } else {
                    right_invalid = Some(versions.len() - 1);
                }
            }
        }
    }

    // Construct the resulting VersionReq
    let mut bounds = vec![];

    if left_invalid.is_some() {
        let min_version = versions[left_valid].clone();

        bounds.push(Comparator {
            op: semver::Op::GreaterEq,
            major: min_version.major,
            minor: Some(min_version.minor),
            patch: Some(min_version.patch),
            pre: min_version.pre.clone(),
        });
    }

    if right_invalid.is_some() {
        let max_version = versions[right_valid].clone();

        bounds.push(Comparator {
            op: semver::Op::LessEq,
            major: max_version.major,
            minor: Some(max_version.minor),
            patch: Some(max_version.patch),
            pre: max_version.pre.clone(),
        });
    }
    let version_req = VersionReq {
        comparators: bounds,
    };

    // Simplify the version requirement if possible
    Ok(simplify_version_req(version_req, &versions))
}

fn simplify_version_req(version_req: VersionReq, versions: &[Version]) -> VersionReq {
    // If the version_req matches all versions, return "*"
    if version_req.comparators.is_empty() || versions.iter().all(|v| version_req.matches(v)) {
        return VersionReq::STAR;
    }

    // If the version_req matches only one version, return "=x.y.z"
    let matching_versions: BTreeSet<Version> = versions
        .iter()
        .filter(|v| version_req.matches(v))
        .cloned()
        .collect();
    if matching_versions.len() == 1 {
        let v = matching_versions.iter().next().unwrap();
        return VersionReq {
            comparators: vec![Comparator {
                op: semver::Op::Exact,
                major: v.major,
                minor: Some(v.minor),
                patch: Some(v.patch),
                pre: v.pre.clone(),
            }],
        };
    }

    // Try simplify to caret requirements (attempt)
    let mut proposal_caret = VersionReq {
        comparators: vec![Comparator {
            op: semver::Op::Caret,
            major: version_req.comparators[0].major,
            minor: None,
            patch: None,
            pre: Prerelease::EMPTY,
        }],
    };

    let check_proposal = |proposal: &VersionReq| {
        let hashset = versions
            .iter()
            .filter(|v| proposal.matches(v))
            .cloned()
            .collect::<BTreeSet<_>>();
        hashset == matching_versions
    };

    if check_proposal(&proposal_caret) {
        return proposal_caret;
    }

    // Make caret more specific if possible
    proposal_caret.comparators[0].minor = Some(version_req.comparators[0].minor.unwrap_or(0));
    if check_proposal(&proposal_caret) {
        return proposal_caret;
    }

    // Make caret even more specific if possible
    proposal_caret.comparators[0].patch = Some(version_req.comparators[0].patch.unwrap_or(0));
    if check_proposal(&proposal_caret) {
        return proposal_caret;
    }

    // If no simplification was possible, return the original version_req
    version_req
}
