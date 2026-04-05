use std::fs;

use anyhow::{Result, bail};

use crate::apple::signing::{clean_local_signing_state, clean_remote_signing_state};
use crate::cli::CleanArgs;
use crate::context::ProjectContext;
use crate::util::prompt_confirm;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct CleanupPlan {
    clean_local_state: bool,
    clean_apple: bool,
}

impl CleanupPlan {
    fn without_apple_cleanup(mut self) -> Self {
        self.clean_apple = false;
        self
    }
}

pub fn clean_project(project: &ProjectContext, args: &CleanArgs) -> Result<()> {
    let mut plan = cleanup_plan(args)?;

    if plan.clean_apple
        && project.app.interactive
        && !prompt_confirm(
            "Delete Orbit-managed Apple Developer resources for this project?",
            false,
        )?
    {
        println!("skipped Apple Developer cleanup");
        plan = plan.without_apple_cleanup();
    }

    // Remote cleanup needs the pre-clean signing state to know which
    // Orbit-managed profiles and identifiers belong to this project.
    let remote_summary = if plan.clean_apple {
        Some(clean_remote_signing_state(project))
    } else {
        None
    };

    if plan.clean_local_state && project.project_paths.orbit_dir.exists() {
        fs::remove_dir_all(&project.project_paths.orbit_dir)?;
        println!(
            "removed_local_orbit_dir: {}",
            project.project_paths.orbit_dir.display()
        );
    }

    if plan.clean_local_state {
        let summary = clean_local_signing_state(project)?;
        println!("removed_local_profiles: {}", summary.removed_profiles);
        println!(
            "removed_local_certificates: {}",
            summary.removed_certificates
        );
    }

    if plan.clean_apple {
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

fn cleanup_plan(args: &CleanArgs) -> Result<CleanupPlan> {
    let plan = CleanupPlan {
        clean_local_state: args.local || args.all || !args.apple,
        clean_apple: args.apple || args.all,
    };
    if !plan.clean_local_state && !plan.clean_apple {
        bail!("select at least one cleanup mode");
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::{CleanupPlan, cleanup_plan};
    use crate::cli::CleanArgs;

    fn args(local: bool, apple: bool, all: bool) -> CleanArgs {
        CleanArgs { local, apple, all }
    }

    #[test]
    fn apple_cleanup_does_not_imply_local_cleanup() {
        let plan = cleanup_plan(&args(false, true, false)).unwrap();
        assert_eq!(
            plan,
            CleanupPlan {
                clean_local_state: false,
                clean_apple: true,
            }
        );
    }

    #[test]
    fn declining_apple_cleanup_still_keeps_local_cleanup_for_all() {
        let plan = cleanup_plan(&args(false, false, true))
            .unwrap()
            .without_apple_cleanup();
        assert_eq!(
            plan,
            CleanupPlan {
                clean_local_state: true,
                clean_apple: false,
            }
        );
    }
}
