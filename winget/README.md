# winget manifests

Source of truth for the `PsyChonek.LogLooker` winget package. winget does not
read them from here - they have to be copied into
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) as a PR. Same
layout and scripts as `PsyChonek/SqlPlanForDummies`.

## Releasing a version

Normally none of this is run by hand: **Actions -> Release -> Run workflow** bumps
the version, builds the MSI, regenerates these manifests from it, commits them,
and opens the winget PR via `winget-releaser`. It needs a `WINGET_TOKEN` secret in
the `release` environment and a fork of winget-pkgs under your account.

The manual path, for when the token is missing or a release needs redoing:

```powershell
npm run build                                   # bump + build the MSI
./scripts/update-winget-manifests.ps1           # fill in SHA256 + ProductCode
winget validate --manifest winget               # optional
./scripts/submit-to-winget.ps1                  # push a branch to your fork
```

Then open the PR from the link the last script prints.

`update-winget-manifests.ps1` reads the SHA256 and the **ProductCode** out of the
built MSI rather than having them typed. That matters: the ProductCode changes on
every version *and* whenever the bundle identifier changes, and a stale one makes
winget unable to tell an installed package from an absent one.

`submit-to-winget.ps1` refuses to submit if the manifests still hold placeholder
values, or if their `PackageVersion` disagrees with the version being submitted.

## Notes

- **Not code-signed.** SmartScreen warns on first run; winget installs it anyway.
  Signing is the only real fix and needs a certificate.
- `InstallerType: msi` and no `Scope`, matching the SqlPlanForDummies manifests
  that winget already accepted. Claiming a scope the MSI does not install with
  makes winget mis-detect it.
- The identifier in `tauri.conf.json` (`com.psychonek.loglooker`) determines the
  MSI's UpgradeCode. Changing it makes winget see a different product and stop
  offering upgrades, so it stays put. `bundle.publisher` is set to `PsyChonek`
  because Tauri would otherwise derive the MSI's Manufacturer from the second
  segment of the identifier.
- Only x64 is built today. The script emits an arm64 entry automatically if an
  arm64 MSI is present.
