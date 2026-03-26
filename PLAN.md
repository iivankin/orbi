# Orbit Apple Family CLI Plan

## Summary

Build Orbit as a local-only Apple platform toolchain orchestrator: a Rust CLI that owns the build graph from `orbit.json`, compiles and packages apps without `xcodebuild` or Xcode projects, signs them for the selected distribution, and submits the resulting artifact to App Store Connect or notarization as appropriate.

This is a hard cutover from the old manifest/runtime prototype. Reuse good ideas from the last committed codebase, but implement a clean v2 around `platform: "apple"` and the new command surface:
- `orbit run --device|--simulator --debug`
- `orbit build --profile <name>`
- `orbit submit`
- `orbit apple device ...`

The only remote systems are Apple Developer / App Store Connect. Orbit stores local build receipts, caches, and non-secret state under Orbit dirs, and stores secrets/session material in macOS Keychain.

## Public Interfaces

### Manifest

Adopt a new manifest version with:
- top-level `platform: "apple"`
- keep the top-level `platforms` map from the current examples
- keep shared `targets` at top level
- keep config in `orbit.json`; do not split submit metadata into separate files

Required shape:
- `platforms.<os>.deployment_target`
- `platforms.<os>.profiles.<profile>`
- `targets[].platforms` for multi-OS targeting where needed
- explicit Apple target kinds:
  - `app`
  - `app-extension`
  - `framework`
  - `static-library`
  - `dynamic-library`
  - `executable`
  - `watch-app`
  - `watch-extension`
  - `widget-extension`

Profile model:
- iOS/tvOS/visionOS/watchOS profiles support `development`, `ad-hoc`, `app-store`
- macOS profiles support `development`, `developer-id`, `mac-app-store`
- each profile declares build configuration, destination defaults, signing/export method, and submit behavior
- `run` uses the platform’s `development` profile by default; destination flags override only the destination

Submit config:
- live in `platforms.<os>` alongside profiles
- include only technical submission config needed for upload/submission/notarization
- no release-note or store-listing authoring in v1

### CLI behavior

- `orbit run`
  - selects the app target, builds it with the development profile, signs it for the chosen destination, and launches it
  - prompts for simulator or physical device selection when multiple candidates exist
- `orbit build --profile`
  - produces the distributable artifact for the chosen target/profile and writes a build receipt
- `orbit submit`
  - consumes the artifact produced by the build step; never rebuilds
  - uploads/submits the recorded artifact from the matching build receipt
- `orbit apple device`
  - subcommands: `list`, `register`, `import`, `remove`
  - manages devices for ad-hoc provisioning and profile reconciliation

## Implementation Changes

### 1. Rebuild the core as a graph-driven Apple backend

Implement a new core pipeline with these stages:
- manifest loader/validator for manifest v2 only; reject the old schema
- Apple-family graph normalizer that expands targets, per-OS variants, embedding rules, and profile selection
- direct toolchain resolver using `xcode-select`, developer-dir inspection, `xcrun`, SDK layout, and tool discovery
- no use of `xcodebuild` for versioning, building, exporting, or archiving
- no Xcode project generation

Normalize the graph into explicit actions:
- source compilation
- package resolution
- resource compilation
- link
- bundle assembly
- signing/provisioning reconciliation
- export/package
- submit/notarize

Keep single-arch outputs only. Choose the one required by the selected profile/destination; do not create fat/universal artifacts by default.

### 2. Expand compilation from the current Swift-only prototype to full Apple-native inputs

Support:
- Swift compilation via `swiftc`
- C/ObjC/C++ compilation via `clang`/`clang++`
- mixed-language link orchestration
- Apple/system frameworks
- XCFramework dependency slice selection
- SwiftPM-backed external dependency resolution

Dependency strategy:
- Orbit remains manifest-driven
- SwiftPM is used as a backend resolver/fetcher for package dependencies and binary artifacts
- do not plan for CocoaPods/Carthage in this implementation

Resource pipeline:
- compile full Apple app resources through direct tool invocation
- include asset catalogs, storyboards/xibs, strings, privacy manifests, widget/intent metadata, Core Data models, and other common bundle resources
- generate and merge bundle metadata/Info.plist/entitlements per target
- encode Apple embedding rules in the graph:
  - app embeds app extensions
  - iOS app hosts watch app
  - watch app hosts watch extension
  - app bundles embed frameworks and XCFramework payloads as required by platform/layout

### 3. Add Apple-first signing, provisioning, and local state management

Provisioning model:
- managed by default
- Apple is the source of truth
- Orbit reconciles local target state against Apple on demand

Implement Apple automation for:
- bundle ID / App ID creation and lookup
- capability reconciliation from entitlements
- certificate creation/reuse/import
- provisioning profile creation/reuse/repair
- ad-hoc device registration/import/removal
- app record lookup/creation needed for submission flows

Auth strategy:
- Apple ID session is primary
- store session material in Keychain
- use ASC API key only where submission/notarization cannot be completed reliably with Apple ID
- do not depend on Fastlane or a custom backend service

Local persistence:
- secrets and Apple auth material in Keychain
- caches, receipts, resolved package state, exported artifacts, and non-secret metadata in Orbit dirs
- build artifacts under project-local `.orbit`
- no mirrored server-side credential database

### 4. Package, export, and submit from build receipts

Build output policy:
- `orbit build --profile` produces both the platform bundle and the exported distributable artifact
- write a build receipt that records target, platform, profile, signing method, artifact path, bundle IDs, and submission eligibility

Export rules:
- iOS/tvOS/visionOS/watchOS app-store/ad-hoc outputs export to `.ipa`
- macOS Developer ID exports `.app` plus signed installer/package form as required for notarization/distribution
- macOS App Store exports the App Store-compatible package form

Submit rules:
- for App Store-family profiles, upload the build artifact to App Store Connect/TestFlight from the recorded build receipt
- for Developer ID macOS, notarize then staple the exported artifact
- `orbit submit` operates on the artifact from the build step and never triggers an implicit rebuild

## Test Plan

### Unit and golden tests

- manifest v2 parsing/validation
- rejection of the old manifest schema
- profile selection and destination validation
- target graph ordering and embedding rules
- target-kind validation for app extensions, watch targets, widgets, frameworks, and libraries
- build receipt creation and submit resolution
- single-arch slice selection for XCFrameworks and platform SDKs

### Integration fixtures

- migrate the current example iOS app to manifest v2 and keep it as the baseline simulator fixture
- migrate the current app-extension example and use it as the baseline embedding/signing fixture
- add fixtures for:
  - macOS app
  - watch companion app + watch extension
  - tvOS app
  - visionOS app
  - mixed Swift/ObjC target
  - SwiftPM dependency
  - XCFramework dependency
  - compiled resource bundle

### Apple-gated live tests

- iOS simulator run
- macOS host run
- physical-device preflight, sign, install, and launch
- ad-hoc device management and provisioning profile reconciliation
- app-store export and upload
- Developer ID notarization and stapling
- submit consumes the build artifact from the prior build receipt and does not rebuild

## Assumptions and Defaults

- Hard cutover only: no backward compatibility parser for the old manifest shape.
- Keep the current top-level `platforms` map, but the manifest family marker becomes `platform: "apple"`.
- Config stays in `orbit.json`, including submit config; no separate metadata files.
- No release-note/store-listing authoring in this implementation.
- Apple Developer / App Store Connect are authoritative; Orbit keeps only cache/receipts/keychain-backed local state.
- Apple ID is the preferred auth path; ASC API keys are fallback-only for submission/notarization where required.
- No `xcodebuild`, no Xcode project generation, no Fastlane.
- No fat/universal outputs by default.
- No CocoaPods/Carthage in this implementation.
- `orbit submit` uploads the artifact produced by `orbit build`; it does not infer a rebuild workflow.
- Treat the deleted Rust prototype as reference material, not a compatibility target.

## Structure

src/
  apple/
   ...apple-related stuff we do currently
  android/
   ...for the future
  utils/
  ..and all other shared stuff between all platforms
