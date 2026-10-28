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

# ---- Console UI ------------------------------------------------------------
# One line per step. The active step is redrawn in place (flowing wave, shimmering
# progress bar); finished steps keep a single summary line. Output that is not an
# interactive console (pipes, CI, FLUXGET_PLAIN=1) gets plain "[n/total]" lines.
# The release bootstrap hands its step count to the payload installer through
# FLUXGET_UI_* so both halves read as one continuous run.
$script:UiIndex = 0
$script:UiTotal = 0
$script:UiLastLen = 0
$script:UiIsChild = $false
$script:UiLive = $false
if ($env:FLUXGET_UI_STEP -match '^\d+$' -and $env:FLUXGET_UI_TOTAL -match '^\d+$') {
    $script:UiIndex = [int]$env:FLUXGET_UI_STEP
    $script:UiTotal = [int]$env:FLUXGET_UI_TOTAL
    $script:UiIsChild = $true
}
if ($env:FLUXGET_UI_LIVE -match '^[01]$') {
    $script:UiLive = $env:FLUXGET_UI_LIVE -eq '1'
} else {
    try {
        $script:UiLive = (-not [Console]::IsOutputRedirected) -and (-not $env:FLUXGET_PLAIN) -and
            (-not $env:CI) -and ($Host.Name -eq 'ConsoleHost') -and ([Console]::WindowWidth -ge 40)
    } catch { $script:UiLive = $false }
}
$script:UiUnicode = -not $env:FLUXGET_ASCII
$script:UiFancyMarks = $script:UiUnicode -and ($env:WT_SESSION -or $PSVersionTable.PSVersion.Major -ge 6)
if ($script:UiUnicode) {
    $script:UiFull = [string][char]0x2588
    $script:UiEmpty = [string][char]0x2591
    $script:UiWave = @(0x2581..0x2588 | ForEach-Object { [string][char]$_ })
    $script:UiDot = ' ' + [char]0xB7 + ' '
} else {
    $script:UiFull = '#'
    $script:UiEmpty = '-'
    $script:UiWave = @('_', '.', '-', '~', '=', '+', '*', '#')
    $script:UiDot = ' - '
}
$script:UiCheck = if ($script:UiFancyMarks) { [string][char]0x2714 } elseif ($script:UiUnicode) { [string][char]0x221A } else { '+' }
$script:UiCross = if ($script:UiFancyMarks) { [string][char]0x2718 } elseif ($script:UiUnicode) { [string][char]0xD7 } else { 'x' }
$script:UiSkip = if ($script:UiUnicode) { [string][char]0x2013 } else { '-' }
# One period of a sine wave, scaled to the eight block heights.
$script:UiWaveShape = @(4, 5, 6, 7, 7, 7, 6, 5, 4, 2, 1, 0, 0, 0, 1, 2)

function Format-Size([double]$Bytes) {
    $inv = [Globalization.CultureInfo]::InvariantCulture
    if ($Bytes -ge 1MB) { return ($Bytes / 1MB).ToString('0.0', $inv) + ' MB' }
    if ($Bytes -ge 1KB) { return [Math]::Round($Bytes / 1KB).ToString($inv) + ' KB' }
    return [Math]::Round($Bytes).ToString($inv) + ' B'
}

# Draws coloured (text, colour) parts over the current line, clearing any leftovers.
function Write-LiveLine([object[]]$Parts) {
    $width = [Console]::WindowWidth - 1
    [Console]::CursorLeft = 0
    $length = 0
    foreach ($part in $Parts) {
        $text = [string]$part[0]
        if ($length + $text.Length -gt $width) { $text = $text.Substring(0, [Math]::Max(0, $width - $length)) }
        if ($text.Length -gt 0) { Write-Host -NoNewline -ForegroundColor $part[1] $text }
        $length += $text.Length
    }
    if ($length -lt $script:UiLastLen) {
        Write-Host -NoNewline (' ' * ($script:UiLastLen - $length))
        [Console]::CursorLeft = $length
    }
    $script:UiLastLen = $length
}

# The Add-* helpers append (text, colour) parts to a list; returning arrays of arrays
# from functions would be flattened by the pipeline.
function New-Parts { return , (New-Object 'System.Collections.Generic.List[object]') }

function Add-WaveParts($Parts, [int]$Frame, [int]$Cells) {
    for ($i = 0; $i -lt $Cells; $i++) {
        $height = $script:UiWaveShape[($Frame + $i * 2) % $script:UiWaveShape.Count]
        $color = if ($height -ge 6) { 'White' } elseif ($height -ge 3) { 'Cyan' } else { 'DarkCyan' }
        $Parts.Add(@($script:UiWave[$height], $color))
    }
}

function Add-BarParts($Parts, [double]$Fraction, [int]$Cells, [int]$Frame, [bool]$Known) {
    $colors = New-Object string[] $Cells
    $marks = New-Object string[] $Cells
    if ($Known) {
        $filled = [int][Math]::Floor($Fraction * $Cells)
        $glint = ($Frame % ($filled + 8)) - 4
        for ($i = 0; $i -lt $Cells; $i++) {
            if ($i -lt $filled) {
                $marks[$i] = $script:UiFull
                $colors[$i] = if ([Math]::Abs($i - $glint) -le 1) { 'White' } else { 'Cyan' }
            } else {
                $marks[$i] = $script:UiEmpty
                $colors[$i] = 'DarkGray'
            }
        }
    } else {
        # Total size unknown: a comet bounces along the track instead.
        $period = [Math]::Max(1, 2 * ($Cells - 1))
        $at = $Frame % $period
        $head = if ($at -lt $Cells) { $at } else { $period - $at }
        for ($i = 0; $i -lt $Cells; $i++) {
            $distance = [Math]::Abs($i - $head)
            $marks[$i] = if ($distance -le 2) { $script:UiFull } else { $script:UiEmpty }
            $colors[$i] = if ($distance -eq 0) { 'White' } elseif ($distance -eq 1) { 'Cyan' } elseif ($distance -eq 2) { 'DarkCyan' } else { 'DarkGray' }
        }
    }
    $run = ''
    for ($i = 0; $i -lt $Cells; $i++) {
        $run += $marks[$i]
        if ($i -eq $Cells - 1 -or $colors[$i + 1] -ne $colors[$i]) {
            $Parts.Add(@($run, $colors[$i]))
            $run = ''
        }
    }
}

function Write-UiHeader([string]$Detail) {
    if ($script:UiIsChild) { return }
    if (-not $script:UiLive) { Write-Host "fluxget installer ($Detail)"; return }
    Write-Host ''
    Write-Host -NoNewline '  '
    $wave = New-Parts
    Add-WaveParts $wave 0 7
    foreach ($part in $wave) { Write-Host -NoNewline -ForegroundColor $part[1] $part[0] }
    Write-Host -NoNewline -ForegroundColor White '  fluxget'
    Write-Host -ForegroundColor DarkGray '  installer'
    Write-Host -ForegroundColor DarkGray "  $Detail"
    Write-Host ''
}

# State is 'ok', 'skip' or 'fail'.
function Write-UiStep([string]$State, [string]$Label, [string]$Detail) {
    $script:UiIndex++
    $count = "$($script:UiIndex)/$($script:UiTotal)"
    if (-not $script:UiLive) {
        Write-Host "[$count] $Label  $Detail"
        return
    }
    if ($State -eq 'ok') { $mark = $script:UiCheck; $color = 'Green' }
    elseif ($State -eq 'skip') { $mark = $script:UiSkip; $color = 'DarkGray' }
    else { $mark = $script:UiCross; $color = 'Red' }
    Write-LiveLine @(@("  $count ".PadRight(7), 'DarkGray'), @("$mark ", $color), @($Label.PadRight(12), 'White'), @($Detail, 'Gray'))
    Write-Host ''
    $script:UiLastLen = 0
}

function Write-UiDone([string]$Version, [string]$Path) {
    if (-not $script:UiLive) { Write-Host "Installed $Version to $Path"; return }
    Write-Host ''
    Write-Host -NoNewline -ForegroundColor Green "  $($script:UiCheck) "
    Write-Host -ForegroundColor White "Installed $Version"
    Write-Host -ForegroundColor DarkGray "    $Path"
}

# Streams a file with a live progress line. Keeps animating even while the network stalls.
function Receive-UiFile([string]$Uri, [string]$OutFile, [string]$Label) {
    Add-Type -AssemblyName System.Net.Http
    $name = [IO.Path]::GetFileName($OutFile)
    $client = New-Object System.Net.Http.HttpClient
    $client.Timeout = [TimeSpan]::FromHours(2)
    $file = $null
    $stream = $null
    $response = $null
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $cursorHidden = $false
    try {
        try {
            $response = $client.GetAsync($Uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            if (-not $response.IsSuccessStatusCode) {
                throw ('server answered {0} {1}' -f [int]$response.StatusCode, $response.ReasonPhrase)
            }
            $total = $response.Content.Headers.ContentLength
            $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            $file = [System.IO.File]::Create($OutFile)
            $buffer = New-Object byte[] 262144
            $received = [long]0
            $speed = 0.0
            $sampleAt = 0
            $sampleBytes = [long]0
            $drawnAt = -1000
            $width = 80
            if ($script:UiLive) {
                try { [Console]::CursorVisible = $false; $cursorHidden = $true } catch { }
            }
            $pending = $stream.ReadAsync($buffer, 0, $buffer.Length)
            while ($true) {
                if (-not $pending.Wait(60)) {
                    # Nothing arrived yet: keep the animation moving.
                } else {
                    $count = $pending.Result
                    if ($count -le 0) { break }
                    $file.Write($buffer, 0, $count)
                    $received += $count
                    $pending = $stream.ReadAsync($buffer, 0, $buffer.Length)
                }
                $now = $clock.ElapsedMilliseconds
                if ($now - $sampleAt -ge 500) {
                    $instant = ($received - $sampleBytes) * 1000.0 / ($now - $sampleAt)
                    $speed = if ($speed -eq 0) { $instant } else { 0.6 * $speed + 0.4 * $instant }
                    $sampleAt = $now
                    $sampleBytes = $received
                }
                if ($script:UiLive -and $now - $drawnAt -ge 60) {
                    $drawnAt = $now
                    $frame = [int]($now / 70)
                    $width = [Console]::WindowWidth - 1
                    $known = $null -ne $total -and $total -gt 0
                    $fraction = if ($known) { [Math]::Min(1.0, $received / [double]$total) } else { 0.0 }
                    $tail = if ($known) {
                        '{0,3}%  {1} / {2}' -f [int][Math]::Floor($fraction * 100), (Format-Size $received), (Format-Size $total)
                    } else { Format-Size $received }
                    if ($speed -gt 0) { $tail += '  ' + (Format-Size $speed) + '/s' }
                    $fixed = 7 + 7 + 2 + 12 + 2 + 2 + $tail.Length
                    $cells = [Math]::Min(24, $width - $fixed)
                    $parts = New-Parts
                    $parts.Add(@(("  {0}/{1} " -f ($script:UiIndex + 1), $script:UiTotal).PadRight(7), 'DarkGray'))
                    Add-WaveParts $parts $frame 6
                    $parts.Add(@('  ', 'Gray'))
                    $parts.Add(@($Label.PadRight(12), 'White'))
                    $parts.Add(@('  ', 'Gray'))
                    if ($cells -ge 8) {
                        Add-BarParts $parts $fraction $cells $frame $known
                        $parts.Add(@('  ', 'Gray'))
                    }
                    $parts.Add(@($tail, 'Gray'))
                    Write-LiveLine $parts
                }
            }
            $file.Dispose(); $file = $null
        } catch {
            if ($file) { $file.Dispose(); $file = $null }
            if ($cursorHidden) { [Console]::CursorVisible = $true; $cursorHidden = $false }
            $reason = if ($_.Exception.InnerException) { $_.Exception.InnerException.Message } else { $_.Exception.Message }
            Write-UiStep 'fail' 'Download failed' $name
            throw "Could not download ${Uri}: $reason"
        }
    } finally {
        if ($cursorHidden) { try { [Console]::CursorVisible = $true } catch { } }
        if ($file) { $file.Dispose() }
        if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }
        $client.Dispose()
    }
    $seconds = [Math]::Max(0.1, $clock.Elapsed.TotalSeconds)
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $average = Format-Size ($received / $seconds)
    Write-UiStep 'ok' ($Label -replace 'ing$', 'ed') ('{0}{1}{2} in {3}s ({4}/s)' -f $name, $script:UiDot, (Format-Size $received), $seconds.ToString('0.0', $inv), $average)
}
# ---------------------------------------------------------------------------

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
        $script:UiTotal = 6
        $releaseName = if ($Version -eq 'latest') { 'latest release' } else { "release $Version" }
        Write-UiHeader "Windows $architecture$($script:UiDot)$releaseName"
        $archive = Join-Path $downloadDir $asset
        $checksum = Join-Path $downloadDir ($asset + '.sha256')
        Receive-UiFile $downloadUrl $archive 'Downloading'
        $progressBefore = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        try { Invoke-WebRequest -UseBasicParsing -Uri ($downloadUrl + '.sha256') -OutFile $checksum }
        finally { $ProgressPreference = $progressBefore }
        $checksumText = (Get-Content -LiteralPath $checksum -Raw).Trim()
        if ($checksumText -notmatch ('^([a-fA-F0-9]{64})  ' + [Regex]::Escape($asset) + '$')) {
            throw 'Invalid release archive checksum file.'
        }
        if ((Get-Sha256 $archive) -ne $Matches[1]) {
            throw 'Release archive SHA-256 mismatch; existing installation preserved.'
        }
        Write-UiStep 'ok' 'Verified' 'SHA-256 matches the published checksum'
        $payloadDir = Join-Path $downloadDir 'package'
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $payloadDir)
        Write-UiStep 'ok' 'Unpacked' 'release package extracted'
        # The pasted bootstrap works under Restricted execution policy; run the
        # verified payload in a child with a process-only policy override too.
        $childArguments = @('-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $payloadDir 'install.ps1'), '-InstallDir', $InstallDir)
        if ($NoModifyPath) { $childArguments += '-NoModifyPath' }
        $env:FLUXGET_UI_STEP = [string]$script:UiIndex
        $env:FLUXGET_UI_TOTAL = [string]$script:UiTotal
        $env:FLUXGET_UI_LIVE = if ($script:UiLive) { '1' } else { '0' }
        try { & (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') @childArguments }
        finally { Remove-Item Env:\FLUXGET_UI_STEP, Env:\FLUXGET_UI_TOTAL, Env:\FLUXGET_UI_LIVE -ErrorAction SilentlyContinue }
        if ($LASTEXITCODE -ne 0) { throw 'Release installer failed; see the error above.' }
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

if (-not $script:UiIsChild) {
    $script:UiTotal = if ($sourceBinary) { 3 } else { 4 }
    Write-UiHeader 'installing from this package'
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
    Write-UiStep 'ok' 'Built' 'compiled from source'
}

# Release packages carry a binary checksum; source installs use the local build.
$checksumFile = Join-Path $PSScriptRoot 'SHA256SUMS'
$checkedDetail = 'version test passed'
if (Test-Path -LiteralPath $checksumFile) {
    $entry = @(Get-Content -LiteralPath $checksumFile | Where-Object { $_ -match '^[a-fA-F0-9]{64}  fluxget\.exe$' })
    if ($entry.Count -ne 1) { throw 'Package is missing a unique fluxget.exe checksum.' }
    $expected = $entry[0].Substring(0, 64)
    $actual = Get-Sha256 $sourceBinary
    if ($actual -ne $expected) { throw 'Package SHA-256 mismatch; installation stopped.' }
    $checkedDetail = 'checksum and version test passed'
}

$version = & $sourceBinary --version
if ($LASTEXITCODE -ne 0 -or "$version" -notmatch '^fluxget [0-9]') {
    throw 'The executable did not pass its version check; installation stopped.'
}
Write-UiStep 'ok' 'Checked' "$version$($script:UiDot)$checkedDetail"

$InstallDir = [System.IO.Path]::GetFullPath($InstallDir)
$binDir = Join-Path $InstallDir 'bin'
$targetBinary = Join-Path $binDir $binaryName
New-Item -ItemType Directory -Path $binDir -Force | Out-Null
if ((Test-Path -LiteralPath $targetBinary) -and
    (Get-Sha256 $targetBinary) -eq (Get-Sha256 $sourceBinary)) {
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
# fx is a short copy of fluxget. Leave an unrelated fx.exe alone.
$shortName = 'fx'
$shortBinary = Join-Path $binDir 'fx.exe'
$shortOurs = $true
if (Test-Path -LiteralPath $shortBinary) {
    $shortVersion = try { & $shortBinary --version 2>$null } catch { '' }
    $shortOurs = "$shortVersion" -match '^fluxget [0-9]'
}
if ($shortOurs) {
    try {
        if (-not (Test-Path -LiteralPath $shortBinary) -or (Get-Sha256 $shortBinary) -ne (Get-Sha256 $targetBinary)) {
            Copy-Item -LiteralPath $targetBinary -Destination $shortBinary -Force
        }
    } catch { $shortOurs = $false }
}
if (-not $shortOurs) { $shortName = $null }
$installedDetail = if ($shortName) { 'fluxget and fx in place, with documentation' } else { 'executable and documentation in place' }
if ($alreadyInstalled) {
    Write-UiStep 'skip' 'Installed' "$version is already installed; refreshing PATH and documentation."
} else {
    Write-UiStep 'ok' 'Installed' $installedDetail
}

if (-not $NoModifyPath) {
    $pathChanged = $false
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($userPath -split ';' | Where-Object { $_ })
    $present = @($entries | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') }).Count -gt 0
    if (-not $present) {
        $updated = if ([string]::IsNullOrEmpty($userPath)) { $binDir } else { $userPath.TrimEnd(';') + ';' + $binDir }
        [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
        $pathChanged = $true
    }
    if (@($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') }).Count -eq 0) {
        $env:Path = $binDir + ';' + $env:Path
    }
    if ($pathChanged) { Write-UiStep 'ok' 'PATH' 'added to your user PATH' }
    else { Write-UiStep 'skip' 'PATH' 'already on your user PATH' }
} else {
    Write-UiStep 'skip' 'PATH' 'left unchanged (-NoModifyPath)'
}

Write-UiDone $version $targetBinary
if ($NoModifyPath) {
    Write-Host 'PATH was left unchanged (-NoModifyPath).'
} else {
    Write-Host 'Open a new terminal and run: fx --help   (or fluxget --help)'
}
