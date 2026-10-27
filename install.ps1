#requires -Version 5.1
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\fluxget'),
    [switch]$NoModifyPath,
    [string]$Version = 'latest',
    [string]$ReleaseBaseUrl = 'https://github.com/Cosmos-0118/fluxget/releases'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Get-Sha256([string]$FilePath) {
    $stream = [System.IO.File]::OpenRead($FilePath)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $sha.Dispose() }
}
$binaryName = 'fluxget.exe'
$sourceBinary = $null
$localRoot = if ($PSScriptRoot) { $PSScriptRoot } else { $null }
$hasLocalPayload = $false
if ($localRoot) {
    foreach ($name in @('fluxget.exe', 'bin\fluxget.exe', 'target\release\fluxget.exe', 'Cargo.toml')) {
        if (Test-Path -LiteralPath (Join-Path $localRoot $name)) { $hasLocalPayload = $true }
    }
}
if (-not $hasLocalPayload) {
    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
    $architecture = switch ($architecture) { 'x64' { 'x86_64' } 'arm64' { 'aarch64' } default { throw "Unsupported Windows CPU: $architecture" } }
    if ($Version -ne 'latest' -and $Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+([-.][a-zA-Z0-9.]+)?$') {
        throw 'Version must be latest or a release tag such as v0.1.0.'
    }
    $baseUri = [Uri]$ReleaseBaseUrl
    if ($baseUri.Scheme -ne 'https' -and -not ($baseUri.Scheme -eq 'http' -and $baseUri.IsLoopback)) {
        throw 'Release downloads require HTTPS (loopback HTTP is allowed for tests).'
    }
    $releasePath = if ($Version -eq 'latest') { 'latest/download' } else { 'download/' + [Uri]::EscapeDataString($Version) }
    $asset = "fluxget-windows-$architecture.zip"
    $downloadUrl = $ReleaseBaseUrl.TrimEnd('/') + '/' + $releasePath + '/' + $asset
    $tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $downloadDir = Join-Path $tempBase ('fluxget-install-' + [Guid]::NewGuid().ToString('N'))
    if (-not [System.IO.Path]::GetFullPath($downloadDir).StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Invalid temporary installation path.'
    }
    New-Item -ItemType Directory -Path $downloadDir | Out-Null
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        Write-Host "Fetching $Version Windows $architecture release..."
        $archive = Join-Path $downloadDir $asset
        $checksum = Join-Path $downloadDir ($asset + '.sha256')
        Invoke-WebRequest -UseBasicParsing -Uri $downloadUrl -OutFile $archive
        Invoke-WebRequest -UseBasicParsing -Uri ($downloadUrl + '.sha256') -OutFile $checksum
        $checksumText = (Get-Content -LiteralPath $checksum -Raw).Trim()
        if ($checksumText -notmatch ('^([a-fA-F0-9]{64})  ' + [Regex]::Escape($asset) + '$')) {
            throw 'Invalid release archive checksum file.'
        }
        if ((Get-Sha256 $archive) -ne $Matches[1]) {
            throw 'Release archive SHA-256 mismatch; existing installation preserved.'
        }
        $payloadDir = Join-Path $downloadDir 'package'
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $payloadDir)
        $options = @{ InstallDir = $InstallDir; NoModifyPath = $NoModifyPath }
        & (Join-Path $payloadDir 'install.ps1') @options
        return
    } finally {
        Remove-Item -LiteralPath $downloadDir -Recurse -Force
    }
}
foreach ($candidate in @('fluxget.exe', 'bin\fluxget.exe', 'target\release\fluxget.exe')) {
    $candidatePath = Join-Path $localRoot $candidate
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) {
        $sourceBinary = $candidatePath
        break
    }
}

if (-not $sourceBinary) {
    if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'Cargo.toml'))) {
        throw 'No Windows executable in this package. Download the Windows release package.'
    }
    $cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
    $cargo = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
    if (-not (Test-Path -LiteralPath $cargo)) {
        throw 'Installing from source needs Rust 1.91+. Use the prebuilt Windows package for installation without build tools.'
    }
    Write-Host 'Building fluxget from source (first installation only)...'
    & $cargo build --release --locked --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml') --target-dir (Join-Path $PSScriptRoot 'target')
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed; no installed executable was changed.' }
    $sourceBinary = Join-Path $PSScriptRoot 'target\release\fluxget.exe'
}

# Release packages carry a binary checksum; source installs use the local build.
$checksumFile = Join-Path $PSScriptRoot 'SHA256SUMS'
if (Test-Path -LiteralPath $checksumFile) {
    $entry = @(Get-Content -LiteralPath $checksumFile | Where-Object { $_ -match '^[a-fA-F0-9]{64}  fluxget\.exe$' })
    if ($entry.Count -ne 1) { throw 'Package is missing a unique fluxget.exe checksum.' }
    $expected = $entry[0].Substring(0, 64)
    $actual = Get-Sha256 $sourceBinary
    if ($actual -ne $expected) { throw 'Package SHA-256 mismatch; installation stopped.' }
}

$version = & $sourceBinary --version
if ($LASTEXITCODE -ne 0 -or "$version" -notmatch '^fluxget [0-9]') {
    throw 'The executable did not pass its version check; installation stopped.'
}

$InstallDir = [System.IO.Path]::GetFullPath($InstallDir)
$binDir = Join-Path $InstallDir 'bin'
$targetBinary = Join-Path $binDir $binaryName
New-Item -ItemType Directory -Path $binDir -Force | Out-Null
if ((Test-Path -LiteralPath $targetBinary) -and
    (Get-Sha256 $targetBinary) -eq (Get-Sha256 $sourceBinary)) {
    Write-Host "$version is already installed; refreshing PATH and documentation."
    $alreadyInstalled = $true
} else { $alreadyInstalled = $false }
$temporaryBinary = Join-Path $binDir ('fluxget-' + [Guid]::NewGuid().ToString('N') + '.tmp.exe')
try {
    if (-not $alreadyInstalled) {
    Copy-Item -LiteralPath $sourceBinary -Destination $temporaryBinary
    if ((Get-Sha256 $temporaryBinary) -ne (Get-Sha256 $sourceBinary)) {
        throw 'Copied executable failed verification; existing installation preserved.'
    }
    if (Test-Path -LiteralPath $targetBinary) {
        $backupBinary = $temporaryBinary + '.previous'
        [System.IO.File]::Replace($temporaryBinary, $targetBinary, $backupBinary)
        Remove-Item -LiteralPath $backupBinary -Force
    } else {
        [System.IO.File]::Move($temporaryBinary, $targetBinary)
    }
    }
} finally {
    if (Test-Path -LiteralPath $temporaryBinary) { Remove-Item -LiteralPath $temporaryBinary -Force }
}

foreach ($name in @('README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.txt')) {
    $source = Join-Path $PSScriptRoot $name
    $target = Join-Path $InstallDir $name
    if ((Test-Path -LiteralPath $source) -and [System.IO.Path]::GetFullPath($source) -ne $target) {
        Copy-Item -LiteralPath $source -Destination $target -Force
    }
}

if (-not $NoModifyPath) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($userPath -split ';' | Where-Object { $_ })
    $present = @($entries | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') }).Count -gt 0
    if (-not $present) {
        $updated = if ([string]::IsNullOrEmpty($userPath)) { $binDir } else { $userPath.TrimEnd(';') + ';' + $binDir }
        [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
    }
    if (@($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') }).Count -eq 0) {
        $env:Path = $binDir + ';' + $env:Path
    }
}

Write-Host "Installed $version to $targetBinary"
if ($NoModifyPath) {
    Write-Host 'PATH was left unchanged (-NoModifyPath).'
} else {
    Write-Host 'Open a new terminal and run: fluxget --help'
}
