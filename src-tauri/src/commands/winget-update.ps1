$ErrorActionPreference = 'Stop'
$Host.UI.RawUI.WindowTitle = 'LogLooker update'
try {
    Write-Host 'Waiting for LogLooker to close...'
    $logLookerProcess = Get-Process -Id __LOGLOOKER_PID__ -ErrorAction SilentlyContinue
    if ($logLookerProcess) {
        if (-not $logLookerProcess.WaitForExit(60000)) {
            throw 'LogLooker is still running. Close it before trying the update again.'
        }
    }
    Write-Host 'Updating LogLooker with winget. Follow any installer or administrator prompts.'
    & winget.exe upgrade --id PsyChonek.LogLooker --exact --source winget --interactive
    if ($LASTEXITCODE -ne 0) {
        throw "Winget did not complete the update (exit code $LASTEXITCODE). Review the output above."
    }
    Write-Host 'Update complete. You can reopen LogLooker and close this window.'
} catch {
    Write-Host "Update failed: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host 'You can reopen LogLooker and retry the update later.'
}
