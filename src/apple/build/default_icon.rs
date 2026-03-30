use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use plist::Value;
use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::manifest::{ApplePlatform, TargetKind};
use crate::util::{ensure_dir, run_command};

const APP_ICON_SET_NAME: &str = "AppIcon";
const DEFAULT_ICON_SOURCE_BASENAME: &str = "orbit-default-icon-source.png";
const DEFAULT_APP_ICON_PNG_BASE64: &str = "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AAAAEAAAAIAAAAAABAAAACAAAAAAAQAAAALAvwIAAAgAAABAAAAAAAIAAAAQAAAAgAAAAAAEAAAAIAAAAAABAAAACAAAAEAAAAAAAgAAAAQAAAAgAAAAAAEAAAAIAAAAQAAAAAACAAAAEAAAAIAAAAAABAAAACAAAAAAAQAAAAgAAABAAAAAAAIAAAAQAAAAIAAAAAABAAAACAAAAEAAAAAAAgAAABAAAACAAAAAAAQAAAAgAAAAAAEAAAAIAAAAEAAAAIAAAAAABAAAACAAAAAAAQAAAAgAAABAAAAAAAIAAAAQAAAAgAAAAAAEAAAAIAAAAEAAAAAAAgAAABAAAACAAAAAAAQAAAAgAAAAAAEAAAAIAAAAQAAAAAACAAAAEAAAACAAAAAAAQAAAAgAAABAAAAAAAIAAAAQAAAAgAAAAAAEAAAAIAAAAAABAAAACAAAABAAAACAAAAAAAQAAAAgAAAAAAEAAAAIAAAAQAAAAAACAAAAEAAAAIAAAAAABAAAAAgAAABAAAAAAAIAAAAQAAAAgAAAAAAEAAAAIAAAAAABAAAACAAAAEAAAAAAAgAAAASAfwUAABAAAACAAAAAAAQAAAAgAAAAAAEAAAAIAAAAQAAAAAACAAAAEAAAAIAAAAAABAAAAAgAAABAAAAAAAIAAAAQAAAAgAAAAAAEAAAAIAAAAAABAAAACAAAAEAAAAAAAgAAAAQAAAAgAAAAAAEAAAAIAAAAQAAAAACn/QtQ7Fd8VhPIegAAAABJRU5ErkJggg==";

pub struct PreparedAssetCatalogs {
    catalogs: Vec<PathBuf>,
    has_app_icon: bool,
    generated_default_app_icon: bool,
    _generated_root: Option<TempDir>,
}

impl PreparedAssetCatalogs {
    pub fn new(asset_catalogs: &[PathBuf]) -> Self {
        Self {
            catalogs: asset_catalogs.to_vec(),
            has_app_icon: false,
            generated_default_app_icon: false,
            _generated_root: None,
        }
    }

    pub fn catalogs(&self) -> &[PathBuf] {
        &self.catalogs
    }

    pub fn has_app_icon(&self) -> bool {
        self.has_app_icon
    }

    pub fn generated_default_app_icon(&self) -> bool {
        self.generated_default_app_icon
    }
}

#[derive(Clone, Copy)]
struct IconImageSpec {
    filename: &'static str,
    idiom: &'static str,
    size: &'static str,
    scale: &'static str,
    pixels: u32,
}

const IOS_ICON_SPECS: &[IconImageSpec] = &[
    IconImageSpec {
        filename: "iphone-20@2x.png",
        idiom: "iphone",
        size: "20x20",
        scale: "2x",
        pixels: 40,
    },
    IconImageSpec {
        filename: "iphone-20@3x.png",
        idiom: "iphone",
        size: "20x20",
        scale: "3x",
        pixels: 60,
    },
    IconImageSpec {
        filename: "iphone-29@2x.png",
        idiom: "iphone",
        size: "29x29",
        scale: "2x",
        pixels: 58,
    },
    IconImageSpec {
        filename: "iphone-29@3x.png",
        idiom: "iphone",
        size: "29x29",
        scale: "3x",
        pixels: 87,
    },
    IconImageSpec {
        filename: "iphone-40@2x.png",
        idiom: "iphone",
        size: "40x40",
        scale: "2x",
        pixels: 80,
    },
    IconImageSpec {
        filename: "iphone-40@3x.png",
        idiom: "iphone",
        size: "40x40",
        scale: "3x",
        pixels: 120,
    },
    IconImageSpec {
        filename: "iphone-60@2x.png",
        idiom: "iphone",
        size: "60x60",
        scale: "2x",
        pixels: 120,
    },
    IconImageSpec {
        filename: "iphone-60@3x.png",
        idiom: "iphone",
        size: "60x60",
        scale: "3x",
        pixels: 180,
    },
    IconImageSpec {
        filename: "ipad-20@1x.png",
        idiom: "ipad",
        size: "20x20",
        scale: "1x",
        pixels: 20,
    },
    IconImageSpec {
        filename: "ipad-20@2x.png",
        idiom: "ipad",
        size: "20x20",
        scale: "2x",
        pixels: 40,
    },
    IconImageSpec {
        filename: "ipad-29@1x.png",
        idiom: "ipad",
        size: "29x29",
        scale: "1x",
        pixels: 29,
    },
    IconImageSpec {
        filename: "ipad-29@2x.png",
        idiom: "ipad",
        size: "29x29",
        scale: "2x",
        pixels: 58,
    },
    IconImageSpec {
        filename: "ipad-40@1x.png",
        idiom: "ipad",
        size: "40x40",
        scale: "1x",
        pixels: 40,
    },
    IconImageSpec {
        filename: "ipad-40@2x.png",
        idiom: "ipad",
        size: "40x40",
        scale: "2x",
        pixels: 80,
    },
    IconImageSpec {
        filename: "ipad-76@1x.png",
        idiom: "ipad",
        size: "76x76",
        scale: "1x",
        pixels: 76,
    },
    IconImageSpec {
        filename: "ipad-76@2x.png",
        idiom: "ipad",
        size: "76x76",
        scale: "2x",
        pixels: 152,
    },
    IconImageSpec {
        filename: "ipad-83.5@2x.png",
        idiom: "ipad",
        size: "83.5x83.5",
        scale: "2x",
        pixels: 167,
    },
    IconImageSpec {
        filename: "ios-marketing-1024.png",
        idiom: "ios-marketing",
        size: "1024x1024",
        scale: "1x",
        pixels: 1024,
    },
];

const MACOS_ICON_SPECS: &[IconImageSpec] = &[
    IconImageSpec {
        filename: "icon_16x16.png",
        idiom: "mac",
        size: "16x16",
        scale: "1x",
        pixels: 16,
    },
    IconImageSpec {
        filename: "icon_16x16@2x.png",
        idiom: "mac",
        size: "16x16",
        scale: "2x",
        pixels: 32,
    },
    IconImageSpec {
        filename: "icon_32x32.png",
        idiom: "mac",
        size: "32x32",
        scale: "1x",
        pixels: 32,
    },
    IconImageSpec {
        filename: "icon_32x32@2x.png",
        idiom: "mac",
        size: "32x32",
        scale: "2x",
        pixels: 64,
    },
    IconImageSpec {
        filename: "icon_128x128.png",
        idiom: "mac",
        size: "128x128",
        scale: "1x",
        pixels: 128,
    },
    IconImageSpec {
        filename: "icon_128x128@2x.png",
        idiom: "mac",
        size: "128x128",
        scale: "2x",
        pixels: 256,
    },
    IconImageSpec {
        filename: "icon_256x256.png",
        idiom: "mac",
        size: "256x256",
        scale: "1x",
        pixels: 256,
    },
    IconImageSpec {
        filename: "icon_256x256@2x.png",
        idiom: "mac",
        size: "256x256",
        scale: "2x",
        pixels: 512,
    },
    IconImageSpec {
        filename: "icon_512x512.png",
        idiom: "mac",
        size: "512x512",
        scale: "1x",
        pixels: 512,
    },
    IconImageSpec {
        filename: "icon_512x512@2x.png",
        idiom: "mac",
        size: "512x512",
        scale: "2x",
        pixels: 1024,
    },
];

pub fn should_generate_default_app_icon(platform: ApplePlatform, target_kind: TargetKind) -> bool {
    matches!(
        (platform, target_kind),
        (ApplePlatform::Ios, TargetKind::App) | (ApplePlatform::Macos, TargetKind::App)
    )
}

pub fn prepare_asset_catalogs(
    platform: ApplePlatform,
    target_kind: TargetKind,
    asset_catalogs: &[PathBuf],
) -> Result<PreparedAssetCatalogs> {
    let mut prepared = PreparedAssetCatalogs::new(asset_catalogs);
    prepared.has_app_icon = asset_catalog_contains_named_set(asset_catalogs, "AppIcon.appiconset");
    if prepared.has_app_icon || !should_generate_default_app_icon(platform, target_kind) {
        return Ok(prepared);
    }

    let generated_root =
        tempdir().context("failed to create temp directory for default app icon")?;
    let generated_catalog = generate_default_asset_catalog(platform, generated_root.path())?;
    prepared.catalogs.push(generated_catalog);
    prepared.has_app_icon = true;
    prepared.generated_default_app_icon = true;
    prepared._generated_root = Some(generated_root);
    Ok(prepared)
}

pub fn ensure_icon_metadata(
    platform: ApplePlatform,
    target_kind: TargetKind,
    info_plist_root: &Path,
    has_app_icon: bool,
) -> Result<()> {
    if !has_app_icon || !requires_top_level_icon_name(platform, target_kind) {
        return Ok(());
    }

    let info_plist_path = info_plist_root.join("Info.plist");
    if !info_plist_path.exists() {
        return Ok(());
    }

    let mut info_plist = Value::from_file(&info_plist_path)
        .with_context(|| format!("failed to read {}", info_plist_path.display()))?;
    let info_dict = info_plist
        .as_dictionary_mut()
        .context("Info.plist must be a dictionary")?;
    if !info_dict.contains_key("CFBundleIconName") {
        info_dict.insert(
            "CFBundleIconName".to_owned(),
            Value::String(APP_ICON_SET_NAME.to_owned()),
        );
    }
    info_plist
        .to_file_xml(&info_plist_path)
        .with_context(|| format!("failed to write {}", info_plist_path.display()))
}

fn requires_top_level_icon_name(platform: ApplePlatform, target_kind: TargetKind) -> bool {
    matches!(
        (platform, target_kind),
        (ApplePlatform::Ios, TargetKind::App)
    )
}

fn generate_default_asset_catalog(platform: ApplePlatform, root: &Path) -> Result<PathBuf> {
    let catalog_root = root.join("OrbitGeneratedAssets.xcassets");
    ensure_dir(&catalog_root)?;
    fs::write(
        catalog_root.join("Contents.json"),
        serde_json::to_vec_pretty(&json!({
            "info": {
                "author": "orbit",
                "version": 1
            }
        }))?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            catalog_root.join("Contents.json").display()
        )
    })?;

    let app_icon_root = catalog_root.join("AppIcon.appiconset");
    ensure_dir(&app_icon_root)?;
    let source_icon_path = app_icon_root.join(DEFAULT_ICON_SOURCE_BASENAME);
    fs::write(&source_icon_path, default_icon_source_png()?)
        .with_context(|| format!("failed to write {}", source_icon_path.display()))?;

    let specs = icon_specs_for_platform(platform)?;
    let mut images = Vec::with_capacity(specs.len());
    for spec in specs {
        let icon_path = app_icon_root.join(spec.filename);
        write_icon_variant(&source_icon_path, &icon_path, spec.pixels)?;
        images.push(json!({
            "filename": spec.filename,
            "idiom": spec.idiom,
            "scale": spec.scale,
            "size": spec.size
        }));
    }

    fs::write(
        app_icon_root.join("Contents.json"),
        serde_json::to_vec_pretty(&json!({
            "images": images,
            "info": {
                "author": "orbit",
                "version": 1
            }
        }))?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            app_icon_root.join("Contents.json").display()
        )
    })?;

    Ok(catalog_root)
}

fn icon_specs_for_platform(platform: ApplePlatform) -> Result<&'static [IconImageSpec]> {
    match platform {
        ApplePlatform::Ios => Ok(IOS_ICON_SPECS),
        ApplePlatform::Macos => Ok(MACOS_ICON_SPECS),
        other => bail!("default app icon generation is not implemented for `{other}`"),
    }
}

fn default_icon_source_png() -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(DEFAULT_APP_ICON_PNG_BASE64)
        .context("failed to decode embedded default app icon")
}

fn write_icon_variant(source_icon_path: &Path, destination: &Path, pixels: u32) -> Result<()> {
    if pixels == 1024 {
        fs::copy(source_icon_path, destination)
            .with_context(|| format!("failed to copy {}", destination.display()))?;
        return Ok(());
    }

    let mut command = Command::new("sips");
    command
        .arg("-z")
        .arg(pixels.to_string())
        .arg(pixels.to_string());
    command.arg(source_icon_path);
    command.arg("--out").arg(destination);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    run_command(&mut command)
}

fn asset_catalog_contains_named_set(asset_catalogs: &[PathBuf], expected_name: &str) -> bool {
    asset_catalogs
        .iter()
        .any(|catalog| catalog.join(expected_name).exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Dictionary;

    #[cfg(target_os = "macos")]
    #[test]
    fn generates_ios_default_app_icon_catalog_when_missing() {
        let prepared = prepare_asset_catalogs(ApplePlatform::Ios, TargetKind::App, &[]).unwrap();

        assert!(prepared.generated_default_app_icon());
        assert!(prepared.has_app_icon());
        assert_eq!(prepared.catalogs().len(), 1);
        assert!(
            prepared.catalogs()[0]
                .join("AppIcon.appiconset/Contents.json")
                .exists()
        );
        assert!(
            prepared.catalogs()[0]
                .join("AppIcon.appiconset/iphone-60@3x.png")
                .exists()
        );
    }

    #[test]
    fn preserves_existing_app_icon_catalogs() {
        let temp = tempdir().unwrap();
        let catalog = temp.path().join("Assets.xcassets");
        let iconset = catalog.join("AppIcon.appiconset");
        ensure_dir(&iconset).unwrap();

        let prepared = prepare_asset_catalogs(
            ApplePlatform::Ios,
            TargetKind::App,
            std::slice::from_ref(&catalog),
        )
        .unwrap();

        assert!(!prepared.generated_default_app_icon());
        assert!(prepared.has_app_icon());
        assert_eq!(prepared.catalogs(), &[catalog]);
    }

    #[test]
    fn inserts_top_level_icon_name_for_ios_apps() {
        let temp = tempdir().unwrap();
        let info_plist_path = temp.path().join("Info.plist");
        Value::Dictionary(Dictionary::from_iter([(
            "CFBundleIcons".to_owned(),
            Value::Dictionary(Dictionary::from_iter([(
                "CFBundlePrimaryIcon".to_owned(),
                Value::Dictionary(Dictionary::new()),
            )])),
        )]))
        .to_file_xml(&info_plist_path)
        .unwrap();

        ensure_icon_metadata(ApplePlatform::Ios, TargetKind::App, temp.path(), true).unwrap();

        let updated = Value::from_file(&info_plist_path).unwrap();
        assert_eq!(
            updated
                .as_dictionary()
                .and_then(|dict| dict.get("CFBundleIconName"))
                .and_then(Value::as_string),
            Some(APP_ICON_SET_NAME)
        );
    }
}
