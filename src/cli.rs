use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{Args, Parser, Subcommand, ValueEnum};

pub const CLAP_STYLING: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Blue.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Magenta.on_default().bold())
    .error(AnsiColor::Red.on_default().bold())
    .context(AnsiColor::Yellow.on_default().dimmed())
    .context_value(AnsiColor::Yellow.on_default().italic());

#[derive(Debug, Parser)]
#[command(name = "orbit")]
#[command(about = "Local-first Apple family build and signing CLI")]
#[command(arg_required_else_help = true)]
#[command(styles = CLAP_STYLING)]
#[command(
    after_help = "Examples:\n  orbit run --platform ios --simulator\n  orbit build --platform ios --distribution development\n  orbit build --platform ios --distribution app-store --release\n  orbit submit --platform ios --wait\n  orbit clean --all\n  orbit apple device list --refresh\n  orbit apple signing export --platform ios --distribution development\n  orbit apple signing import --platform ios --distribution development --p12 ./signing.p12 --password secret"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub manifest: Option<PathBuf>,

    #[arg(long, global = true)]
    pub non_interactive: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run(RunArgs),
    Build(BuildArgs),
    Submit(SubmitArgs),
    Clean(CleanArgs),
    Apple(Box<AppleArgs>),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, conflicts_with = "device")]
    pub simulator: bool,

    #[arg(long, conflicts_with = "simulator")]
    pub device: bool,

    #[arg(long)]
    pub device_id: Option<String>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub release: bool,

    #[arg(long, conflicts_with = "device")]
    pub simulator: bool,

    #[arg(long, conflicts_with = "simulator")]
    pub device: bool,

    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub receipt: Option<PathBuf>,

    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    #[arg(long, conflicts_with_all = ["apple", "all"])]
    pub local: bool,

    #[arg(long, conflicts_with_all = ["local", "all"])]
    pub apple: bool,

    #[arg(long, conflicts_with_all = ["local", "apple"])]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct AppleArgs {
    #[command(subcommand)]
    pub command: AppleCommand,
}

#[derive(Debug, Subcommand)]
pub enum AppleCommand {
    Device {
        #[command(subcommand)]
        command: AppleDeviceCommand,
    },
    Signing {
        #[command(subcommand)]
        command: AppleSigningCommand,
    },
    Debug {
        #[command(subcommand)]
        command: Box<AppleDebugCommand>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppleDeviceCommand {
    List(ListDevicesArgs),
    Register(RegisterDeviceArgs),
    Import(ImportDevicesArgs),
    Remove(RemoveDeviceArgs),
}

#[derive(Debug, Args)]
pub struct ListDevicesArgs {
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct RegisterDeviceArgs {
    #[arg(long)]
    pub name: Option<String>,

    #[arg(long)]
    pub udid: Option<String>,

    #[arg(long, value_enum, default_value_t = DevicePlatform::Ios)]
    pub platform: DevicePlatform,

    #[arg(long, conflicts_with_all = ["name", "udid"])]
    pub current_machine: bool,
}

#[derive(Debug, Args)]
pub struct ImportDevicesArgs {
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RemoveDeviceArgs {
    #[arg(long)]
    pub id: Option<String>,

    #[arg(long)]
    pub udid: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum DevicePlatform {
    #[value(name = "ios")]
    Ios,
    #[value(name = "macos")]
    MacOs,
    #[value(name = "universal")]
    Universal,
}

#[derive(Debug, Subcommand)]
pub enum AppleSigningCommand {
    Sync(SigningSyncArgs),
    Export(SigningExportArgs),
    Import(SigningImportArgs),
}

#[derive(Debug, Subcommand)]
pub enum AppleDebugCommand {
    GrandSlam(Box<AppleGrandSlamDebugArgs>),
    DeveloperServices(AppleDeveloperServicesDebugArgs),
    NotaryStatus(AppleNotaryStatusDebugArgs),
    AscSession(AppleAscSessionDebugArgs),
}

#[derive(Debug, Args)]
pub struct AppleGrandSlamDebugArgs {
    #[arg(long)]
    pub skip_lookup: bool,

    #[arg(long)]
    pub skip_fetch_global_configs: bool,

    #[arg(long)]
    pub skip_srp: bool,

    #[arg(long, default_value = "iTunes")]
    pub service: String,

    #[arg(long)]
    pub client_identifier: Option<String>,

    #[arg(long)]
    pub client_info: Option<String>,

    #[arg(long)]
    pub user_agent: Option<String>,

    #[arg(long)]
    pub accept_language: Option<String>,

    #[arg(long)]
    pub locale: Option<String>,

    #[arg(long)]
    pub device_id: Option<String>,

    #[arg(long)]
    pub serial_number: Option<String>,

    #[arg(long)]
    pub md: Option<String>,

    #[arg(long = "md-m")]
    pub md_m: Option<String>,

    #[arg(long = "md-rinfo")]
    pub md_rinfo: Option<String>,

    #[arg(long)]
    pub probe_content_delivery: bool,

    #[arg(long)]
    pub dump_plist: bool,
}

#[derive(Debug, Args)]
pub struct AppleNotaryStatusDebugArgs {
    #[arg(long)]
    pub submission_id: String,
}

#[derive(Debug, Args)]
pub struct AppleDeveloperServicesDebugArgs {
    #[arg(long)]
    pub bundle_id: Option<String>,

    #[arg(long)]
    pub certificate_type: Option<String>,
}

#[derive(Debug, Args)]
pub struct AppleAscSessionDebugArgs {
    #[arg(
        long,
        default_value = "https://api.appstoreconnect.apple.com/v1/apps?limit=1"
    )]
    pub url: String,
}

#[derive(Debug, Args)]
pub struct SigningSyncArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long, conflicts_with = "device")]
    pub simulator: bool,

    #[arg(long, conflicts_with = "simulator")]
    pub device: bool,
}

#[derive(Debug, Args)]
pub struct SigningExportArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SigningImportArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub p12: PathBuf,

    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum TargetPlatform {
    #[value(name = "ios")]
    Ios,
    #[value(name = "macos")]
    Macos,
    #[value(name = "tvos")]
    Tvos,
    #[value(name = "visionos")]
    Visionos,
    #[value(name = "watchos")]
    Watchos,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum DistributionArg {
    #[value(name = "development")]
    Development,
    #[value(name = "ad-hoc")]
    AdHoc,
    #[value(name = "app-store")]
    AppStore,
    #[value(name = "developer-id")]
    DeveloperId,
    #[value(name = "mac-app-store")]
    MacAppStore,
}
