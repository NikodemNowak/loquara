param(
    [switch]$Built
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot

if ($Built) {
    $executable = Join-Path $repository "src-tauri\target\release\loquara.exe"
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Missing release executable: $executable. Run 'pnpm tauri build' first."
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
