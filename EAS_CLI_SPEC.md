# EAS CLI iOS Credential Handling Spec

Version inspected: `eas-cli@18.4.0`  
Git tag inspected: `v18.4.0`  
Tag commit inspected: `4e202db843be2dca6450af4b45ee76b226a662ea`

## Purpose

This document describes how EAS CLI handles the following iOS concerns:

- Apple account authentication used to talk to Apple services
- bundle identifier creation and entitlement-to-capability syncing
- Apple device registration and selection for ad hoc builds
- provisioning profile creation, reuse, repair, and assignment
- how those Apple-side artifacts are mirrored into Expo/EAS GraphQL records

The goal is to describe actual `eas-cli` behavior from source, not the idealized product behavior from docs.

## Scope

This spec is based on the TypeScript sources under `packages/eas-cli/src`.

Primary entry points inspected:

- `packages/eas-cli/src/build/ios/build.ts`
- `packages/eas-cli/src/build/ios/credentials.ts`
- `packages/eas-cli/src/credentials/context.ts`
- `packages/eas-cli/src/credentials/ios/IosCredentialsProvider.ts`
- `packages/eas-cli/src/credentials/ios/actions/*`
- `packages/eas-cli/src/credentials/ios/appstore/*`
- `packages/eas-cli/src/project/ios/target.ts`
- `packages/eas-cli/src/project/ios/entitlements.ts`
- `packages/eas-cli/src/devices/*`

Out of scope unless they intersect these flows:

- Android credentials
- push key management except where it affects auth mode expectations
- App Store submission flows
- Expo website implementation behind device registration URLs

## Important Terminology

### Two different meanings of "Apple auth"

In this area of EAS CLI, "Apple auth" can mean two different things:

1. Apple account authentication used by EAS CLI to call Apple APIs
2. The app capability "Sign In with Apple", driven by the entitlement `com.apple.developer.applesignin`

Both are handled here, but they are separate mechanisms.

### Main local objects

- `CredentialsContext`
  - runtime context for iOS credential operations
  - owns `ctx.appStore` and `ctx.ios`
- `AppStoreApi`
  - wrapper around Apple-side operations
  - caches one auth context in memory
- `Target`
  - EAS CLI's per-Xcode-target model
  - includes `targetName`, `bundleIdentifier`, `parentBundleIdentifier`, `entitlements`, and optional `buildSettings`

### Main Expo/EAS GraphQL objects

- `AppleTeam`
- `AppleAppIdentifier`
- `AppleDevice`
- `AppleProvisioningProfile`
- `IosAppCredentials`
- `IosAppBuildCredentials`

### Main Apple-side artifacts

- Apple Team
- Bundle ID / App ID
- Capability and capability identifiers
- Distribution certificate
- Provisioning profile
- Registered device

## High-Level Build Flow

For a normal iOS build with credentials enabled, the flow is:

1. `createIosContextAsync` resolves the Xcode scheme, targets, bundle identifiers, and entitlements.
2. `prepareBuildRequestForPlatformAsync` gathers credentials before project sync.
3. `ensureIosCredentialsAsync` constructs `IosCredentialsProvider`.
4. `IosCredentialsProvider` chooses either:
   - local build credentials from `credentials.json`
   - remote build credentials managed through EAS GraphQL and Apple APIs
5. Remote mode runs per target:
   - best-effort Apple auth
   - bundle ID existence check
   - capability sync from target entitlements
   - provisioning profile setup for the requested distribution type
   - `IosAppBuildCredentials` assignment
6. Prepared credentials are serialized into the build job as base64 P12 + base64 mobileprovision per target.

Credential gathering is skipped entirely when:

- the build is for a simulator
- the build profile sets `withoutCredentials`

The final build payload sent to the builder uses this shape:

- target name
- distribution certificate base64 + password
- provisioning profile base64

Source:

- `packages/eas-cli/src/build/build.ts`
- `packages/eas-cli/src/build/ios/build.ts`
- `packages/eas-cli/src/build/ios/credentials.ts`
- `packages/eas-cli/src/build/ios/prepareJob.ts`

## Target Resolution and Entitlement Input

### Bare workflow

For bare/native iOS projects, EAS CLI:

- resolves the application target and signable dependencies from the Xcode project
- reads each target's bundle identifier
- reads each target's entitlements plist from the native project
- preserves parent-child target relationships through `parentBundleIdentifier`

Each signable dependency gets its own `Target`, which means separate provisioning profile handling later.

Source:

- `packages/eas-cli/src/project/ios/target.ts`
- `packages/eas-cli/src/project/ios/entitlements.ts`

### Managed workflow

For managed projects, EAS CLI:

- introspects config with `npx expo config --json --type introspect`
- merges the build environment into `process.env` while introspecting
- sets `EXPO_NO_DOTENV=1` for that introspection subprocess
- falls back to the version of `@expo/config-plugins` bundled in EAS CLI if the external `npx expo config` path fails
- uses `exp.ios.entitlements` as the application target entitlements
- reads extra app extension targets from `extra.eas.build.experimental.ios.appExtensions`

Each managed extension can define:

- `targetName`
- `bundleIdentifier`
- `parentBundleIdentifier`
- `entitlements`

Source:

- `packages/eas-cli/src/project/ios/target.ts`
- `packages/eas-cli/src/project/ios/entitlements.ts`

## Apple Account Authentication

## Authentication Modes

EAS CLI supports two Apple-side auth modes:

- `AuthenticationMode.USER`
  - cookie/session based
  - richer functionality
  - can require interactive 2FA
- `AuthenticationMode.API_KEY`
  - App Store Connect API key based
  - better for CI/non-interactive use
  - less capable for some portal actions

Default mode selection:

- if any of `EXPO_ASC_API_KEY_PATH`, `EXPO_ASC_KEY_ID`, or `EXPO_ASC_ISSUER_ID` is set, default mode is API key
- otherwise default mode is user auth

Source:

- `packages/eas-cli/src/credentials/ios/appstore/AppStoreApi.ts`
- `packages/eas-cli/src/credentials/ios/appstore/resolveCredentials.ts`

## Best-Effort Auth During Build Credential Setup

`SetUpTargetBuildCredentials.runAsync` starts with `ctx.bestEffortAppStoreAuthenticateAsync()`.

This method is intentionally soft:

- if already authenticated, it does nothing
- if non-interactive, it does nothing
- if default mode is API key, it authenticates immediately
- if default mode is user auth, it prints an explanation and asks whether the user wants to log in

If the user declines:

- EAS CLI keeps going
- later steps may still force authentication if Apple access becomes necessary

That distinction matters:

- bundle ID sync only happens if `ctx.appStore.authCtx` exists
- creating or repairing Apple-managed provisioning profiles later still requires auth

Source:

- `packages/eas-cli/src/credentials/context.ts`
- `packages/eas-cli/src/credentials/ios/actions/SetUpTargetBuildCredentials.ts`

## User Auth Flow

User auth is implemented in `authenticateAsUserAsync`.

Resolution order:

1. try cookie-based login if cookies were provided
2. resolve Apple ID from options or env
3. if needed, prompt for Apple ID
4. attempt to restore a saved auth state from user credentials
5. if restore fails, log in with user credentials and password

Environment and cached inputs used by this path:

- `EXPO_APPLE_ID`
- `EXPO_APPLE_PASSWORD`
- `EXPO_APPLE_TEAM_ID`
- `EXPO_APPLE_PROVIDER_ID`

Password behavior:

- password may come from env
- otherwise EAS CLI tries local keychain lookup
- otherwise it prompts
- unless `EXPO_NO_KEYCHAIN` is set, successful password entry is saved to the local keychain
- invalid password errors remove the keychain entry and offer a retry

Other notes:

- username suggestions are cached via `JsonFileCache`
- provider auto-resolution is enabled through `autoResolveProvider: true`
- after login, EAS CLI fetches teams from Apple and resolves the active team from `authState.context.teamId`
- the returned user auth context includes `fastlaneSession`

Source:

- `packages/eas-cli/src/credentials/ios/appstore/authenticate.ts`
- `packages/eas-cli/src/credentials/ios/appstore/resolveCredentials.ts`

## API Key Auth Flow

API key auth is implemented in `authenticateWithApiKeyAsync`.

The CLI resolves:

- key contents from `ascApiKey.keyP8` or `EXPO_ASC_API_KEY_PATH`
- key id from `ascApiKey.keyId` or `EXPO_ASC_KEY_ID`
- issuer id from `ascApiKey.issuerId` or `EXPO_ASC_ISSUER_ID`
- team id and team type from options or env/prompt

Team metadata resolution for API key mode uses:

- `EXPO_APPLE_TEAM_ID`
- `EXPO_APPLE_TEAM_TYPE`

The request context is created with a JWT token valid for 20 minutes.

Important limitation:

- API key auth creates a token-only Apple request context
- some features require cookie-based user sessions and are therefore reduced or skipped

Source:

- `packages/eas-cli/src/credentials/ios/appstore/authenticate.ts`
- `packages/eas-cli/src/credentials/ios/appstore/resolveCredentials.ts`
- `packages/eas-cli/src/credentials/ios/utils/authType.ts`

## Auth Limitations and Reauthentication Rules

When code specifically requires user auth, `AppStoreApi.ensureUserAuthenticatedAsync` will:

- detect that an API key auth context is already cached
- clear it
- reauthenticate as a user

This matters for operations such as:

- push key management
- ASC API key listing/creation/revocation inside the CLI

Token-only API key sessions also affect bundle ID capability identifier syncing:

- linking or creating capability identifiers is skipped entirely for token-only sessions

Source:

- `packages/eas-cli/src/credentials/ios/appstore/AppStoreApi.ts`
- `packages/eas-cli/src/credentials/ios/appstore/capabilityIdentifiers.ts`

## Bundle Identifier and Entitlement Sync

## When It Runs

Bundle ID handling happens in `SetUpTargetBuildCredentials.runAsync`.

After best-effort auth:

- if `ctx.appStore.authCtx` exists, EAS CLI calls `ensureBundleIdExistsAsync`
- if the user never authenticated, this whole bundle ID + capability sync step is skipped

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpTargetBuildCredentials.ts`

## Bundle ID Creation

`ensureBundleIdExistsAsync` and `ensureBundleIdExistsWithNameAsync`:

- look up the bundle ID on Apple
- create it if missing
- then sync capabilities and capability identifiers if options were provided

Naming convention:

- the Apple-side bundle ID name is `@<account>/<project>`

### App Clip special case

EAS CLI treats a target as an App Clip if the entitlements include:

- `com.apple.developer.parent-application-identifiers`

For App Clips:

- the CLI must create the App Clip bundle ID using `BundleId.createAppClipAsync`
- the parent bundle identifier must already exist on Apple
- the parent bundle ID's opaque Apple ID is required

If the parent bundle ID is missing, the CLI throws.

Source:

- `packages/eas-cli/src/credentials/ios/appstore/ensureAppExists.ts`

## Capability Sync From Entitlements

Capability syncing is driven by `syncCapabilitiesForEntitlementsAsync`.

The algorithm is:

1. fetch current remote capabilities from Apple
2. iterate local entitlements
3. map supported entitlements to Apple capability types using `CapabilityMapping`
4. decide per capability whether to:
   - enable
   - disable
   - skip
5. submit a patch request to Apple
6. after capability sync, run capability identifier sync

Supported capabilities are hardcoded in `CapabilityMapping`.

Unsupported entitlements:

- are ignored
- in debug mode the CLI logs that they were skipped

Capability syncing can be fully disabled with:

- `EXPO_NO_CAPABILITY_SYNC=1`

Source:

- `packages/eas-cli/src/credentials/ios/appstore/bundleIdCapabilities.ts`
- `packages/eas-cli/src/credentials/ios/appstore/capabilityList.ts`

## Validation Rules for Entitlement Values

The CLI validates entitlement values before sending capability updates.

Examples:

- `com.apple.developer.applesignin`
  - must be an array containing allowed values such as `["Default"]`
- `aps-environment`
  - must be `"development"` or `"production"`
- `com.apple.security.application-groups`
  - must be an array of strings prefixed with `group.`
- `com.apple.developer.in-app-payments`
  - must be an array of strings prefixed with `merchant.`
- `com.apple.developer.icloud-container-identifiers`
  - must be an array of strings prefixed with `iCloud.`

Invalid entitlement values cause the CLI to throw before the Apple update request is sent.

Source:

- `packages/eas-cli/src/credentials/ios/appstore/bundleIdCapabilities.ts`
- `packages/eas-cli/src/credentials/ios/appstore/capabilityList.ts`

## Sign In with Apple Capability

The app capability "Sign In with Apple" is handled through entitlement sync, not through the Apple account auth layer.

Mapping:

- entitlement: `com.apple.developer.applesignin`
- Apple capability: `CapabilityType.APPLE_ID_AUTH`

Behavior:

- if the entitlement is present with valid options, the capability is enabled
- if the entitlement is removed, normal disable logic can turn the capability off
- this capability uses the "capability with settings" sync path

Source:

- `packages/eas-cli/src/credentials/ios/appstore/capabilityList.ts`

## Push Notifications Special Case

Push Notifications capability sync is driven by:

- entitlement `aps-environment`
- additional option `usesBroadcastPushNotifications`

If broadcast push notifications are enabled in config, the capability update uses the broadcast-specific push option.

Source:

- `packages/eas-cli/src/credentials/context.ts`
- `packages/eas-cli/src/credentials/ios/appstore/capabilityList.ts`

## Capability Identifier Sync

After capability toggles, EAS CLI runs `syncCapabilityIdentifiersForEntitlementsAsync`.

This only applies to capabilities that use identifier objects:

- App Groups
- Apple Pay merchant IDs
- iCloud containers

Behavior:

- reads identifier arrays from entitlements
- validates them
- fetches existing identifier objects from Apple
- creates missing identifier objects
- links all identifier object IDs back onto the bundle ID capability relationship

Important limitation:

- token-only API key sessions skip this entire step
- the CLI prints a warning explaining that cookies-based auth is required

Source:

- `packages/eas-cli/src/credentials/ios/appstore/capabilityIdentifiers.ts`

## Provisioning Profile Handling

## Distribution Routing

Per target, `SetUpTargetBuildCredentials.setupBuildCredentialsAsync` chooses the profile strategy like this:

- store distribution
  - use `SetUpProvisioningProfile` with `IosDistributionType.AppStore`
- internal distribution + `enterpriseProvisioning=adhoc`
  - use `SetUpAdhocProvisioningProfile`
- internal distribution + `enterpriseProvisioning=universal`
  - use `SetUpProvisioningProfile` with `IosDistributionType.Enterprise`
- internal distribution with no explicit `enterpriseProvisioning`
  - use `SetUpInternalProvisioningProfile`

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpTargetBuildCredentials.ts`

## Shared Facts Across All Profile Flows

- distribution certificate setup happens before profile setup
- each target gets separate build credentials
- multi-target builds can share a distribution certificate
- each target still needs its own provisioning profile

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpBuildCredentials.ts`
- `packages/eas-cli/src/credentials/ios/actions/SetUpDistributionCertificate.ts`

## Validation Before Reuse

`validateProvisioningProfileAsync` validates an existing profile in two layers.

### Local-only validation

The CLI:

- parses the base64 mobileprovision plist
- calculates the SHA-1 fingerprint for the uploaded P12 certificate
- checks that the profile's first `DeveloperCertificates` entry matches that fingerprint
- checks the profile bundle ID through `application-identifier`
- allows wildcard matching through `minimatch`
- rejects expired profiles

### Apple-side validation

If authenticated, the CLI also:

- lists Apple-side profiles for the bundle ID and expected profile class
- matches by `developerPortalIdentifier` first, otherwise by raw profile content
- requires the Apple-side status to be `ACTIVE`

If not authenticated:

- Apple-side validation is skipped
- the CLI accepts the result of local validation only

Source:

- `packages/eas-cli/src/credentials/ios/validators/validateProvisioningProfile.ts`

## Standard App Store and Enterprise Profiles

`SetUpProvisioningProfile` is used for:

- App Store distribution
- Enterprise universal distribution

Behavior:

1. resolve distribution certificate
2. if current build credentials validate, reuse them
3. if a fix is needed:
   - `--freeze-credentials` blocks the change
   - non-interactive mode without API key auth blocks the change
4. if there is no current profile, create a new one
5. if there is a current EAS profile but it no longer exists on Apple, create a new one and delete the old EAS profile record
6. if the Apple profile still exists, ask whether to reuse it
7. if reusing, attempt to reconfigure it against the chosen distribution certificate
8. if reconfiguration fails, create a new one and delete the old EAS profile record

Creation path details:

- Apple profile name is `*[expo] <bundleId> <AppStore|Enterprise> <ISO timestamp>`
- device list is empty
- profile type is derived from platform plus enterprise vs non-enterprise mode

Reconfiguration path details:

- the CLI finds the Apple profile by portal ID or profile content
- it swaps or confirms the distribution certificate association
- it regenerates the Apple profile
- it updates the stored EAS `AppleProvisioningProfile` blob

Token-only API key sessions use manual regeneration paths where needed.

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/actions/CreateProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/actions/ConfigureProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/appstore/provisioningProfile.ts`

## Interactive Manual Profile Input in Standard Flow

During standard profile creation, the CLI can offer a user-provided provisioning profile file.

Important details:

- this only exists in the standard profile creation path
- the CLI still authenticates with Apple before reaching that branch
- user-provided profiles do not have Apple portal IDs
- the CLI warns that it cannot validate such profiles properly

This is different from `credentials.json` upload mode, which is discussed later.

Source:

- `packages/eas-cli/src/credentials/ios/actions/CreateProvisioningProfile.ts`

## Internal Distribution Resolution

`SetUpInternalProvisioningProfile` decides between ad hoc and universal enterprise profiles when `enterpriseProvisioning` was not explicitly set.

Interactive behavior:

- if authenticated and the Apple team is in-house, ask the user which profile family to use
- if authenticated and the team is not in-house, force ad hoc
- if not authenticated:
  - if both kinds already exist on EAS, ask which to use
  - if only one exists, use it
  - if none exist, force login and then decide again based on team type

Non-interactive behavior:

- if both ad hoc and enterprise credentials exist, throw and require `enterpriseProvisioning` in `eas.json`
- if only one exists, use it
- if none exist, throw and require an interactive run

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpInternalProvisioningProfile.ts`

## Ad Hoc Provisioning Profiles

Ad hoc handling is materially different because device selection is part of the flow.

### High-level behavior

`SetUpAdhocProvisioningProfile` does this:

1. resolve ad hoc distribution certificate
2. validate current ad hoc build credentials if they exist
3. in non-interactive mode:
   - reuse only if current credentials are already valid
   - otherwise throw
4. resolve Apple team
5. fetch Apple devices registered in EAS for that team
6. if there are no EAS devices, ask to register them now
7. let the user choose which registered devices to include
8. create or reuse an Apple ad hoc profile for those UDIDs and the selected certificate
9. mirror that Apple profile back into the EAS `AppleProvisioningProfile` record
10. assign `IosAppBuildCredentials`

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpAdhocProvisioningProfile.ts`

### Reuse behavior when a valid ad hoc profile already exists

If current build credentials validate:

- EAS CLI compares the set of EAS-registered devices on the team against the set of devices currently provisioned in the stored profile
- if they match, the user can:
  - reuse the profile
  - show devices and then decide again
  - choose devices again
- if the profile is missing some registered devices, the CLI shows the missing set and asks whether the user wants to choose devices again

If the user answers "no" to choosing again in that mismatch case, the CLI reuses the incomplete profile.

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpAdhocProvisioningProfile.ts`

### Apple-side ad hoc profile management

Apple-side ad hoc management is implemented in `createOrReuseAdhocProvisioningProfileAsync`.

Algorithm:

1. register any missing UDIDs on Apple
2. look for valid `*[expo]` ad hoc profiles for the bundle ID and profile type
3. prefer profiles already tied to the desired certificate serial number
4. if a matching profile with the exact device set exists and is valid, reuse it
5. if a matching profile exists but the device set is incomplete, regenerate it with the full selected device list
6. intended behavior: if only an Expo-managed profile with the wrong certificate exists, replace the certificate and regenerate it
7. if no suitable profile exists, create a new one

Creation naming convention:

- `*[expo] <bundleId> AdHoc <timestamp>`

Supported Apple profile types here:

- iOS ad hoc
- tvOS ad hoc

Low-level profile type resolution explicitly rejects:

- macOS
- visionOS

Important `v18.4.0` implementation note:

- `findProfileAsync` uses array truthiness checks for `expoProfilesWithCertificate` and `expoProfiles`
- because empty arrays are truthy in JavaScript, the intended "existing Expo profile with wrong certificate" branch is effectively unreachable as written
- when Expo-managed profiles exist but none use the requested certificate, the code behaves like "no matching reusable profile found" and falls through toward new profile creation instead of updating the existing Expo-managed profile in place

Source:

- `packages/eas-cli/src/credentials/ios/appstore/provisioningProfileAdhoc.ts`
- `packages/eas-cli/src/credentials/ios/appstore/provisioningProfile.ts`

### How device registration interacts with ad hoc profiles

The EAS device list is the source of device selection in the CLI.

The Apple Developer Portal is updated later:

- if a chosen UDID is not yet on Apple, the CLI creates it on Apple during ad hoc profile management
- newly created Apple-side devices use the placeholder name `iOS Device (added by Expo)`

This means:

- most EAS device registration methods only create EAS-side `AppleDevice` records
- Apple-side device creation is deferred until ad hoc profile generation or regeneration

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpAdhocProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/appstore/provisioningProfileAdhoc.ts`

### Post-generation device reconciliation

After EAS uploads or updates the ad hoc profile record, it compares:

- user-selected devices
- `appleDevices` returned on the resulting `AppleProvisioningProfile`

If some selected devices are missing:

- the CLI warns
- it explains that Apple may still be processing devices for 24-72 hours
- it asks whether to continue without them

Inference:

- EAS CLI never manually writes `appleDevices` onto the profile object
- the Expo backend is therefore likely deriving that device list from the uploaded profile blob or linked Apple metadata

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpAdhocProvisioningProfile.ts`
- `packages/eas-cli/src/graphql/types/credentials/AppleProvisioningProfile.ts`

## Profile Type Guards for Local Credentials

When `credentialsSource` is local, EAS CLI does not run the remote Apple profile setup flow.

It reads raw credentials from `credentials.json` and checks profile type locally:

- ad hoc profile detection: `ProvisionedDevices` exists
- universal enterprise profile detection: `ProvisionsAllDevices` exists

Rules:

- internal builds require ad hoc unless `enterpriseProvisioning=universal`
- app store builds reject ad hoc profiles

Source:

- `packages/eas-cli/src/credentials/ios/IosCredentialsProvider.ts`
- `packages/eas-cli/src/credentials/ios/utils/provisioningProfile.ts`

## Device Handling

## Device Registration Entry Point

The CLI entry point is `eas device:create`.

It is explicitly interactive.

High-level flow:

1. resolve which Expo account should own the device registrations
2. authenticate with Apple
3. create or reuse the corresponding EAS `AppleTeam`
4. ask how the user wants to register devices

Source:

- `packages/eas-cli/src/commands/device/create.ts`
- `packages/eas-cli/src/devices/manager.ts`

## Account and Team Resolution

Device registration is attached to:

- an Expo account
- an EAS `AppleTeam` record under that account

If the CLI is running inside a project with a known owner account:

- it offers to use that account first

Then it authenticates with Apple and runs:

- `createOrGetExistingAppleTeamAndUpdateNameIfChangedAsync`

That keeps the Expo-side `AppleTeam` record aligned with the active Apple team identifier and name.

Source:

- `packages/eas-cli/src/devices/manager.ts`
- `packages/eas-cli/src/credentials/ios/api/GraphqlClient.ts`

## Registration Methods

The CLI offers these device registration methods:

- Website
- Developer Portal
- Input
- Current Machine

Source:

- `packages/eas-cli/src/devices/actions/create/action.ts`

### Website method

Behavior:

- create `AppleDeviceRegistrationRequest` in EAS GraphQL
- print a QR code and Expo website URL
- actual registration continues outside the CLI

The URL shape is:

- `https://.../register-device/<request-id>`

The CLI itself does not register the device on Apple in this branch.

Source:

- `packages/eas-cli/src/devices/actions/create/registrationUrlMethod.ts`
- `packages/eas-cli/src/credentials/ios/api/graphql/mutations/AppleDeviceRegistrationRequestMutation.ts`

### Developer Portal import method

Behavior:

- fetch devices already present on Apple through `Device.getAsync`
- fetch existing EAS devices for the same Apple team
- filter out UDIDs already imported to EAS
- let the user choose which Apple devices to import
- create EAS `AppleDevice` records in chunks of 10

Supported imported device classes:

- iPad
- iPhone
- Apple TV
- Mac

Important implementation detail:

- Apple TV devices are included in the Apple-side filter
- but the inspected `DeviceClass -> AppleDeviceClass` mapping only covers iPad, iPhone, and Mac
- so Apple TV imports are sent to GraphQL with `deviceClass` omitted

This path imports Apple -> EAS. It does not create new Apple devices.

Source:

- `packages/eas-cli/src/devices/actions/create/developerPortalMethod.ts`
- `packages/eas-cli/src/credentials/ios/api/graphql/queries/AppleDeviceQuery.ts`
- `packages/eas-cli/src/credentials/ios/api/graphql/mutations/AppleDeviceMutation.ts`

### Input method

Behavior:

- prompt for UDID
- prompt for name
- prompt for device class
- confirm
- create an EAS `AppleDevice`

This path creates an EAS record only.

Source:

- `packages/eas-cli/src/devices/actions/create/inputMethod.ts`

### Current Machine method

Behavior:

- only available on Apple Silicon macOS machines
- reads `provisioning_UDID` from `system_profiler -json SPHardwareDataType`
- prompts for the machine name
- stores device class as `Mac`
- creates an EAS `AppleDevice`

This path also creates an EAS record only.

Source:

- `packages/eas-cli/src/devices/actions/create/currentMachineMethod.ts`

## How Ad Hoc Builds Consume Devices

When ad hoc credentials are being prepared:

- the CLI fetches EAS devices with `getDevicesForAppleTeamAsync`
- selection is performed against that EAS-side list
- after a registration flow, ad hoc setup refetches with `useCache: false`

That cache bypass is important because website-based registration happens asynchronously outside the running CLI process.

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpAdhocProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/api/GraphqlClient.ts`
- `packages/eas-cli/src/credentials/ios/api/graphql/queries/AppleDeviceQuery.ts`

## Expo/EAS GraphQL Data Model

## AppleTeam

Created or reused by Apple team identifier.

If the team name changes:

- the CLI updates the existing EAS `AppleTeam`

Source:

- `packages/eas-cli/src/credentials/ios/api/GraphqlClient.ts`

## AppleAppIdentifier

Created lazily when needed.

Important behaviors:

- lookup is by bundle identifier within the Expo account
- if the record does not exist, it is created
- App Clip child app identifiers can reference a parent `AppleAppIdentifier`
- wildcard bundle identifiers require an Apple team

Source:

- `packages/eas-cli/src/credentials/ios/api/GraphqlClient.ts`

## IosAppCredentials and IosAppBuildCredentials

Model split:

- `IosAppCredentials`
  - common app-level iOS credential container
- `IosAppBuildCredentials`
  - distribution-specific binding to:
    - distribution certificate
    - provisioning profile
    - `iosDistributionType`

Create/update behavior:

- if no matching `IosAppBuildCredentials` exists, create it
- otherwise:
  - update distribution certificate reference
  - update provisioning profile reference

Source:

- `packages/eas-cli/src/credentials/ios/api/GraphqlClient.ts`
- `packages/eas-cli/src/credentials/ios/api/graphql/mutations/IosAppBuildCredentialsMutation.ts`

## AppleProvisioningProfile

EAS stores:

- base64 provisioning profile content
- optional Apple Developer Portal ID
- linked Apple team
- exposed `appleDevices`

Inference:

- the CLI only sends the raw profile blob and optional portal ID
- the presence of `appleDevices` on the returned fragment implies Expo backend-side parsing or enrichment

Source:

- `packages/eas-cli/src/credentials/ios/api/graphql/mutations/AppleProvisioningProfileMutation.ts`
- `packages/eas-cli/src/graphql/types/credentials/AppleProvisioningProfile.ts`

## Local Credentials vs Remote Credentials

## `credentialsSource: local`

For builds using local credentials:

- EAS CLI reads `credentials.json`
- relative paths are resolved from the project root
- P12 and provisioning profile files are loaded and base64 encoded
- target coverage is validated
- profile type is checked locally against the requested distribution mode
- no remote Apple bundle ID sync is performed
- no remote EAS GraphQL mutation is needed for build execution itself

Build workers receive the raw local cert/profile material directly.

Source:

- `packages/eas-cli/src/credentials/credentialsJson/read.ts`
- `packages/eas-cli/src/credentials/ios/IosCredentialsProvider.ts`

## Uploading `credentials.json` Into EAS

This is a different workflow from `credentialsSource: local`.

When using the credentials manager action that uploads local iOS credentials into EAS:

- the CLI reads the local P12 and mobileprovision
- reads the Apple team from the provisioning profile
- creates or reuses the EAS `AppleTeam`
- creates or reuses EAS distribution certificate records
- creates or reuses EAS provisioning profile records
- assigns `IosAppBuildCredentials`

Notes:

- equality checks are raw-content based
- if the current EAS profile blob or cert blob matches the local one, it is reused
- this path does not itself call Apple to validate the local profile contents

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpBuildCredentialsFromCredentialsJson.ts`
- `packages/eas-cli/src/credentials/ios/actions/SetUpTargetBuildCredentialsFromCredentialsJson.ts`

## Non-Interactive and Freeze Constraints

Important hard rules enforced by the CLI:

- if credentials are frozen and a profile needs to be created or repaired, the CLI throws
- non-interactive mode without API key auth cannot create or repair standard Apple-managed provisioning profiles
- non-interactive ad hoc setup can only reuse already-valid credentials
- if internal distribution is ambiguous between ad hoc and universal in non-interactive mode, the CLI throws and requires explicit `enterpriseProvisioning`

Source:

- `packages/eas-cli/src/credentials/ios/actions/SetUpProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/actions/CreateProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/actions/ConfigureProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/actions/SetUpInternalProvisioningProfile.ts`
- `packages/eas-cli/src/credentials/ios/actions/SetUpAdhocProvisioningProfile.ts`

## Notable Edge Cases and Implementation Details

- Apple bundle ID creation has explicit error handling for expired or unsigned Apple program agreements and surfaces a more actionable message.
- Bundle ID capability disable logic intentionally avoids disabling Game Center and In-App Purchase.
- Managed associated domains has a special-case guard to avoid being disabled when associated domains is still enabled.
- Some Apple APIs return stale profile data; `getProfilesForBundleIdAsync` probes returned profiles to filter out stale entries.
- Token-only API key contexts use `regenerateManuallyAsync` in some profile regeneration paths.
- Standard profile generation uses empty device lists, while ad hoc profile generation carries explicit device IDs.
- The CLI resolves Apple platform from Xcode target build settings when possible. Low-level provisioning profile type resolution still only supports iOS and tvOS in the inspected code.

Source:

- `packages/eas-cli/src/credentials/ios/appstore/ensureAppExists.ts`
- `packages/eas-cli/src/credentials/ios/appstore/bundleIdCapabilities.ts`
- `packages/eas-cli/src/credentials/ios/appstore/bundleId.ts`
- `packages/eas-cli/src/credentials/ios/appstore/provisioningProfile.ts`

## Practical End-to-End Summaries

## App Store build in remote mode

Expected flow:

1. resolve targets and entitlements
2. optionally authenticate with Apple
3. if authenticated, ensure bundle IDs exist and sync supported capabilities
4. choose or create a valid distribution certificate
5. validate existing App Store profile
6. reuse, repair, or create the profile
7. assign `IosAppBuildCredentials`
8. send base64 cert/profile to the builder

## Internal ad hoc build in remote mode

Expected flow:

1. resolve targets and entitlements
2. authenticate if needed
3. ensure bundle IDs exist and sync capabilities when authenticated
4. choose or create ad hoc distribution certificate
5. fetch EAS devices for the team
6. allow user to register or import devices if needed
7. choose which devices to provision
8. create or regenerate the Apple ad hoc profile so it matches:
   - chosen UDIDs
   - selected certificate
   - correct bundle ID
9. mirror the resulting profile into EAS
10. assign `IosAppBuildCredentials`

## Core Conclusion

EAS CLI treats iOS credential handling as a layered system:

- project inspection produces targets and entitlements
- Apple auth unlocks bundle ID and profile automation
- EAS GraphQL stores normalized copies of Apple teams, app identifiers, devices, certs, and profiles
- provisioning profile behavior branches mainly by distribution type
- ad hoc flows are device-driven
- standard App Store and enterprise flows are certificate-and-profile driven
- local credential builds bypass most of that remote orchestration and go straight to raw credential delivery

