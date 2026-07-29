param(
    [string]$Python = "python",
    [switch]$SkipInstall
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot
$requirements = Join-Path $repository "engine\requirements.txt"
$worker = Join-Path $repository "engine\parakeet_worker.py"

& $Python -c "import sys; assert sys.version_info >= (3, 10), 'Python 3.10 or newer is required'; print(f'Python {sys.version.split()[0]}')"
if ($LASTEXITCODE -ne 0) {
    throw "Python 3.10 or newer is required."
}

if (-not $SkipInstall) {
    & $Python -m pip install -r $requirements
    if ($LASTEXITCODE -ne 0) {
        throw "Could not install Parakeet engine requirements."
    }
}

& $Python -c "import torch; print(f'PyTorch {torch.__version__}'); print(f'CUDA available: {torch.cuda.is_available()}'); print(f'CUDA version: {torch.version.cuda or ""none""}')"
if ($LASTEXITCODE -ne 0) {
    throw "PyTorch is not installed. Install the correct CUDA build separately, then rerun this script."
}

$ping = '{"request_id":"setup-ping","command":"ping"}' |
    & $Python -u $worker
if ($LASTEXITCODE -ne 0) {
    throw "Parakeet worker ping failed."
}

$response = $ping | ConvertFrom-Json
if (
    $response.request_id -ne "setup-ping" -or
    -not $response.ok -or
    $response.result.status -ne "ready"
) {
    throw "Parakeet worker returned an invalid ping response: $ping"
}

Write-Host "Parakeet worker ping: ready (model was not downloaded or loaded)."
