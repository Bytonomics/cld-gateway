# NOTE: Windows support is CURRENTLY UNSUPPORTED. This script is provided as a placeholder
# and is designed to be forward-compatible for eventual Windows support in a future release.
#
# STATUS: This script will be activated when cld-gateway Windows binaries become available.

[CmdletBinding()]
param(
    [string]$Release = $env:CLD_GATEWAY_RELEASE
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($Release)) {
    $Release = "latest"
}

$NonInteractive = $env:CLD_GATEWAY_NON_INTERACTIVE -match "^(?i:1|true|yes)$"

$defaultInstallDir = Join-Path $HOME ".local\bin"
$InstallDir = if ([string]::IsNullOrWhiteSpace($env:CLD_GATEWAY_INSTALL_DIR)) {
    $defaultInstallDir
} else {
    $env:CLD_GATEWAY_INSTALL_DIR
}
$BinPath = Join-Path $InstallDir "cld-gateway.exe"

function Write-Step {
    param(
        [string]$Message
    )

    Write-Host "==> $Message"
}

function Write-WarningStep {
    param(
        [string]$Message
    )

    Write-Warning $Message
}

function Prompt-YesNo {
    param(
        [string]$Prompt
    )

    if ($NonInteractive) {
        return $false
    }

    if ([Console]::IsInputRedirected -or [Console]::IsOutputRedirected) {
        return $false
    }

    $choice = Read-Host "$Prompt [y/N]"
    return $choice -match "^(?i:y(?:es)?)$"
}

function Normalize-Version {
    param(
        [string]$RawVersion
    )

    if ([string]::IsNullOrWhiteSpace($RawVersion) -or $RawVersion -eq "latest") {
        return "latest"
    }

    if ($RawVersion.StartsWith("cld-gateway-v")) {
        return $RawVersion.Substring(13)
    }

    if ($RawVersion.StartsWith("v")) {
        return $RawVersion.Substring(1)
    }

    return $RawVersion
}

function Assert-ValidReleaseVersion {
    param(
        [string]$Version
    )

    if ($Version -cne "latest" -and $Version -cnotmatch "^[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta)(?:\.[0-9]+)?)?$") {
        throw "Invalid cld-gateway release version: $Version. Expected latest or x.y.z[-alpha[.N]|-beta[.N]]."
    }
}

function Resolve-Version {
    $normalizedVersion = Normalize-Version -RawVersion $Release
    Assert-ValidReleaseVersion -Version $normalizedVersion
    if ($normalizedVersion -ne "latest") {
        return $normalizedVersion
    }

    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/bytonomics/gateway/releases/latest"
    if (-not $release.tag_name) {
        Write-Error "Failed to resolve the latest cld-gateway release version."
        exit 1
    }

    $resolvedVersion = Normalize-Version -RawVersion $release.tag_name
    Assert-ValidReleaseVersion -Version $resolvedVersion
    return $resolvedVersion
}

function Get-ReleaseAssetUrl {
    param(
        [string]$AssetName,
        [string]$ResolvedVersion
    )

    return "https://github.com/bytonomics/gateway/releases/download/cld-gateway-v$ResolvedVersion/$AssetName"
}

function Get-PackageArchiveDigest {
    param(
        [string]$ManifestPath,
        [string]$AssetName
    )

    $escapedAssetName = [regex]::Escape($AssetName)
    foreach ($line in Get-Content -LiteralPath $ManifestPath) {
        $match = [regex]::Match($line, "^\s*([0-9a-fA-F]{64})\s+$escapedAssetName\s*$")
        if ($match.Success) {
            return $match.Groups[1].Value.ToLowerInvariant()
        }
    }

    throw "Could not find SHA-256 digest for $AssetName in cld-gateway-package_SHA256SUMS."
}

function Test-ArchiveDigest {
    param(
        [string]$ArchivePath,
        [string]$ExpectedDigest
    )

    $actualDigest = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualDigest -ne $ExpectedDigest) {
        throw "Downloaded cld-gateway archive checksum did not match expected digest. Expected $ExpectedDigest but got $actualDigest."
    }
}

function Path-Contains {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return $false
    }

    $needle = $Entry.TrimEnd("\")
    foreach ($segment in $PathValue.Split(";", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        if ($segment.TrimEnd("\") -ieq $needle) {
            return $true
        }
    }

    return $false
}

# ── main ────────────────────────────────────────────────────────────────────
# PLATFORM VALIDATION: Windows support is currently unsupported.

if ($env:OS -ne "Windows_NT") {
    Write-Error "install.ps1 is designed for Windows only. Use install.sh on macOS or Linux."
    exit 1
}

# ── WINDOWS NOT CURRENTLY SUPPORTED ──────────────────────────────────────────
# cld-gateway binaries for Windows are not yet available. This script is a
# placeholder for future Windows support. This message will be removed when
# Windows binary support is released.
#
# WORKAROUNDS FOR WINDOWS USERS:
# 1. Use WSL2 (Windows Subsystem for Linux) and run install.sh from there
# 2. Build from source: clone the repo and run 'cargo build --release -p gatewayd'
#
# Check https://github.com/bytonomics/gateway/releases for future Windows binary
# availability.
# This placeholder is intentionally not part of the v1 release asset set.
# This placeholder is intentionally not part of the v1 release asset set.

Write-Host "╔═══════════════════════════════════════════════════════════════════════════╗"
Write-Host "║                                                                           ║"
Write-Host "║  cld-gateway for Windows is CURRENTLY UNSUPPORTED                         ║"
Write-Host "║                                                                           ║"
Write-Host "║  Windows binaries will be available in a future release.                  ║"
Write-Host "║                                                                           ║"
Write-Host "║  RECOMMENDED WORKAROUNDS:                                                 ║"
Write-Host "║  1. Use WSL2: Install WSL2, then run:                                     ║"
Write-Host "║     bash -c 'curl -fsSL github.com/bytonomics/gateway/releases/latest"
Write-Host "║     /download/install.sh | sh'"
Write-Host "║                                                                           ║"
Write-Host "║  2. Build from source: https://github.com/bytonomics/gateway              ║"
Write-Host "║     git clone <repo> && cd gateway                                        ║"
Write-Host "║     cargo build --release -p gatewayd                                     ║"
Write-Host "║                                                                           ║"
Write-Host "║  For updates, watch: https://github.com/bytonomics/gateway/releases       ║"
Write-Host "║                                                                           ║"
Write-Host "╚═══════════════════════════════════════════════════════════════════════════╝"
exit 1

# ──────────────────────────────────────────────────────────────────────────────
# FUTURE IMPLEMENTATION SECTION (for eventual Windows support)
#
# When Windows binary support is released, remove the exit 1 above and uncomment
# the following code sections to enable actual installation functionality.
#
# ──────────────────────────────────────────────────────────────────────────────
#
# # [FUTURE] Platform detection (uncomment when Windows binaries are released)
# #
# # if (-not [Environment]::Is64BitOperatingSystem) {
# #     Write-Error "cld-gateway requires a 64-bit version of Windows."
# #     exit 1
# # }
# #
# # $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
# # $target = $null
# # $platformLabel = $null
# # switch ($architecture) {
# #     "X64" {
# #         $target = "x86_64-pc-windows-msvc"
# #         $platformLabel = "Windows (x64)"
# #     }
# #     "Arm64" {
# #         $target = "aarch64-pc-windows-msvc"
# #         $platformLabel = "Windows (ARM64)"
# #     }
# #     default {
# #         Write-Error "Unsupported architecture: $architecture."
# #         exit 1
# #     }
# # }
# #
# # $resolvedVersion = Resolve-Version
# # $packageAsset = "cld-gateway-package-$target.tar.gz"
# # $checksumAsset = "cld-gateway-package_SHA256SUMS"
# # $packageUrl = Get-ReleaseAssetUrl -AssetName $packageAsset -ResolvedVersion $resolvedVersion
# # $checksumUrl = Get-ReleaseAssetUrl -AssetName $checksumAsset -ResolvedVersion $resolvedVersion
# #
# # Write-Step "Installing cld-gateway"
# # Write-Step "Detected platform: $platformLabel"
# # Write-Step "Resolved version: $resolvedVersion"
# #
# # [FUTURE] Download and install (uncomment when Windows binaries are released)
# #
# # $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cld-gateway-install-" + [System.Guid]::NewGuid().ToString("N"))
# # New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
# #
# # try {
# #     $archivePath = Join-Path $tempDir $packageAsset
# #     $checksumPath = Join-Path $tempDir $checksumAsset
# #
# #     Write-Step "Downloading cld-gateway"
# #     Invoke-WebRequest -Uri $checksumUrl -OutFile $checksumPath
# #     $expectedPackageDigest = Get-PackageArchiveDigest -ManifestPath $checksumPath -AssetName $packageAsset
# #     Invoke-WebRequest -Uri $packageUrl -OutFile $archivePath
# #     Test-ArchiveDigest -ArchivePath $archivePath -ExpectedDigest $expectedPackageDigest
# #
# #     Write-Step "Installing cld-gateway to $InstallDir"
# #     New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
# #
# #     $extractDir = Join-Path $tempDir "extract"
# #     New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
# #     tar -xzf $archivePath -C $extractDir "bin/cld-gateway.exe"
# #
# #     $extractedBin = Join-Path $extractDir "bin\cld-gateway.exe"
# #     Copy-Item -LiteralPath $extractedBin -Destination $BinPath -Force
# # } finally {
# #     Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue
# # }
# #
# # [FUTURE] PATH configuration (uncomment when Windows binaries are released)
# #
# # $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
# # if (-not (Path-Contains -PathValue $userPath -Entry $InstallDir)) {
# #     if ([string]::IsNullOrWhiteSpace($userPath)) {
# #         $newUserPath = $InstallDir
# #     } else {
# #         $newUserPath = "$InstallDir;$userPath"
# #     }
# #     [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
# #     Write-Step "PATH updated for future PowerShell sessions."
# # }
# #
# # if (-not (Path-Contains -PathValue $env:Path -Entry $InstallDir)) {
# #     $env:Path = "$InstallDir;$env:Path"
# # }
# #
# # Write-Host "cld-gateway $resolvedVersion installed successfully to $BinPath."
# # Write-Step "Installation complete. Open a new PowerShell window to use cld-gateway."
# #
# # if (Prompt-YesNo "Start cld-gateway now?") {
# #     Write-Step "Launching cld-gateway"
# #     & $BinPath
# # }
