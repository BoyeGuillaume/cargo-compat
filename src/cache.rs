use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Duration, Utc};
use log::debug;
use serde::{Deserialize, Serialize};

use crate::crates::Crate;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateCacheEntry {
    pub krate: Crate,
    pub last_fetched_at: DateTime<Utc>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct CrateCache {
    pub entries: BTreeMap<String, CrateCacheEntry>,
}

impl CrateCache {
    pub fn load_from_path(path: &Path) -> Result<Self, crate::error::Error> {
        // Try to read the cache file
        if !path.exists() {
            debug!("Cache file does not exist at: {}", path.to_string_lossy());
            Ok(CrateCache::default())
        } else {
            debug!("Loading cache from: {}", path.to_string_lossy());

            let reader =
                std::fs::File::open(path).map_err(|e| crate::error::Error::FileSystemError {
                    path: path.to_string_lossy().to_string(),
                    error: e.kind(),
                })?;
            let reader = std::io::BufReader::new(reader);

            serde_cbor::from_reader(reader).map_err(|e| {
                crate::error::Error::Other(
                    format!(
                        "Failed to deserialize cache from {}: {}",
                        path.to_string_lossy(),
                        e
                    )
                    .into(),
                )
            })
        }
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), crate::error::Error> {
        debug!("Saving cache to: {}", path.to_string_lossy());

        // If path does not exist, create parent directories
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    crate::error::Error::FileSystemError {
                        path: parent.to_string_lossy().to_string(),
                        error: e.kind(),
                    }
                })?;
            }
        };

        let writer =
            std::fs::File::create(path).map_err(|e| crate::error::Error::FileSystemError {
                path: path.to_string_lossy().to_string(),
                error: e.kind(),
            })?;
        let writer = std::io::BufWriter::new(writer);

        // Serialize the cache to CBOR format
        serde_cbor::to_writer(writer, self)
            .map_err(|e| {
                crate::error::Error::Other(
                    format!(
                        "Failed to serialize cache to {}: {}",
                        path.to_string_lossy(),
                        e
                    )
                    .into(),
                )
            })
            .inspect(|_| {
                debug!("Cache successfully saved to: {}", path.to_string_lossy());
            })
    }

    pub fn retrieve_packages_no_fetch(
        &mut self,
        crate_names: &[&str],
        cache_validity: Duration,
    ) -> BTreeMap<String, Crate> {
        let mut found_crates = BTreeMap::new();
        let now = Utc::now();

        for &name in crate_names {
            if let Some(entry) = self.entries.get(name) {
                let age = now.signed_duration_since(entry.last_fetched_at);
                if age < cache_validity {
                    debug!(
                        "Cache hit for crate '{}' (age: {} seconds)",
                        name,
                        age.num_seconds()
                    );
                    found_crates.insert(name.to_string(), entry.krate.clone());
                } else {
                    debug!(
                        "Cache stale for crate '{}' (age: {} seconds)",
                        name,
                        age.num_seconds()
                    );
                }
            }
        }

        found_crates
    }

    pub async fn retrives_packages_fetch(
        &mut self,
        crate_names: &[&str],
        cache_validity: Duration,
    ) -> Result<BTreeMap<String, Crate>, crate::error::Error> {
        let mut packages = self.retrieve_packages_no_fetch(crate_names, cache_validity);

        // Determine which crates need to be fetched
        let mut to_fetch = Vec::new();
        for &name in crate_names {
            if !packages.contains_key(name) {
                to_fetch.push(name);
            }
        }

        // Fetch missing crates
        if !to_fetch.is_empty() {
            let fetched_crates = crate::crates::download_crates(&to_fetch).await?;

            // Update the cache with fetched crates
            let now = Utc::now();
            for krate in fetched_crates.iter() {
                self.entries.insert(
                    krate.name.clone(),
                    CrateCacheEntry {
                        krate: krate.clone(),
                        last_fetched_at: now,
                    },
                );
            }

            // Combine previously found packages with newly fetched ones
            for krate in fetched_crates {
                packages.insert(krate.name.clone(), krate);
            }
        }

        Ok(packages)
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn filter_expired_entries(&mut self, cache_validity: Duration) {
        let now = Utc::now();
        self.entries.retain(|name, entry| {
            let age = now.signed_duration_since(entry.last_fetched_at);
            if age < cache_validity {
                true
            } else {
                debug!(
                    "Removing expired cache entry for crate '{}' (age: {} seconds)",
                    name,
                    age.num_seconds()
                );
                false
            }
        });
    }
}
