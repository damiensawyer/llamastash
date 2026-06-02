# install.ps1 — install llamastash from GitHub Releases on Windows.
#
# Usage:
#   irm https://llamastash.dev/install.ps1 | iex
#   irm https://github.com/llamastash/llamastash/releases/latest/download/install.ps1 | iex
#
# Parameters (named or environment):
#   -Version <vX.Y.Z>     Install a specific tag instead of the latest release.
#   -InstallDir <path>    Install into <path> instead of %LOCALAPPDATA%\Programs\llamastash.
#   -Quiet                Suppress progress chatter; errors still print.
#   -AddToPath            Append InstallDir to the user PATH (idempotent).
#
# Environment variables (used as defaults when the parameter is unset):
#   LLAMASTASH_VERSION       Same as -Version.
#   LLAMASTASH_INSTALL_DIR   Same as -InstallDir.
#   LLAMASTASH_QUIET=1       Same as -Quiet.
#
# Exit codes mirror install.sh:
#   0   success
#   1   generic failure (download error, network, unknown)
#   2   checksum verification failed
#   64  unsupported platform or invalid usage
#
# Only x86_64-pc-windows-msvc ships in 0.0.2; aarch64 is deferred per the
# Windows-support plan's Scope Boundaries. No admin elevation is required —
# %LOCALAPPDATA% is per-user.

[CmdletBinding()]
param(
  [string]$Version = $env:LLAMASTASH_VERSION,
  [string]$InstallDir = $env:LLAMASTASH_INSTALL_DIR,
  [switch]$Quiet,
  [switch]$AddToPath
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'llamastash/llamastash'
$BinName = 'llamastash.exe'

# Test-only overrides; in production these resolve to GitHub's real endpoints.
$BaseUrl = if ($env:LLAMASTASH_BASE_URL) { $env:LLAMASTASH_BASE_URL } else { "https://github.com/$Repo/releases/download" }
$LatestUrl = if ($env:LLAMASTASH_LATEST_URL) { $env:LLAMASTASH_LATEST_URL } else { "https://api.github.com/repos/$Repo/releases/latest" }

if ([string]::IsNullOrEmpty($InstallDir)) {
  $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\llamastash'
}

if (-not $Quiet -and $env:LLAMASTASH_QUIET -eq '1') {
  $Quiet = $true
}

function Write-Log {
  param([string]$Message)
  if (-not $Quiet) { Write-Host $Message }
}

function Exit-WithCode {
  param([int]$Code, [string]$Message)
  Write-Error $Message
  exit $Code
}

# ----- target detection -------------------------------------------------------

$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }

$target = switch -Regex ($arch) {
  '^(AMD64|x86_64)$' { 'x86_64-pc-windows-msvc' }
  default {
    Exit-WithCode 64 "unsupported Windows architecture: $arch (only x86_64 ships in 0.0.2)"
  }
}

# ----- resolve version --------------------------------------------------------

if ([string]::IsNullOrEmpty($Version)) {
  Write-Log 'resolving latest release tag…'
  try {
    $release = Invoke-RestMethod -Uri $LatestUrl -Headers @{ 'User-Agent' = 'llamastash-install.ps1' }
  } catch {
    Exit-WithCode 1 "could not fetch latest release metadata: $_"
  }
  $Version = $release.tag_name
  if ([string]::IsNullOrEmpty($Version)) {
    Exit-WithCode 1 'GH latest-release response has no tag_name'
  }
}

# Strip leading 'v' for the asset filename ('v0.0.2' -> '0.0.2').
$VersionBare = $Version -replace '^v', ''
$AssetName = "llamastash-$VersionBare-$target.zip"
$AssetUrl = "$BaseUrl/$Version/$AssetName"
$SumsUrl = "$BaseUrl/$Version/SHA256SUMS"

Write-Log "target:  $target"
Write-Log "version: $Version"
Write-Log "asset:   $AssetName"

# ----- download + verify ------------------------------------------------------

$tmpDir = Join-Path $env:TEMP "llamastash-install-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $tmpDir | Out-Null
try {
  $zipPath = Join-Path $tmpDir $AssetName
  $sumsPath = Join-Path $tmpDir 'SHA256SUMS'

  Write-Log "downloading $AssetUrl…"
  try {
    Invoke-WebRequest -Uri $AssetUrl -OutFile $zipPath -UseBasicParsing
  } catch {
    Exit-WithCode 1 "could not download ${AssetName}: $_"
  }

  Write-Log 'downloading SHA256SUMS…'
  try {
    Invoke-WebRequest -Uri $SumsUrl -OutFile $sumsPath -UseBasicParsing
  } catch {
    Exit-WithCode 1 "could not download SHA256SUMS: $_"
  }

  # Find the expected hex for our asset in SHA256SUMS — same shape as
  # `sha256sum -c`: <64-hex-chars>  <filename>, where the filename may carry a
  # leading '*' binary-mode marker (sha256sum --binary / shasum -b).
  $expected = (Get-Content $sumsPath | Where-Object { $_ -match "\s\*?$([regex]::Escape($AssetName))$" }) `
    -replace '^\s*([a-fA-F0-9]{64}).*$', '$1'
  if ($expected -eq '' -or $expected.Length -ne 64) {
    Exit-WithCode 2 "no SHA256SUMS entry found for $AssetName"
  }
  $actual = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash.ToLower()
  if ($actual -ne $expected.ToLower()) {
    Exit-WithCode 2 "SHA-256 mismatch: expected $expected, got $actual"
  }
  Write-Log 'checksum OK'

  # ----- extract --------------------------------------------------------------

  $extractDir = Join-Path $tmpDir 'extract'
  New-Item -ItemType Directory -Path $extractDir | Out-Null
  Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force

  # Asset is staged as llamastash-<ver>-<target>/llamastash.exe (matching
  # the release.yml package step).
  $sourceBin = Get-ChildItem -Recurse -Path $extractDir -Filter $BinName | Select-Object -First 1
  if (-not $sourceBin) {
    Exit-WithCode 1 "extracted archive did not contain $BinName"
  }

  if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir | Out-Null
  }
  $destBin = Join-Path $InstallDir $BinName
  Copy-Item -Path $sourceBin.FullName -Destination $destBin -Force
  Write-Log "installed: $destBin"

  # ----- PATH update (opt-in) -------------------------------------------------

  if ($AddToPath) {
    $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $entries = if ($userPath) { $userPath -split ';' } else { @() }
    if ($entries -notcontains $InstallDir) {
      $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
      [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
      Write-Log "added $InstallDir to user PATH (open a new terminal to pick it up)"
    } else {
      Write-Log "$InstallDir already on user PATH"
    }
  } else {
    Write-Log "tip: add `"$InstallDir`" to your user PATH, or rerun with -AddToPath"
  }
} finally {
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmpDir
}

Write-Log 'done. run `llamastash --help` to get started.'
