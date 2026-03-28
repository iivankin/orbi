pub mod apple;
pub mod build;
pub mod clean;
pub mod cli;
pub mod context;
pub mod manifest;
pub mod util;

use anyhow::Result;
use clap::Parser;

use crate::apple::device as apple_device;
use crate::apple::signing as apple_signing;
use crate::cli::{AppleCommand, AppleDeviceCommand, AppleSigningCommand, Cli, Command};
use crate::context::AppContext;

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let app = AppContext::new(cli.non_interactive)?;

    match &cli.command {
        Command::Run(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            build::run_on_destination(&project, args)
        }
        Command::Build(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            build::build_artifact(&project, args)
        }
        Command::Submit(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            build::submit_artifact(&project, args)
        }
        Command::Clean(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            clean::clean_project(&project, args)
        }
        Command::Apple(apple) => match &apple.command {
            AppleCommand::Device { command } => match command {
                AppleDeviceCommand::List(args) => apple_device::list_devices(&app, args),
                AppleDeviceCommand::Register(args) => apple_device::register_device(&app, args),
                AppleDeviceCommand::Import(args) => apple_device::import_devices(&app, args),
                AppleDeviceCommand::Remove(args) => apple_device::remove_device(&app, args),
            },
            AppleCommand::Signing { command } => match command {
                AppleSigningCommand::Sync(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple_signing::sync_signing(&project, args)
                }
                AppleSigningCommand::Export(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple_signing::export_signing_credentials(&project, args)
                }
                AppleSigningCommand::ExportPush(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple_signing::export_push_auth_key(&project, args)
                }
                AppleSigningCommand::Import(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple_signing::import_signing_credentials(&project, args)
                }
            },
        },
    }
}
