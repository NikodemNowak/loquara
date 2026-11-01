param(
    [string]$ExePath = "",
    [int]$StartupTimeoutSeconds = 30,
    [switch]$KeepRunning
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ExePath)) {
    $ExePath = Join-Path $repository "src-tauri\target\release\loquara.exe"
}
$ExePath = [IO.Path]::GetFullPath($ExePath)
if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    throw "Missing Loquara executable: $ExePath. Run 'pnpm tauri build' first."
}
if ($StartupTimeoutSeconds -le 0) {
    throw "StartupTimeoutSeconds must be positive."
}

$process = $null
try {
    $process = Start-Process `
        -FilePath $ExePath `
        -WorkingDirectory (Split-Path $ExePath) `
        -WindowStyle Hidden `
        -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)
    $healthPath = Join-Path $env:APPDATA "io.loquara.desktop\health.json"
    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            throw "Loquara exited during startup (exit $($process.ExitCode))."
        }
        $healthy = $false
        if (Test-Path -LiteralPath $healthPath -PathType Leaf) {
            try {
                $health = Get-Content -LiteralPath $healthPath -Raw | ConvertFrom-Json
                $healthy = $health.pid -eq $process.Id -and
                    $health.trayInitialized -eq $true -and
                    $health.identifier -eq "io.loquara.desktop"
            }
            catch {
                # The marker is atomic, but antivirus can briefly lock it.
            }
        }
    } while (-not $healthy -and [DateTime]::UtcNow -lt $deadline)

    if (-not $healthy) {
        throw "Loquara did not report startup readiness for PID $($process.Id) within $StartupTimeoutSeconds seconds."
    }
    Write-Host "Smoke OK: PID $($process.Id), version $($health.version), app state and tray initialized. Model inference is checked separately."
}
finally {
    if (
        $null -ne $process -and
        -not $KeepRunning -and
        -not $process.HasExited
    ) {
        Stop-Process -Id $process.Id
        $process.WaitForExit()
    }
}
