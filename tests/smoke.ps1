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
    $database = Join-Path $env:APPDATA "io.loquara.desktop\loquara.sqlite3"
    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            throw "Loquara exited during startup (exit $($process.ExitCode))."
        }
        $healthy = Test-Path -LiteralPath $database -PathType Leaf
    } while (-not $healthy -and [DateTime]::UtcNow -lt $deadline)

    if (-not $healthy) {
        throw "Loquara did not initialize its database within $StartupTimeoutSeconds seconds."
    }
    Write-Host "Smoke OK: PID $($process.Id), native engine ready, DB $database"
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
