param(
    [string]$Python = $(if ($env:MOW_PYTHON) { $env:MOW_PYTHON } else { "python" }),
    [switch]$LocalOnly
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot
$arguments = @("-X", "utf8", "-m", "engine.download_model")
if ($LocalOnly) {
    $arguments += "--local-only"
}

Push-Location $repository
try {
    & $Python @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Could not download or verify the Parakeet model."
    }
}
finally {
    Pop-Location
}
