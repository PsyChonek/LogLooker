# winget manifests

Source of truth for the `PsyChonek.LogLooker` winget package. winget does not
read them from here - they have to be copied into
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) as a PR. Same
layout and scripts as `PsyChonek/SqlPlanForDummies`.

## Releasing a version

Normally none of this is run by hand: **Actions -> Release -> Run workflow** bumps
the version, builds x64 and ARM64 installers, regenerates these manifests from the
MSIs, commits them, and opens the winget PR via `winget-releaser`. It needs a `WINGET_TOKEN` secret in
the `release` environment and a fork of winget-pkgs under your account. Submission
is skipped while the repository is private or when `skip_winget` is selected.

The manual path, for when the token is missing or a release needs redoing:

```powershell
npm run build                                   # bump + build the MSI
./scripts/update-winget-manifests.ps1           # fill in SHA256 + ProductCode
New-Item -ItemType Directory -Path dist/winget -Force | Out-Null
Copy-Item winget/*.yaml dist/winget
winget validate --manifest dist/winget          # only the three YAML manifests
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
- `InstallerType: wix` and `Scope: machine` match Tauri's generated WiX package,
  whose `InstallScope` is `perMachine`.
- The identifier in `tauri.conf.json` (`com.psychonek.loglooker`) determines the
  MSI's UpgradeCode. Changing it makes winget see a different product and stop
  offering upgrades, so it stays put. `bundle.publisher` is set to `PsyChonek`
  because Tauri would otherwise derive the MSI's Manufacturer from the second
  segment of the identifier.
- GitHub releases build both x64 and ARM64. Local `npm run build` builds x64;
  the manifest script includes every architecture whose MSI is present.
