param(
    [string]$Python = "python",
    [switch]$SkipInstall,
    [string]$WorkerPath = "",
    [int]$PingTimeoutSeconds = 10
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot
$requirements = Join-Path $repository "engine\requirements.txt"
if ([string]::IsNullOrWhiteSpace($WorkerPath)) {
    $worker = Join-Path $repository "engine\parakeet_worker.py"
}
else {
    $worker = $WorkerPath
}
if ($PingTimeoutSeconds -le 0) {
    throw "PingTimeoutSeconds must be positive."
}

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

$utf8 = New-Object System.Text.UTF8Encoding($false)
$startInfo = New-Object System.Diagnostics.ProcessStartInfo
$startInfo.FileName = $Python
$startInfo.Arguments = "-X utf8 -u `"$worker`""
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardInput = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $false
$startInfo.StandardOutputEncoding = $utf8
if ($null -ne $startInfo.PSObject.Properties["StandardInputEncoding"]) {
    $startInfo.StandardInputEncoding = $utf8
}

$process = New-Object System.Diagnostics.Process
$process.StartInfo = $startInfo
$originalInputEncoding = [Console]::InputEncoding
$processStarted = $false
try {
    # Windows PowerShell 5.1 has no StandardInputEncoding property and uses
    # Console.InputEncoding when it constructs Process.StandardInput.
    [Console]::InputEncoding = $utf8
    if (-not $process.Start()) {
        throw "Could not start Parakeet worker."
    }
    $processStarted = $true

    $pingBytes = $utf8.GetBytes(
        "{`"request_id`":`"setup-ping`",`"command`":`"ping`"}`n"
    )
    $process.StandardInput.BaseStream.Write($pingBytes, 0, $pingBytes.Length)
    $process.StandardInput.BaseStream.Flush()
    $process.StandardInput.Close()
    $timeoutMilliseconds = $PingTimeoutSeconds * 1000
    if (-not $process.WaitForExit($timeoutMilliseconds)) {
        throw "Parakeet worker ping timed out after $PingTimeoutSeconds seconds."
    }
    $ping = $process.StandardOutput.ReadToEnd().Trim()
    $workerExitCode = $process.ExitCode
}
finally {
    [Console]::InputEncoding = $originalInputEncoding
    if ($processStarted -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    $process.Dispose()
}

if ($workerExitCode -ne 0) {
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
