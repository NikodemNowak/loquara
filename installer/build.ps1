param(
    [Parameter(Mandatory = $true)][string]$PayloadPath,
    [Parameter(Mandatory = $true)][string]$OutputPath,
    [Parameter(Mandatory = $true)][ValidatePattern('^\d+\.\d+\.\d+$')][string]$Version
)
$ErrorActionPreference = 'Stop'
$sourceRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$payload = (Resolve-Path -LiteralPath $PayloadPath).Path
$output = [IO.Path]::GetFullPath($OutputPath)
if ([IO.Path]::GetExtension($payload) -ne '.exe') { throw 'Payload must be a Tauri NSIS executable.' }
if ($payload -eq $output) { throw 'Output must not replace the source payload.' }
$payloadVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($payload).ProductVersion
if ($payloadVersion -ne $Version) { throw "Payload version $payloadVersion does not match requested $Version." }
$framework = Join-Path $env:WINDIR 'Microsoft.NET/Framework64/v4.0.30319'
$compiler = Join-Path $framework 'csc.exe'
if (!(Test-Path -LiteralPath $compiler)) { throw '.NET Framework 4 compiler was not found.' }
$wpf = Join-Path $framework 'WPF'
$stage = Join-Path ([IO.Path]::GetTempPath()) ('Loquara-build-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($stage) | Out-Null
try {
    # Hash the exact staged bytes passed to the compiler, never a mutable source.
    $stagedPayload = Join-Path $stage 'payload.exe'
    Copy-Item -LiteralPath $payload -Destination $stagedPayload
    $hash = (Get-FileHash -LiteralPath $stagedPayload -Algorithm SHA256).Hash.ToLowerInvariant()
    $length = (Get-Item -LiteralPath $stagedPayload).Length
    $metadata = Join-Path $stage 'PayloadMetadata.cs'
    @"
using System.Reflection;
[assembly: AssemblyTitle("Loquara Setup")]
[assembly: AssemblyProduct("Loquara")]
[assembly: AssemblyVersion("$Version.0")]
[assembly: AssemblyFileVersion("$Version.0")]
internal static class PayloadMetadata {
    internal const string Version = "$Version";
    internal const string Sha256 = "$hash";
    internal const long Length = ${length}L;
}
"@ | Set-Content -LiteralPath $metadata -Encoding UTF8
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($output)) | Out-Null
    $resources = Join-Path $stage 'Fonts.resources'
    # Static Inter 4.1 fonts from https://github.com/rsms/inter/releases/tag/v4.1;
    # the accompanying OFL license ships in assets/OFL.txt.
    $writer = [Resources.ResourceWriter]::new($resources)
    try {
        foreach ($font in @('Inter-Regular.ttf', 'Inter-SemiBold.ttf')) {
            $bytes = [IO.File]::ReadAllBytes((Join-Path $sourceRoot "assets/$font"))
            $writer.AddResource(('assets/' + $font.ToLowerInvariant()), [IO.MemoryStream]::new($bytes), $true)
        }
        $writer.Generate()
    } finally { $writer.Dispose() }
    $assemblyName = [IO.Path]::GetFileNameWithoutExtension($output)
    $icon = [IO.Path]::GetFullPath((Join-Path $sourceRoot '../src-tauri/icons/icon.ico'))
    if (!(Test-Path -LiteralPath $icon)) { throw 'The application Fold icon was not found.' }
    $arguments = @('/nologo', '/target:winexe', '/platform:x64', '/optimize+', '/warnaserror+', "/out:$output",
        "/win32manifest:$sourceRoot\app.manifest", "/win32icon:$icon",
        "/reference:$framework\System.dll", "/reference:$framework\System.Core.dll",
        "/reference:$framework\System.Xaml.dll", "/reference:$wpf\WindowsBase.dll",
        "/reference:$wpf\PresentationCore.dll", "/reference:$wpf\PresentationFramework.dll",
        "/resource:${stagedPayload},Loquara.Payload.exe", "/resource:${resources},$assemblyName.g.resources", "$sourceRoot\Bootstrapper.cs", $metadata)
    & $compiler @arguments
    if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed ($LASTEXITCODE)." }
    [PSCustomObject]@{ Output = $output; Version = $Version; PayloadBytes = $length; PayloadSha256 = $hash }
} finally {
    # Only known files in this invocation's unique staging directory are removed.
    foreach ($name in @('payload.exe', 'PayloadMetadata.cs', 'Fonts.resources')) {
        $file = Join-Path $stage $name
        if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file -Force }
    }
    if (Test-Path -LiteralPath $stage) { [IO.Directory]::Delete($stage, $false) }
}
