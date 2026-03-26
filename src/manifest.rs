use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub team_id: Option<String>,
    pub provider_id: Option<String>,
    #[serde(default)]
    pub source_roots: Vec<PathBuf>,
    #[serde(default)]
    pub toolchain: ToolchainManifest,
    pub platforms: BTreeMap<ApplePlatform, PlatformManifest>,
    pub targets: Vec<TargetManifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolchainManifest {
    pub xcode_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformManifest {
    pub deployment_target: String,
    pub profiles: BTreeMap<String, ProfileManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileManifest {
    pub configuration: String,
    pub distribution: DistributionKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplePlatform {
    Ios,
    Macos,
    Tvos,
    Visionos,
    Watchos,
}

impl std::fmt::Display for ApplePlatform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            ApplePlatform::Ios => "ios",
            ApplePlatform::Macos => "macos",
            ApplePlatform::Tvos => "tvos",
            ApplePlatform::Visionos => "visionos",
            ApplePlatform::Watchos => "watchos",
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DistributionKind {
    Development,
    AdHoc,
    AppStore,
    DeveloperId,
    MacAppStore,
}

impl DistributionKind {
    pub fn supports_submit(self) -> bool {
        matches!(
            self,
            DistributionKind::AppStore
                | DistributionKind::DeveloperId
                | DistributionKind::MacAppStore
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetManifest {
    pub name: String,
    pub kind: TargetKind,
    pub bundle_id: String,
    #[serde(default)]
    pub platforms: Vec<ApplePlatform>,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub swift_packages: Vec<SwiftPackageDependency>,
    pub entitlements: Option<PathBuf>,
    #[serde(default)]
    pub extension: Option<ExtensionManifest>,
}

impl TargetManifest {
    pub fn supports_platform(&self, platform: ApplePlatform) -> bool {
        self.platforms.is_empty() || self.platforms.contains(&platform)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetKind {
    App,
    AppExtension,
    Framework,
    StaticLibrary,
    DynamicLibrary,
    Executable,
    WatchApp,
    WatchExtension,
    WidgetExtension,
}

impl TargetKind {
    pub fn bundle_extension(self) -> &'static str {
        match self {
            TargetKind::App | TargetKind::WatchApp => "app",
            TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
                "appex"
            }
            TargetKind::Framework => "framework",
            TargetKind::StaticLibrary => "a",
            TargetKind::DynamicLibrary => "dylib",
            TargetKind::Executable => "",
        }
    }

    pub fn is_bundle(self) -> bool {
        !matches!(
            self,
            TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Executable
        )
    }

    pub fn is_embeddable(self) -> bool {
        matches!(
            self,
            TargetKind::AppExtension
                | TargetKind::WatchApp
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension
                | TargetKind::Framework
                | TargetKind::DynamicLibrary
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwiftPackageDependency {
    pub product: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub point_identifier: String,
    pub principal_class: String,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.platform != "apple" {
            bail!(
                "unsupported manifest platform `{}`; Orbit v2 requires `platform: \"apple\"`",
                self.platform
            );
        }

        if self.platforms.is_empty() {
            bail!("manifest must declare at least one Apple platform");
        }

        if self.targets.is_empty() {
            bail!("manifest must declare at least one target");
        }

        let target_names = self
            .targets
            .iter()
            .map(|target| target.name.as_str())
            .collect::<HashSet<_>>();

        for (platform, manifest) in &self.platforms {
            if manifest.deployment_target.trim().is_empty() {
                bail!("platform `{platform}` must declare a deployment_target");
            }
            if manifest.profiles.is_empty() {
                bail!("platform `{platform}` must declare at least one build profile");
            }
        }

        for target in &self.targets {
            if target.sources.is_empty()
                && !matches!(
                    target.kind,
                    TargetKind::Framework | TargetKind::StaticLibrary
                )
            {
                bail!(
                    "target `{}` must declare at least one source root",
                    target.name
                );
            }
            if target.bundle_id.trim().is_empty() {
                bail!("target `{}` must declare a bundle_id", target.name);
            }
            for dependency in &target.dependencies {
                if !target_names.contains(dependency.as_str()) {
                    bail!(
                        "target `{}` depends on unknown target `{dependency}`",
                        target.name
                    );
                }
            }
            match target.kind {
                TargetKind::AppExtension
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension => {
                    if target.extension.is_none() {
                        bail!(
                            "target `{}` of kind `{}` must define the `extension` block",
                            target.name,
                            serde_json::to_string(&target.kind).unwrap_or_default()
                        );
                    }
                }
                _ => {}
            }
        }

        let target_name_set = self
            .targets
            .iter()
            .map(|target| target.name.as_str())
            .collect::<HashSet<_>>();
        if target_name_set.len() != self.targets.len() {
            bail!("target names must be unique");
        }

        Ok(())
    }

    pub fn default_platform(&self) -> ApplePlatform {
        *self
            .platforms
            .keys()
            .next()
            .expect("validated manifest has at least one platform")
    }

    pub fn resolve_target<'a>(&'a self, name: Option<&str>) -> Result<&'a TargetManifest> {
        if let Some(name) = name {
            return self
                .targets
                .iter()
                .find(|target| target.name == name)
                .with_context(|| format!("unknown target `{name}`"));
        }

        self.targets
            .iter()
            .find(|target| matches!(target.kind, TargetKind::App))
            .or_else(|| self.targets.first())
            .context("manifest did not contain any targets")
    }

    pub fn resolve_platform_for_target(
        &self,
        target: &TargetManifest,
        explicit: Option<ApplePlatform>,
    ) -> Result<ApplePlatform> {
        if let Some(platform) = explicit {
            if !self.platforms.contains_key(&platform) {
                bail!("platform `{platform}` is not declared in the manifest");
            }
            if !target.supports_platform(platform) {
                bail!(
                    "target `{}` does not support platform `{platform}`",
                    target.name
                );
            }
            return Ok(platform);
        }

        if let Some(platform) = target
            .platforms
            .iter()
            .copied()
            .find(|platform| self.platforms.contains_key(platform))
        {
            return Ok(platform);
        }

        Ok(self.default_platform())
    }

    pub fn profile_for<'a>(
        &'a self,
        platform: ApplePlatform,
        name: &str,
    ) -> Result<&'a ProfileManifest> {
        self.platforms
            .get(&platform)
            .context("platform missing from manifest")?
            .profiles
            .get(name)
            .with_context(|| format!("unknown profile `{name}` for platform `{platform}`"))
    }

    pub fn topological_targets<'a>(
        &'a self,
        root_target: &'a str,
    ) -> Result<Vec<&'a TargetManifest>> {
        let by_name = self
            .targets
            .iter()
            .map(|target| (target.name.as_str(), target))
            .collect::<HashMap<_, _>>();
        let mut ordered = Vec::new();
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();

        fn visit<'a>(
            name: &'a str,
            by_name: &HashMap<&'a str, &'a TargetManifest>,
            ordered: &mut Vec<&'a TargetManifest>,
            visiting: &mut HashSet<&'a str>,
            visited: &mut HashSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name) {
                bail!("target dependency cycle detected at `{name}`");
            }
            let target = by_name
                .get(name)
                .with_context(|| format!("unknown target `{name}`"))?;
            for dependency in &target.dependencies {
                visit(dependency, by_name, ordered, visiting, visited)?;
            }
            visiting.remove(name);
            visited.insert(name);
            ordered.push(*target);
            Ok(())
        }

        visit(
            root_target,
            &by_name,
            &mut ordered,
            &mut visiting,
            &mut visited,
        )?;
        Ok(ordered)
    }

    pub fn shared_source_roots(&self) -> BTreeSet<PathBuf> {
        self.source_roots.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DistributionKind, Manifest};

    fn fixture(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    #[test]
    fn loads_example_simulator_manifest() {
        let manifest = Manifest::load(&fixture("examples/ios-simulator-app/orbit.json")).unwrap();
        let profile = manifest
            .profile_for(super::ApplePlatform::Ios, "development")
            .unwrap();
        assert!(matches!(
            profile.distribution,
            DistributionKind::Development
        ));
        assert_eq!(manifest.targets.len(), 1);
    }

    #[test]
    fn sorts_extension_dependencies_before_host_app() {
        let manifest = Manifest::load(&fixture("examples/ios-app-extension/orbit.json")).unwrap();
        let ordered = manifest
            .topological_targets("ExampleExtensionApp")
            .unwrap()
            .into_iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                "TunnelExtension".to_owned(),
                "ExampleExtensionApp".to_owned()
            ]
        );
    }
}
