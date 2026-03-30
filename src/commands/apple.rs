use anyhow::Result;

use crate::apple;
use crate::cli::{
    AppleCommand, AppleDebugCommand, AppleDeviceCommand, AppleSigningCommand, Cli, Command,
};
use crate::context::AppContext;

pub fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Run(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::build::run_on_destination(&project, args)
        }
        Command::Build(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::build::build_artifact(&project, args)
        }
        Command::Submit(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::submit::submit_artifact(&project, args)
        }
        Command::Clean(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::clean::clean_project(&project, args)
        }
        Command::Apple(apple_args) => match &apple_args.command {
            AppleCommand::Device { command } => match command {
                AppleDeviceCommand::List(args) => apple::device::list_devices(app, args),
                AppleDeviceCommand::Register(args) => apple::device::register_device(app, args),
                AppleDeviceCommand::Import(args) => apple::device::import_devices(app, args),
                AppleDeviceCommand::Remove(args) => apple::device::remove_device(app, args),
            },
            AppleCommand::Signing { command } => match command {
                AppleSigningCommand::Sync(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple::signing::sync_signing(&project, args)
                }
                AppleSigningCommand::Export(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple::signing::export_signing_credentials(&project, args)
                }
                AppleSigningCommand::Import(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple::signing::import_signing_credentials(&project, args)
                }
            },
            AppleCommand::Debug { command } => match command.as_ref() {
                AppleDebugCommand::GrandSlam(args) => {
                    apple::grand_slam::debug_grand_slam(app, args.as_ref())
                }
                AppleDebugCommand::DeveloperServices(args) => {
                    apple::debug::debug_developer_services(app, cli.manifest.as_deref(), args)
                }
                AppleDebugCommand::NotaryStatus(args) => {
                    apple::debug::debug_notary_status(app, cli.manifest.as_deref(), args)
                }
                AppleDebugCommand::AscSession(args) => apple::debug::debug_asc_session(app, args),
            },
        },
    }
}
