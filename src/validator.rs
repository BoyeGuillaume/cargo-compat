use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use either::Either;
use log::info;
use semver::Version;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BuildOptions {
    pub packages: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    pub release: bool,
}

impl BuildOptions {
    pub fn arguments(&self) -> impl Iterator<Item = String> + '_ {
        self.packages
            .as_ref()
            .into_iter()
            .flat_map(|pkgs| pkgs.iter().map(|p| ["--package".to_string(), p.clone()]))
            .flatten()
            .chain(
                self.features
                    .as_ref()
                    .into_iter()
                    .flat_map(|feats| ["--features".to_string(), feats.join(",")]),
            )
            .chain(if self.release {
                Some("--release".to_string())
            } else {
                None
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TestOptions {
    pub filters: Vec<String>,
}

impl TestOptions {
    pub fn arguments(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once("--".to_string())
            .filter(|_| !self.filters.is_empty())
            .chain(
                self.filters
                    .iter()
                    .flat_map(|f| ["--test".to_string(), f.clone()]),
            )
    }
}

#[derive(Clone, Copy)]
pub enum Check<'a> {
    Build {
        build_opts: &'a BuildOptions,
    },
    RunTest {
        build_opts: &'a BuildOptions,
        test_opts: &'a TestOptions,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildFailure {
    pub cargo_error_code: i32,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationError {
    pub test_failured: bool,
    pub build_failure: Option<BuildFailure>,
    pub runned_at: DateTime<Utc>,
}

/// Trait for validating repositories
pub trait RepoValidator {
    fn set_dependency(&mut self, name: String, version: Version);

    fn set_dependencies(&mut self, dependencies: BTreeMap<String, Version>) {
        for (name, version) in dependencies {
            self.set_dependency(name, version);
        }
    }

    fn run_check(
        &mut self,
        check: Check,
    ) -> Result<(), Either<ValidationError, crate::error::Error>>;
}

/// A Cargo-based implementation of RepoValidator
pub struct CargoRepoValidator {
    cargo_command: String,
}

impl CargoRepoValidator {
    fn run_cargo_command(
        &self,
        args: &[String],
    ) -> Result<std::process::Output, crate::error::Error> {
        let elem = std::process::Command::new(self.cargo_command.as_str())
            .args(args)
            .output()
            .map_err(|e| crate::error::Error::AnyIoError(e))?;

        info!(
            "Running cargo command: {} {}...{}",
            self.cargo_command,
            args.iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(" "),
            if elem.status.success() {
                " OK"
            } else {
                " FAILED"
            }
        );

        Ok(elem)
    }

    pub fn new(cargo_command: Option<String>) -> Self {
        Self {
            cargo_command: cargo_command.unwrap_or_else(|| "cargo".to_string()),
        }
    }
}

impl RepoValidator for CargoRepoValidator {
    fn set_dependency(&mut self, name: String, version: Version) {
        self.run_cargo_command(&["add".to_string(), format!("{}@={}", name, version)])
            .unwrap();
    }

    fn run_check(
        &mut self,
        check: Check,
    ) -> Result<(), Either<ValidationError, crate::error::Error>> {
        let mut args = vec![];

        match check {
            Check::Build { build_opts } => {
                args.push("build".to_string());
                args.extend(build_opts.arguments());

                let output = self.run_cargo_command(&args).map_err(Either::Right)?;
                let status = output.status.code().unwrap_or(1);

                if status != 0 {
                    let build_failure = BuildFailure {
                        cargo_error_code: status,
                        message: String::from_utf8_lossy(&output.stderr).to_string(),
                    };

                    let validation_error = ValidationError {
                        test_failured: false,
                        build_failure: Some(build_failure),
                        runned_at: Utc::now(),
                    };

                    return Err(Either::Left(validation_error));
                }

                Ok(())
            }
            Check::RunTest {
                build_opts,
                test_opts: test_runner,
            } => {
                args.push("test".to_string());
                args.extend(build_opts.arguments());
                args.extend(test_runner.arguments());

                let output = self.run_cargo_command(&args).map_err(Either::Right)?;
                let status = output.status.code().unwrap_or(1);

                if status != 0 {
                    let validation_error = ValidationError {
                        test_failured: true,
                        build_failure: None,
                        runned_at: Utc::now(),
                    };

                    return Err(Either::Left(validation_error));
                }

                Ok(())
            }
        }
    }
}
