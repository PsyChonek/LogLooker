# In-app winget updates

LogLooker checks the `winget` source for `PsyChonek.LogLooker` shortly after the
main window opens. A popup appears only when a newer numeric release version is
available. Choose **Later** to dismiss it for this session, or use **Settings >
Check for updates** to check again and see connection or source errors.

**Update and close** rechecks availability, opens an interactive PowerShell
window, and exits all LogLooker windows. Active searches/downloads stop and
unsaved work is lost. The helper waits for the app process to exit before running:

```powershell
winget upgrade --id PsyChonek.LogLooker --exact --source winget --interactive
```

Follow any winget agreement, installer, or administrator prompts. The window
stays open with the result, including failures. Reopen LogLooker after completion.
The upgrade targets only LogLooker and preserves winget's normal package matching,
pinning, installer verification, and elevation behavior.

Checks use `winget show --versions --disable-interactivity`, with a 45-second
timeout and no console window. Missing winget, an unpublished package, unaccepted
source terms, and failed checks are errors, not an indication that the app is up
to date. Automatic checks stay quiet on errors; manual checks show instructions.
Versions are compared to the running Tauri app version, not the Cargo version.

The package must be published in the winget community source before this feature
can offer an upgrade. GitHub release availability alone is insufficient. See the
Microsoft documentation for [show](https://learn.microsoft.com/en-us/windows/package-manager/winget/show)
and [upgrade](https://learn.microsoft.com/en-us/windows/package-manager/winget/upgrade).
