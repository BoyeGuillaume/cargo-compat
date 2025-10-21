//! Types and helpers for interacting with crates.io and representing crates and their versions.
use cargo_util_schemas::manifest::{PackageName, TomlDependency};
use chrono::{DateTime, Utc};
use log::{debug, error, info};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub crate_name: String,
    pub required_version: VersionReq,
    pub features: Vec<String>,
    pub git: bool,
    pub optional: bool,
}

impl Dependency {
    pub fn from_cargo_toml(
        name: &PackageName,
        dep: &cargo_util_schemas::manifest::InheritableDependency,
        workspace: Option<&cargo_util_schemas::manifest::TomlWorkspace>,
    ) -> Result<Self, crate::error::Error> {
        use cargo_util_schemas::manifest::InheritableDependency;

        let crate_name = name.to_string();
        let mut features = vec![];
        let mut optional = false;
        let mut git = false;

        if workspace.is_some() {
            debug!(
                "Resolving dependency {} with workspace inheritance",
                crate_name
            );
        }

        let normalized = match &dep {
            InheritableDependency::Value(v) => v,
            InheritableDependency::Inherit(v) => {
                if workspace.is_none() {
                    error!(
                        "Dependency {} is trying to inherit version from workspace, but no workspace is defined",
                        crate_name
                    );
                    return Err(crate::error::Error::Other(
                        "Cannot inherit version from workspace".into(),
                    ));
                }

                features.extend(v.features.iter().flat_map(|x| x.iter()).cloned());

                workspace
                    .unwrap()
                    .dependencies
                    .as_ref()
                    .and_then(|deps| deps.get(name))
                    .ok_or_else(|| {
                        crate::error::Error::Other(
                            format!("Dependency {} not found in workspace", crate_name).into(),
                        )
                    })?
            }
        };

        let required_version = match normalized {
            TomlDependency::Simple(version) => VersionReq::parse(version)
                .map_err(crate::error::Error::InvalidVersionSyntax)
                .inspect_err(|e| {
                    error!(
                        "Failed to parse version requirement for {}: {}",
                        crate_name, e
                    );
                }),
            TomlDependency::Detailed(toml_detailed_dependency) => {
                if let Some(ftrs) = &toml_detailed_dependency.features {
                    features = ftrs.clone();
                }
                optional = toml_detailed_dependency.optional.unwrap_or(false);
                git = toml_detailed_dependency.git.is_some();

                VersionReq::parse(toml_detailed_dependency.version.as_deref().unwrap_or("*"))
                    .map_err(crate::error::Error::InvalidVersionSyntax)
                    .inspect_err(|e| {
                        error!(
                            "Failed to parse version requirement for {}: {}",
                            crate_name, e
                        );
                    })
            }
        };

        Ok(Self {
            crate_name,
            required_version: required_version?,
            features,
            git,
            optional,
        })
    }
}

impl TryFrom<crates_io_api::Dependency> for Dependency {
    type Error = crate::error::Error;

    fn try_from(value: crates_io_api::Dependency) -> Result<Self, Self::Error> {
        Ok(Self {
            crate_name: value.crate_id,
            required_version: VersionReq::parse(&value.req)
                .map_err(crate::error::Error::InvalidVersionSyntax)?,
            features: value.features,
            optional: value.optional,
            git: false,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateVersion {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub yanked: bool,
    pub version: Version,
    pub checksum: String,
    pub dependencies: Option<Vec<Dependency>>,
}

impl TryFrom<crates_io_api::FullVersion> for CrateVersion {
    type Error = crate::error::Error;

    fn try_from(value: crates_io_api::FullVersion) -> Result<Self, Self::Error> {
        let dependencies = value
            .dependencies
            .into_iter()
            .map(|d| d.try_into())
            .collect::<Result<_, _>>()?;

        Ok(Self {
            created_at: value.created_at,
            updated_at: value.updated_at,
            yanked: value.yanked,
            version: Version::parse(&value.num)
                .map_err(crate::error::Error::InvalidVersionSyntax)?,
            dependencies: Some(dependencies),
            checksum: value.checksum,
        })
    }
}

impl TryFrom<crates_io_api::Version> for CrateVersion {
    type Error = crate::error::Error;

    fn try_from(value: crates_io_api::Version) -> Result<Self, Self::Error> {
        Ok(Self {
            created_at: value.created_at,
            updated_at: value.updated_at,
            yanked: value.yanked,
            version: Version::parse(&value.num)
                .map_err(crate::error::Error::InvalidVersionSyntax)?,
            dependencies: None,
            checksum: value.checksum,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Crate {
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub versions: Vec<CrateVersion>,
}

impl TryFrom<crates_io_api::CrateResponse> for Crate {
    type Error = crate::error::Error;

    fn try_from(value: crates_io_api::CrateResponse) -> Result<Self, Self::Error> {
        let versions = value
            .versions
            .into_iter()
            .map(|v| v.try_into())
            .collect::<Result<_, _>>()?;

        Ok(Self {
            name: value.crate_data.name,
            description: value.crate_data.description,
            created_at: value.crate_data.created_at,
            updated_at: value.crate_data.updated_at,
            versions,
        })
    }
}

impl TryFrom<crates_io_api::FullCrate> for Crate {
    type Error = crate::error::Error;

    fn try_from(value: crates_io_api::FullCrate) -> Result<Self, Self::Error> {
        let versions = value
            .versions
            .into_iter()
            .map(|v| v.try_into())
            .collect::<Result<_, _>>()?;

        Ok(Self {
            name: value.name,
            description: value.description,
            created_at: value.created_at,
            updated_at: value.updated_at,
            versions,
        })
    }
}

pub async fn download_crates(crate_names: &[&str]) -> Result<Vec<Crate>, crate::error::Error> {
    // Create the async-client
    let async_client = crates_io_api::AsyncClient::new(
        "rust-version-searcher (github.com/BoyeGuillaume/rust-version-searcher)",
        std::time::Duration::from_millis(500),
    )
    .unwrap();

    let atomic_usize = std::sync::atomic::AtomicUsize::new(0);

    // For each crate name, download the crate data
    debug!("Downloading crate data for: [{}]", crate_names.join(", "));
    let crates = crate_names
        .iter()
        .map(async |name| {
            let elem = async_client.get_crate(name).await;
            info!(
                "Downloaded crate data for {} ({}/{})",
                name,
                atomic_usize.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1,
                crate_names.len()
            );
            elem
        })
        .collect::<Vec<_>>();
    let crates = futures::future::join_all(crates)
        .await
        .into_iter()
        .map(|res| res.map_err(crate::error::Error::CratesIoApiError))
        .collect::<Result<Vec<_>, _>>()?;
    crates
        .into_iter()
        .map(|c| c.try_into())
        .collect::<Result<Vec<_>, _>>()
}

pub async fn download_full_crates(crate_names: &[&str]) -> Result<Vec<Crate>, crate::error::Error> {
    // Create the async-client
    let async_client = crates_io_api::AsyncClient::new(
        "rust-version-searcher (github.com/BoyeGuillaume/rust-version-searcher)",
        std::time::Duration::from_millis(500),
    )
    .unwrap();

    let atomic_usize = std::sync::atomic::AtomicUsize::new(0);

    // For each crate name, download the crate data
    debug!(
        "Downloading full crate data for: [{}]",
        crate_names.join(", ")
    );
    let crates = crate_names
        .iter()
        .map(async |name| {
            let elem = async_client.full_crate(name, true).await;
            info!(
                "Downloaded full crate data for {} ({}/{})",
                name,
                atomic_usize.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1,
                crate_names.len()
            );
            elem
        })
        .collect::<Vec<_>>();
    let crates = futures::future::join_all(crates)
        .await
        .into_iter()
        .map(|res| res.map_err(crate::error::Error::CratesIoApiError))
        .collect::<Result<Vec<_>, _>>()?;
    crates
        .into_iter()
        .map(|c| c.try_into())
        .collect::<Result<Vec<_>, _>>()
}
