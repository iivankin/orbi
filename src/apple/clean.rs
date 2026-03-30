use std::fs;

use anyhow::{Result, bail};

use crate::apple::signing::{clean_local_signing_state, clean_remote_signing_state};
use crate::cli::CleanArgs;
use crate::context::ProjectContext;
use crate::util::prompt_confirm;

pub fn clean_project(project: &ProjectContext, args: &CleanArgs) -> Result<()> {
    let clean_local_build_state = args.local || args.all || !args.apple;
    let clean_apple = args.apple || args.all;

    if !clean_local_build_state && !clean_apple {
        bail!("select at least one cleanup mode");
    }

    if clean_apple
        && project.app.interactive
        && !prompt_confirm(
            "Delete Orbit-managed Apple Developer resources for this project?",
            false,
        )?
    {
        println!("skipped Apple Developer cleanup");
        return Ok(());
    }

    // Remote cleanup needs the pre-clean signing state to know which
    // Orbit-managed profiles and identifiers belong to this project.
    let remote_summary = if clean_apple {
        Some(clean_remote_signing_state(project))
    } else {
        None
    };

    if clean_local_build_state && project.project_paths.orbit_dir.exists() {
        fs::remove_dir_all(&project.project_paths.orbit_dir)?;
        println!(
            "removed_local_orbit_dir: {}",
            project.project_paths.orbit_dir.display()
        );
    }

    if clean_local_build_state || clean_apple {
        let summary = clean_local_signing_state(project)?;
        println!("removed_local_profiles: {}", summary.removed_profiles);
        println!(
            "removed_local_certificates: {}",
            summary.removed_certificates
        );
    }

    if clean_apple {
        let summary = remote_summary.expect("remote summary must be initialized")?;
        println!("removed_remote_profiles: {}", summary.removed_profiles);
        println!("removed_remote_apps: {}", summary.removed_apps);
        println!("removed_remote_app_groups: {}", summary.removed_app_groups);
        println!("removed_remote_merchants: {}", summary.removed_merchants);
        println!(
            "removed_remote_cloud_containers: {}",
            summary.removed_cloud_containers
        );
    }

    Ok(())
}
