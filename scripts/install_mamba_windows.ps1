[CmdletBinding()]
param(
    [string]$RustToolchain,
    [switch]$SkipGit,
    [switch]$SkipBuildTools,
    [switch]$SkipSourceSync,
    [switch]$SkipBuild,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$onWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT

if ($onWindows) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Invoke-External {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Command
    )

    $rendered = ($Command | ForEach-Object {
            if ($_ -match '\s') {
                '"' + $_.Replace('"', '\"') + '"'
            } else {
                $_
            }
        }) -join ' '
    Write-Host "+ $rendered"

    if ($DryRun) {
        return
    }

    if ($Command.Length -eq 1) {
        & $Command[0]
    } else {
        & $Command[0] @($Command[1..($Command.Length - 1)])
    }

    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE: $rendered"
    }
}

function Refresh-ProcessPath {
    $machinePath = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $segments = @()
    if ($machinePath) {
        $segments += $machinePath
    }
    if ($userPath) {
        $segments += $userPath
    }
    $env:Path = ($segments -join ";")
}

function Get-RepoRoot {
    return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

function Get-RequestedToolchain {
    param([string]$RepoRoot, [string]$Override)

    if ($Override) {
        return $Override
    }

    $toolchainFile = Join-Path $RepoRoot "rust-toolchain.toml"
    if (-not (Test-Path $toolchainFile)) {
        throw "missing rust-toolchain.toml at $toolchainFile"
    }

    $match = Select-String -Path $toolchainFile -Pattern '^\s*channel\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $match) {
        throw "failed to read Rust toolchain channel from $toolchainFile"
    }

    return $match.Matches[0].Groups[1].Value
}

function Test-Command {
    param([string]$Name)
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Install-WithWinget {
    param(
        [string]$PackageId,
        [string[]]$ExtraArgs = @()
    )

    $args = @(
        "install",
        "--exact",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--id",
        $PackageId
    ) + $ExtraArgs
    Invoke-External (@("winget") + $args)
}

function Install-WithChocolatey {
    param(
        [string]$PackageName,
        [string[]]$ExtraArgs = @()
    )

    $args = @("install", "-y", $PackageName) + $ExtraArgs
    Invoke-External (@("choco") + $args)
}

function Invoke-DownloadInstall {
    param(
        [string]$Uri,
        [string]$OutFile,
        [string[]]$ArgumentList
    )

    Write-Host "+ download $Uri -> $OutFile"
    if ($DryRun) {
        return
    }

    Invoke-WebRequest -Uri $Uri -OutFile $OutFile
    $process = Start-Process -FilePath $OutFile -ArgumentList $ArgumentList -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "installer failed with exit code $($process.ExitCode): $OutFile"
    }
}

function Ensure-Git {
    if ((Test-Command "git.exe") -or (Test-Command "git")) {
        return
    }

    if (Test-Command "winget") {
        Install-WithWinget -PackageId "Git.Git"
        Refresh-ProcessPath
        return
    }

    if (Test-Command "choco") {
        Install-WithChocolatey -PackageName "git"
        Refresh-ProcessPath
        return
    }

    throw "git is required but neither winget nor choco is available to install it"
}

function Get-VswherePath {
    $programFilesX86 = ${env:ProgramFiles(x86)}
    if (-not $programFilesX86) {
        return $null
    }

    $candidate = Join-Path $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $candidate) {
        return $candidate
    }

    return $null
}

function Get-VsInstallationPath {
    $vswhere = Get-VswherePath
    if (-not $vswhere) {
        return $null
    }

    $result = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0) {
        throw "vswhere failed while looking for Visual Studio Build Tools"
    }

    if (-not $result) {
        return $null
    }

    return ($result | Select-Object -First 1).Trim()
}

function Ensure-VsBuildTools {
    if (Get-VsInstallationPath) {
        return
    }

    $override = "--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --add Microsoft.VisualStudio.Component.Windows11SDK.22621"

    if (Test-Command "winget") {
        Install-WithWinget -PackageId "Microsoft.VisualStudio.2022.BuildTools" -ExtraArgs @("--override", $override)
        Refresh-ProcessPath
        return
    }

    if (Test-Command "choco") {
        Install-WithChocolatey -PackageName "visualstudio2022buildtools" -ExtraArgs @("--package-parameters", $override)
        Refresh-ProcessPath
        return
    }

    $tempExe = Join-Path $env:TEMP "vs_BuildTools.exe"
    Invoke-DownloadInstall -Uri "https://aka.ms/vs/17/release/vs_BuildTools.exe" -OutFile $tempExe -ArgumentList @(
        "--wait",
        "--passive",
        "--norestart",
        "--add",
        "Microsoft.VisualStudio.Workload.VCTools",
        "--includeRecommended",
        "--add",
        "Microsoft.VisualStudio.Component.Windows11SDK.22621"
    )
    Refresh-ProcessPath
}

function Ensure-Rustup {
    param([string]$Toolchain)

    if ((Test-Command "rustup.exe") -or (Test-Command "rustup")) {
        return
    }

    if (Test-Command "winget") {
        Install-WithWinget -PackageId "Rustlang.Rustup"
        Refresh-ProcessPath
        return
    }

    if (Test-Command "choco") {
        Install-WithChocolatey -PackageName "rustup.install"
        Refresh-ProcessPath
        return
    }

    $tempExe = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-DownloadInstall -Uri "https://win.rustup.rs/x86_64" -OutFile $tempExe -ArgumentList @(
        "-y",
        "--profile",
        "minimal",
        "--default-toolchain",
        $Toolchain
    )
    Refresh-ProcessPath
}

function Import-VsDevEnvironment {
    $installationPath = Get-VsInstallationPath
    if (-not $installationPath) {
        throw "Visual Studio Build Tools with the C++ workload are not installed"
    }

    $vsDevCmd = Join-Path $installationPath "Common7\Tools\VsDevCmd.bat"
    if (-not (Test-Path $vsDevCmd)) {
        throw "VsDevCmd.bat not found at $vsDevCmd"
    }

    if ($DryRun) {
        Write-Host "+ cmd /c `"$vsDevCmd`" -no_logo && set"
        return
    }

    cmd /c "`"$vsDevCmd`" -no_logo && set" | ForEach-Object {
        if ($_ -match "^(.*?)=(.*)$") {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
        }
    }

    if ($LASTEXITCODE -ne 0) {
        throw "failed to import the Visual Studio developer environment"
    }
}

function Get-GitBashPath {
    $candidates = @()
    if ($env:ProgramFiles) {
        $candidates += (Join-Path $env:ProgramFiles "Git\bin\bash.exe")
    }
    if (${env:ProgramFiles(x86)}) {
        $candidates += (Join-Path ${env:ProgramFiles(x86)} "Git\bin\bash.exe")
    }

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) {
            return $candidate
        }
    }

    throw "Git Bash was not found. Install Git first."
}

$repoRoot = Get-RepoRoot
$toolchain = Get-RequestedToolchain -RepoRoot $repoRoot -Override $RustToolchain

if (-not $onWindows) {
    if ($DryRun) {
        Write-Warning "Non-Windows host detected; dry-run only."
    } else {
        throw "this script must be run on Windows PowerShell or PowerShell 7 on Windows"
    }
}

Write-Step "Repo root: $repoRoot"
Write-Step "Rust toolchain: $toolchain"

if (-not $SkipGit) {
    Write-Step "Ensuring Git is installed"
    Ensure-Git
}

if (-not $SkipBuildTools) {
    Write-Step "Ensuring Visual Studio Build Tools (C++) are installed"
    Ensure-VsBuildTools
}

Write-Step "Ensuring rustup is installed"
Ensure-Rustup -Toolchain $toolchain

Refresh-ProcessPath
$cargoHomeRoot = $null
if ($env:USERPROFILE) {
    $cargoHomeRoot = $env:USERPROFILE
} elseif ($HOME) {
    $cargoHomeRoot = $HOME
}
$cargoBin = $null
if ($cargoHomeRoot) {
    $cargoBin = Join-Path $cargoHomeRoot ".cargo\bin"
}
if ($cargoBin -and (Test-Path $cargoBin)) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Test-Command "rustup.exe") -and -not (Test-Command "rustup")) {
    throw "rustup is not available after installation"
}

Import-VsDevEnvironment

$windowsToolchain = "$toolchain-x86_64-pc-windows-msvc"
Write-Step "Installing Rust toolchain $windowsToolchain"
Invoke-External @("rustup", "toolchain", "install", $windowsToolchain, "--profile", "minimal")
Invoke-External @("rustup", "default", $windowsToolchain)

if (-not $SkipSourceSync) {
    Write-Step "Refreshing upstream mirrors"
    $bashExe = Get-GitBashPath
    $syncCommand = "cd '$repoRoot' && scripts/sync_sources.sh"
    Invoke-External @($bashExe, "-lc", $syncCommand)
}

if (-not $SkipBuild) {
    Write-Step "Running locked mamba, mamba_api, and mamba_mcp builds"
    Push-Location $repoRoot
    try {
        Invoke-External @("cargo", "build", "--locked", "--bin", "mamba", "--bin", "mamba_api", "--bin", "mamba_mcp")
    }
    finally {
        Pop-Location
    }
}

Write-Host ""
Write-Host "Windows host setup complete."
Write-Host "Rust toolchain: $windowsToolchain"
if ($SkipBuild) {
    Write-Host "Build validation skipped."
} else {
    Write-Host "Validated with: cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp"
}
