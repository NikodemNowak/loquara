param(
    [string]$ExePath = "",
    [string]$Python = $(if ($env:MOW_PYTHON) { $env:MOW_PYTHON } else { "python" }),
    [int]$StartupTimeoutSeconds = 30,
    [switch]$KeepRunning
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ExePath)) {
    $ExePath = Join-Path $repository "src-tauri\target\release\mow.exe"
}
$ExePath = [IO.Path]::GetFullPath($ExePath)
if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    throw "Missing Mow executable: $ExePath"
}
if ($StartupTimeoutSeconds -le 0) {
    throw "StartupTimeoutSeconds must be positive."
}

$bundleRoot = Split-Path $ExePath
$workerCandidates = @(
    (Join-Path $bundleRoot "engine\parakeet_worker.py"),
    (Join-Path $bundleRoot "_up_\engine\parakeet_worker.py")
)
$worker = $workerCandidates |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1
if ([string]::IsNullOrWhiteSpace($worker)) {
    throw "Bundled Parakeet worker is missing next to $ExePath"
}

& (Join-Path $repository "scripts\setup-engine.ps1") `
    -Python $Python `
    -SkipInstall `
    -WorkerPath $worker

Push-Location $repository
try {
    & $Python -X utf8 -m engine.download_model --local-only
    if ($LASTEXITCODE -ne 0) {
        throw "The exact model revision is incomplete in cache."
    }
}
finally {
    Pop-Location
}

$process = $null
try {
    $process = Start-Process `
        -FilePath $ExePath `
        -WorkingDirectory (Split-Path $ExePath) `
        -WindowStyle Hidden `
        -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)
    $database = Join-Path $env:APPDATA "pl.mow.desktop\mow.sqlite3"
    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            throw "Mow exited during startup (exit $($process.ExitCode))."
        }
        $healthy = Test-Path -LiteralPath $database -PathType Leaf
    } while (-not $healthy -and [DateTime]::UtcNow -lt $deadline)

    if (-not $healthy) {
        throw "Mow did not initialize its database within $StartupTimeoutSeconds seconds."
    }
    Write-Host "Smoke OK: PID $($process.Id), worker ready, model complete, DB $database"
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
