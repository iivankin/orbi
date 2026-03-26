use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apple::asc_api::{AscClient, DeviceAttributes, Resource};
use crate::apple::auth::resolve_api_key_auth;
use crate::cli::{
    DevicePlatform, ImportDevicesArgs, ListDevicesArgs, RegisterDeviceArgs, RemoveDeviceArgs,
};
use crate::context::{AppContext, DeviceCache};
use crate::util::{prompt_input, prompt_select};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDevice {
    pub id: String,
    pub name: String,
    pub udid: String,
    pub platform: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ImportedDevice {
    name: String,
    udid: String,
    platform: String,
}

pub fn list_devices(app: &AppContext, args: &ListDevicesArgs) -> Result<()> {
    let cache = load_cached_or_remote_devices(app, args.refresh)?;
    if cache.devices.is_empty() {
        println!("no devices registered");
        return Ok(());
    }

    for device in cache.devices {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            device.id, device.platform, device.udid, device.status, device.name
        );
    }
    Ok(())
}

pub fn register_device(app: &AppContext, args: &RegisterDeviceArgs) -> Result<()> {
    let client = asc_client(app)?;
    let imported = if args.current_machine {
        current_machine_device(args.platform)?
    } else {
        ImportedDevice {
            name: match &args.name {
                Some(name) => name.clone(),
                None if app.interactive => prompt_input("Device name", None)?,
                None => bail!("--name is required in non-interactive mode"),
            },
            udid: match &args.udid {
                Some(udid) => udid.clone(),
                None if app.interactive => prompt_input("Device UDID", None)?,
                None => bail!("--udid is required in non-interactive mode"),
            },
            platform: platform_value(args.platform).to_owned(),
        }
    };

    if let Some(existing) = client.find_device_by_udid(&imported.udid)? {
        println!(
            "reused\t{}\t{}\t{}\t{}",
            existing.id,
            existing.attributes.platform,
            existing.attributes.udid,
            existing.attributes.name
        );
    } else {
        let created = client.create_device(&imported.name, &imported.udid, &imported.platform)?;
        println!(
            "created\t{}\t{}\t{}\t{}",
            created.id,
            created.attributes.platform,
            created.attributes.udid,
            created.attributes.name
        );
    }

    let _ = refresh_cache(app)?;
    Ok(())
}

pub fn import_devices(app: &AppContext, args: &ImportDevicesArgs) -> Result<()> {
    let client = asc_client(app)?;
    let file = match &args.file {
        Some(file) => file.clone(),
        None if app.interactive => {
            PathBuf::from(prompt_input("Path to JSON or CSV device list", None)?)
        }
        None => bail!("--file is required in non-interactive mode"),
    };

    let mut created_count = 0usize;
    let devices = load_import_file(&file)?;
    for device in devices {
        if client.find_device_by_udid(&device.udid)?.is_none() {
            let created = client.create_device(&device.name, &device.udid, &device.platform)?;
            println!(
                "created\t{}\t{}\t{}\t{}",
                created.id,
                created.attributes.platform,
                created.attributes.udid,
                created.attributes.name
            );
            created_count += 1;
        }
    }

    let _ = refresh_cache(app)?;
    if created_count == 0 {
        println!("no new devices were imported");
    }
    Ok(())
}

pub fn remove_device(app: &AppContext, args: &RemoveDeviceArgs) -> Result<()> {
    let client = asc_client(app)?;
    let target_id = if let Some(id) = &args.id {
        id.clone()
    } else if let Some(udid) = &args.udid {
        let Some(device) = client.find_device_by_udid(udid)? else {
            bail!("no Apple device found for UDID `{udid}`");
        };
        device.id
    } else if app.interactive {
        let cache = refresh_cache(app)?;
        if cache.devices.is_empty() {
            bail!("no registered Apple devices found");
        }
        let labels = cache
            .devices
            .iter()
            .map(|device| format!("{} [{}] {}", device.name, device.platform, device.udid))
            .collect::<Vec<_>>();
        let index = prompt_select("Select a device to remove", &labels)?;
        cache.devices[index].id.clone()
    } else {
        bail!("pass --id or --udid");
    };

    client.delete_device(&target_id)?;
    println!("removed\t{target_id}");
    let _ = refresh_cache(app)?;
    Ok(())
}

pub fn refresh_cache(app: &AppContext) -> Result<DeviceCache> {
    let client = asc_client(app)?;
    let devices = client
        .list_devices()?
        .into_iter()
        .map(cached_device_from_resource)
        .collect::<Vec<_>>();
    let mut devices = devices;
    devices.sort_by(|left, right| {
        left.platform
            .cmp(&right.platform)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.udid.cmp(&right.udid))
    });
    let cache = DeviceCache { devices };
    app.write_device_cache(&cache)?;
    Ok(cache)
}

fn load_import_file(path: &Path) -> Result<Vec<ImportedDevice>> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if path.extension().and_then(|value| value.to_str()) == Some("json") {
        if let Ok(items) = serde_json::from_str::<Vec<ImportedDevice>>(&contents) {
            return Ok(items);
        }
    }

    let mut items = Vec::new();
    let mut seen_udids = std::collections::HashSet::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts = trimmed.split(',').map(str::trim).collect::<Vec<_>>();
        if parts.len() != 3 {
            bail!(
                "invalid device import line {} in {}; expected `udid,name,platform`",
                index + 1,
                path.display()
            );
        }
        if index == 0
            && parts[0].eq_ignore_ascii_case("udid")
            && parts[1].eq_ignore_ascii_case("name")
            && parts[2].eq_ignore_ascii_case("platform")
        {
            continue;
        }
        let device = ImportedDevice {
            udid: parts[0].to_owned(),
            name: parts[1].to_owned(),
            platform: parts[2].to_owned(),
        };
        if seen_udids.insert(device.udid.clone()) {
            items.push(device);
        }
    }
    Ok(items)
}

fn current_machine_device(platform: DevicePlatform) -> Result<ImportedDevice> {
    if matches!(platform, DevicePlatform::Ios) {
        bail!("`--current-machine` requires `--platform macos` or `--platform universal`");
    }

    let output = crate::util::command_output(
        std::process::Command::new("system_profiler").args(["-json", "SPHardwareDataType"]),
    )?;
    let value: Value = serde_json::from_str(&output)?;
    let entry = value
        .get("SPHardwareDataType")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .context("system_profiler did not return hardware information")?;
    let udid = entry
        .get("provisioning_UDID")
        .and_then(Value::as_str)
        .context("current machine does not expose provisioning_UDID")?;
    let name = entry
        .get("_name")
        .and_then(Value::as_str)
        .unwrap_or("Current Mac");
    Ok(ImportedDevice {
        name: name.to_owned(),
        udid: udid.to_owned(),
        platform: platform_value(platform).to_owned(),
    })
}

fn asc_client(app: &AppContext) -> Result<AscClient> {
    let auth = resolve_api_key_auth(app)?
        .context("device management requires App Store Connect API key auth; set ORBIT_ASC_API_KEY_PATH, ORBIT_ASC_KEY_ID, and ORBIT_ASC_ISSUER_ID")?;
    AscClient::new(auth)
}

fn load_cached_or_remote_devices(app: &AppContext, refresh: bool) -> Result<DeviceCache> {
    if refresh {
        return refresh_cache(app);
    }

    let cache = app.read_device_cache()?;
    if cache.devices.is_empty() {
        refresh_cache(app)
    } else {
        Ok(cache)
    }
}

fn cached_device_from_resource(resource: Resource<DeviceAttributes>) -> CachedDevice {
    CachedDevice {
        id: resource.id,
        name: resource.attributes.name,
        udid: resource.attributes.udid,
        platform: resource.attributes.platform,
        status: resource
            .attributes
            .status
            .unwrap_or_else(|| "UNKNOWN".to_owned()),
    }
}

fn platform_value(platform: DevicePlatform) -> &'static str {
    match platform {
        DevicePlatform::Ios => "IOS",
        DevicePlatform::MacOs => "MAC_OS",
        DevicePlatform::Universal => "UNIVERSAL",
    }
}
