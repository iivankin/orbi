pub mod default_icon;
pub mod external;
pub mod pipeline;
pub mod receipt;
pub mod toolchain;
pub mod verify;

use anyhow::Result;

use crate::cli::{BuildArgs, RunArgs};
use crate::context::ProjectContext;

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    pipeline::run_on_destination(project, args)
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    pipeline::build_artifact(project, args)
}
