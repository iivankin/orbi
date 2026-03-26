use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "orbit")]
#[command(about = "Local-first Apple family build and signing CLI")]
#[command(arg_required_else_help = true)]
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
    Apple(AppleArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long)]
    pub target: Option<String>,

    #[arg(long)]
    pub profile: Option<String>,

    #[arg(long)]
    pub simulator: bool,

    #[arg(long)]
    pub device: bool,

    #[arg(long)]
    pub device_id: Option<String>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    #[arg(long)]
    pub target: Option<String>,

    #[arg(long)]
    pub profile: String,

    #[arg(long)]
    pub simulator: bool,

    #[arg(long)]
    pub device: bool,

    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    #[arg(long)]
    pub target: Option<String>,

    #[arg(long)]
    pub profile: Option<String>,

    #[arg(long)]
    pub receipt: Option<PathBuf>,

    #[arg(long)]
    pub wait: bool,
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

    #[arg(long)]
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
}

#[derive(Debug, Args)]
pub struct SigningSyncArgs {
    #[arg(long)]
    pub target: Option<String>,

    #[arg(long)]
    pub profile: String,

    #[arg(long)]
    pub simulator: bool,

    #[arg(long)]
    pub device: bool,
}
