param(
    [switch]$SetupEngine,
    [switch]$DownloadModel,
    [switch]$Built,
    [string]$Python = $(if ($env:MOW_PYTHON) { $env:MOW_PYTHON } else { "python" })
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot

if ($SetupEngine) {
    & (Join-Path $PSScriptRoot "setup-engine.ps1") -Python $Python
}
if ($DownloadModel) {
    & (Join-Path $PSScriptRoot "download-model.ps1") -Python $Python
}

if ($Built) {
    $executable = Join-Path $repository "src-tauri\target\release\mow.exe"
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Missing release executable: $executable"
    }
    Start-Process -FilePath $executable -WorkingDirectory (Split-Path $executable)
    return
}

Push-Location $repository
try {
    & pnpm tauri dev
    if ($LASTEXITCODE -ne 0) {
        throw "pnpm tauri dev failed."
    }
}
finally {
    Pop-Location
}
