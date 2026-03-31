use std::path::PathBuf;
use std::process::Command;

use crate::manifest::ApplePlatform;
use crate::util::command_output;
use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DestinationKind {
    Simulator,
    Device,
}

impl DestinationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DestinationKind::Simulator => "simulator",
            DestinationKind::Device => "device",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub platform: ApplePlatform,
    pub destination: DestinationKind,
    pub sdk_name: String,
    pub sdk_path: PathBuf,
    pub deployment_target: String,
    pub architecture: String,
    pub target_triple: String,
}

#[derive(Debug, Clone)]
pub struct BundleBuildMetadata {
    pub build_machine_os_build: String,
    pub compiler: String,
    pub platform_build: String,
    pub platform_name: String,
    pub platform_version: String,
    pub sdk_build: String,
    pub sdk_name: String,
    pub xcode: String,
    pub xcode_build: String,
}

impl Toolchain {
    pub fn resolve(
        platform: ApplePlatform,
        deployment_target: &str,
        destination: DestinationKind,
    ) -> Result<Self> {
        let sdk_name = match (platform, destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => "iphonesimulator",
            (ApplePlatform::Ios, DestinationKind::Device) => "iphoneos",
            (ApplePlatform::Macos, _) => "macosx",
            (ApplePlatform::Tvos, DestinationKind::Simulator) => "appletvsimulator",
            (ApplePlatform::Tvos, DestinationKind::Device) => "appletvos",
            (ApplePlatform::Visionos, DestinationKind::Simulator) => "xrsimulator",
            (ApplePlatform::Visionos, DestinationKind::Device) => "xros",
            (ApplePlatform::Watchos, DestinationKind::Simulator) => "watchsimulator",
            (ApplePlatform::Watchos, DestinationKind::Device) => "watchos",
        }
        .to_owned();

        let sdk_path = command_output(Command::new("xcrun").args([
            "--sdk",
            sdk_name.as_str(),
            "--show-sdk-path",
        ]))?;
        let sdk_path = PathBuf::from(sdk_path.trim());

        let host_architecture = host_architecture()?;
        let architecture = match (platform, destination) {
            (ApplePlatform::Ios, DestinationKind::Device)
            | (ApplePlatform::Tvos, DestinationKind::Device)
            | (ApplePlatform::Visionos, DestinationKind::Device) => "arm64".to_owned(),
            (ApplePlatform::Watchos, DestinationKind::Device) => "arm64_32".to_owned(),
            _ => host_architecture.clone(),
        };
        let target_triple = match (platform, destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => {
                format!("{architecture}-apple-ios{deployment_target}-simulator")
            }
            (ApplePlatform::Ios, DestinationKind::Device) => {
                format!("arm64-apple-ios{deployment_target}")
            }
            (ApplePlatform::Macos, _) => format!("{architecture}-apple-macosx{deployment_target}"),
            (ApplePlatform::Tvos, DestinationKind::Simulator) => {
                format!("{architecture}-apple-tvos{deployment_target}-simulator")
            }
            (ApplePlatform::Tvos, DestinationKind::Device) => {
                format!("arm64-apple-tvos{deployment_target}")
            }
            (ApplePlatform::Visionos, DestinationKind::Simulator) => {
                format!("{architecture}-apple-xros{deployment_target}-simulator")
            }
            (ApplePlatform::Visionos, DestinationKind::Device) => {
                format!("arm64-apple-xros{deployment_target}")
            }
            (ApplePlatform::Watchos, DestinationKind::Simulator) => {
                format!("{architecture}-apple-watchos{deployment_target}-simulator")
            }
            (ApplePlatform::Watchos, DestinationKind::Device) => {
                format!("arm64_32-apple-watchos{deployment_target}")
            }
        };

        Ok(Self {
            platform,
            destination,
            sdk_name,
            sdk_path,
            deployment_target: deployment_target.to_owned(),
            architecture,
            target_triple,
        })
    }

    pub fn swiftc(&self) -> Command {
        let mut command = Command::new("xcrun");
        command.args(["--sdk", self.sdk_name.as_str(), "swiftc"]);
        command
    }

    pub fn toolchain_root(&self) -> Result<PathBuf> {
        let swiftc_path = command_output(Command::new("xcrun").args(["--find", "swiftc"]))?;
        let swiftc_path = PathBuf::from(swiftc_path.trim());
        let usr_dir = swiftc_path
            .parent()
            .and_then(|parent| parent.parent())
            .context("failed to resolve Swift toolchain root from `xcrun --find swiftc`")?;
        if usr_dir.file_name().and_then(|name| name.to_str()) == Some("usr") {
            return Ok(usr_dir.parent().unwrap_or(usr_dir).to_path_buf());
        }
        Ok(usr_dir.to_path_buf())
    }

    pub fn clang(&self, cpp: bool) -> Command {
        let tool = if cpp { "clang++" } else { "clang" };
        let mut command = Command::new("xcrun");
        command.args(["--sdk", self.sdk_name.as_str(), tool]);
        command
    }

    pub fn libtool(&self) -> Command {
        let mut command = Command::new("xcrun");
        command.args(["--sdk", self.sdk_name.as_str(), "libtool"]);
        command
    }

    pub fn actool_command(&self) -> Command {
        Command::new("xcrun")
    }

    pub fn info_plist_supported_platform(&self) -> &'static str {
        match (self.platform, self.destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => "iPhoneSimulator",
            (ApplePlatform::Ios, DestinationKind::Device) => "iPhoneOS",
            (ApplePlatform::Macos, _) => "MacOSX",
            (ApplePlatform::Tvos, DestinationKind::Simulator) => "AppleTVSimulator",
            (ApplePlatform::Tvos, DestinationKind::Device) => "AppleTVOS",
            (ApplePlatform::Visionos, DestinationKind::Simulator) => "XRSimulator",
            (ApplePlatform::Visionos, DestinationKind::Device) => "XROS",
            (ApplePlatform::Watchos, DestinationKind::Simulator) => "WatchSimulator",
            (ApplePlatform::Watchos, DestinationKind::Device) => "WatchOS",
        }
    }

    pub fn actool_platform_name(&self) -> &'static str {
        match (self.platform, self.destination) {
            (ApplePlatform::Ios, DestinationKind::Simulator) => "iphonesimulator",
            (ApplePlatform::Ios, DestinationKind::Device) => "iphoneos",
            (ApplePlatform::Macos, _) => "macosx",
            (ApplePlatform::Tvos, DestinationKind::Simulator) => "appletvsimulator",
            (ApplePlatform::Tvos, DestinationKind::Device) => "appletvos",
            (ApplePlatform::Visionos, DestinationKind::Simulator) => "xrsimulator",
            (ApplePlatform::Visionos, DestinationKind::Device) => "xros",
            (ApplePlatform::Watchos, DestinationKind::Simulator) => "watchsimulator",
            (ApplePlatform::Watchos, DestinationKind::Device) => "watchos",
        }
    }

    pub fn actool_target_device(&self) -> &'static [&'static str] {
        match self.platform {
            ApplePlatform::Ios => &["iphone", "ipad"],
            ApplePlatform::Tvos => &["tv"],
            ApplePlatform::Watchos => &["watch"],
            ApplePlatform::Visionos => &["vision"],
            ApplePlatform::Macos => &["mac"],
        }
    }

    pub fn bundle_build_metadata(&self) -> Result<BundleBuildMetadata> {
        let sdk_version = self.sdk_value("--show-sdk-version")?;
        let sdk_build = self.sdk_value("--show-sdk-build-version")?;
        let (xcode, xcode_build) = xcode_metadata()?;

        Ok(BundleBuildMetadata {
            build_machine_os_build: macos_build_version()?,
            compiler: "com.apple.compilers.llvm.clang.1_0".to_owned(),
            platform_build: sdk_build.clone(),
            platform_name: self.sdk_name.clone(),
            platform_version: sdk_version.clone(),
            sdk_build,
            sdk_name: format!("{}{}", self.sdk_name, sdk_version),
            xcode,
            xcode_build,
        })
    }

    fn sdk_value(&self, flag: &str) -> Result<String> {
        let value =
            command_output(Command::new("xcrun").args(["--sdk", self.sdk_name.as_str(), flag]))?;
        Ok(value.trim().to_owned())
    }
}

fn host_architecture() -> Result<String> {
    let output = command_output(Command::new("uname").arg("-m"))?;
    let architecture = output.trim();
    match architecture {
        "arm64" | "x86_64" => Ok(architecture.to_owned()),
        other => bail!("unsupported host architecture `{other}`"),
    }
}

fn macos_build_version() -> Result<String> {
    let output = command_output(Command::new("sw_vers").arg("-buildVersion"))?;
    Ok(output.trim().to_owned())
}

fn xcode_metadata() -> Result<(String, String)> {
    let output = command_output(Command::new("xcodebuild").arg("-version"))?;
    let mut xcode_version = None;
    let mut xcode_build = None;
    for line in output.lines() {
        if let Some(version) = line.strip_prefix("Xcode ") {
            xcode_version = Some(normalize_xcode_version(version.trim())?);
        } else if let Some(build) = line.strip_prefix("Build version ") {
            xcode_build = Some(build.trim().to_owned());
        }
    }
    match (xcode_version, xcode_build) {
        (Some(version), Some(build)) => Ok((version, build)),
        _ => bail!("failed to parse `xcodebuild -version` output"),
    }
}

fn normalize_xcode_version(version: &str) -> Result<String> {
    let mut components = version.split('.').map(str::trim);
    let major = components
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing major Xcode version"))?
        .parse::<u64>()?;
    let minor = components.next().unwrap_or("0").parse::<u64>()?;
    let patch = components.next().unwrap_or("0").parse::<u64>()?;
    if components.next().is_some() {
        bail!("unsupported Xcode version format `{version}`");
    }
    Ok((major * 100 + minor * 10 + patch).to_string())
}
