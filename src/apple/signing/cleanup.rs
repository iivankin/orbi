use std::collections::HashSet;

use anyhow::Result;

use super::{
    delete_certificate_files, delete_file_if_exists, delete_p12_password, load_state,
    resolve_local_team_id_if_known, save_state,
};
use crate::context::ProjectContext;

#[derive(Debug, Clone, Default)]
pub struct LocalSigningCleanSummary {
    pub removed_profiles: usize,
    pub removed_certificates: usize,
}

pub fn clean_local_signing_state(project: &ProjectContext) -> Result<LocalSigningCleanSummary> {
    let Some(team_id) = resolve_local_team_id_if_known(project)? else {
        return Ok(LocalSigningCleanSummary::default());
    };
    let mut state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let mut removed_profile_cert_ids = HashSet::new();
    let mut removed_profiles = 0usize;

    state.profiles.retain(|profile| {
        if !bundle_ids.contains(&profile.bundle_id) {
            return true;
        }
        let _ = delete_file_if_exists(&profile.path);
        removed_profile_cert_ids.extend(profile.certificate_ids.iter().cloned());
        removed_profiles += 1;
        false
    });

    let remaining_certificate_ids = state
        .profiles
        .iter()
        .flat_map(|profile| profile.certificate_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let mut removed_certificates = 0usize;
    state.certificates.retain(|certificate| {
        if !removed_profile_cert_ids.contains(&certificate.id)
            || remaining_certificate_ids.contains(&certificate.id)
        {
            return true;
        }
        let _ = delete_certificate_files(certificate);
        let _ = delete_p12_password(&certificate.p12_password_account);
        removed_certificates += 1;
        false
    });

    save_state(project, &team_id, &state)?;
    Ok(LocalSigningCleanSummary {
        removed_profiles,
        removed_certificates,
    })
}

fn project_bundle_ids(project: &ProjectContext) -> HashSet<String> {
    project
        .resolved_manifest
        .targets
        .iter()
        .map(|target| target.bundle_id.clone())
        .collect()
}
