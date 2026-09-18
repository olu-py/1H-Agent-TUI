#Requires -Version 5.1
<#
.SYNOPSIS
    构建并启动 1H-Agent TUI，用于本地功能测试。

.DESCRIPTION
    流程：
      1. 解析 cargo、target 目录与测试用数据目录；
      2. 可选 -LocalCore：通过 cargo --config 把 protium-core 临时指向本地 core 源码；
      3. cargo build（-NoBuild 可跳过）；
      4. 启动 TUI。

    脚本不修改仓库中受跟踪的 Cargo.toml 或 .cargo/config.toml。
    使用 -LocalCore 时若 Cargo.lock 原本干净，构建结束后会自动还原。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\run-tui.ps1

.EXAMPLE
    .\scripts\run-tui.ps1 -Workspace D:\workbase\test

.EXAMPLE
    .\scripts\run-tui.ps1 -LocalCore -EnvFile .env

.EXAMPLE
    .\scripts\run-tui.ps1 -Release -NoBuild

.EXAMPLE
    .\scripts\run-tui.ps1 -DryRun
#>
[CmdletBinding()]
param(
    # Agent 可访问的工作区目录，默认当前目录。
    [string]$Workspace = (Get-Location).Path,

    # TUI 配置 TOML；不指定时使用 core 默认路径（Windows: %APPDATA%\1h-agent\config.toml）。
    [string]$Config,

    # 会话数据库目录；默认仓库内 .runtime-data（已被 .gitignore 忽略）。
    [string]$DataDir,

    # 改用 OS 默认数据目录，而不是仓库内 .runtime-data。
    [switch]$UseDefaultData,

    # 构建产物目录；默认 $env:CARGO_TARGET_DIR，否则 <repo>\target。
    [string]$TargetDir,

    # 构建 release 版本。
    [switch]$Release,

    # 把 protium-core 临时 patch 到本地 core 源码，用于 core/TUI 联调。
    [switch]$LocalCore,

    # -LocalCore 使用的 core 源码路径；默认同级目录 1H-Agent-core。
    [string]$CorePath,

    # 允许 cargo 联网（默认离线，只使用本地缓存）。
    [switch]$Online,

    # 跳过 cargo build，直接运行已有二进制。
    [switch]$NoBuild,

    # 只构建，不启动 TUI（用于先验证编译是否通过）。
    [switch]$BuildOnly,

    # 从 .env 风格文件加载 KEY=VALUE 环境变量（用于注入 Provider API Key）。
    [string]$EnvFile,

    # 只打印将要执行的命令，不构建也不启动。
    [switch]$DryRun,

    # 透传给 TUI 二进制的额外参数。
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ExtraArgs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $repoRoot 'Cargo.toml'
$lockPath = Join-Path $repoRoot 'Cargo.lock'
$isWindowsHost = ((Test-Path Env:OS) -and ($env:OS -eq 'Windows_NT'))

function Write-Step([string]$Message) {
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Get-DataDirDisplay {
    if (Test-Path Env:AGENT_DATA_DIR) { return $env:AGENT_DATA_DIR }
    return '\u9ed8\u8ba4\uff08%LOCALAPPDATA%\\1h-agent\uff09'
}

function Resolve-Cargo {
    $candidates = @()
    if (Test-Path Env:CARGO_HOME) { $candidates += (Join-Path $env:CARGO_HOME 'bin\cargo.exe') }
    $candidates += 'cargo'
    if ($env:USERPROFILE) { $candidates += (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe') }
    foreach ($candidate in $candidates) {
        $command = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($command) { return $command.Source }
    }
    throw '找不到 cargo。请安装 Rust 工具链，或设置 CARGO_HOME 后重试。'
}

function Get-Sha256([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($Path)
        try {
            $bytes = $sha.ComputeHash($stream)
        } finally {
            $stream.Dispose()
        }
    } finally {
        $sha.Dispose()
    }
    return ([System.BitConverter]::ToString($bytes)).Replace('-', '')
}

function Get-PinnedCoreCommit([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $text = Get-Content -LiteralPath $Path -Raw
    $match = [regex]::Match($text, 'name = "protium-core"[\s\S]{0,240}?source = "git\+[^#]*#([0-9a-f]{7,40})"')
    if ($match.Success) { return $match.Groups[1].Value.Substring(0, 8) }
    return $null
}

# --- 工作区 ---------------------------------------------------------------
try {
    $workspacePath = (Resolve-Path -LiteralPath $Workspace -ErrorAction Stop).Path
} catch {
    throw "工作区不存在: $Workspace"
}

# --- 环境变量文件 ---------------------------------------------------------
if ($EnvFile) {
    if (-not (Test-Path -LiteralPath $EnvFile)) { throw "找不到 env 文件: $EnvFile" }
    $loadedNames = @()
    foreach ($line in (Get-Content -LiteralPath $EnvFile)) {
        $trimmed = $line.Trim()
        if (-not $trimmed -or $trimmed.StartsWith('#')) { continue }
        $separator = $trimmed.IndexOf('=')
        if ($separator -lt 1) { continue }
        $name = $trimmed.Substring(0, $separator).Trim()
        $value = $trimmed.Substring($separator + 1).Trim().Trim('"').Trim("'")
        Set-Item -Path ("Env:" + $name) -Value $value
        $loadedNames += $name
    }
    if ($loadedNames.Count -gt 0) {
        Write-Step ("已加载环境变量: " + ($loadedNames -join ', '))
    }
}

# --- 数据目录 -------------------------------------------------------------
if (-not $UseDefaultData) {
    if (-not $DataDir) { $DataDir = Join-Path $repoRoot '.runtime-data' }
    if (-not $DryRun) {
        try {
            New-Item -ItemType Directory -Force -Path $DataDir -ErrorAction Stop | Out-Null
        } catch {
            $fallback = Join-Path ([System.IO.Path]::GetTempPath()) '1h-agent-tui-data'
            Write-Warning "无法创建数据目录 $DataDir，改用 $fallback"
            New-Item -ItemType Directory -Force -Path $fallback -ErrorAction Stop | Out-Null
            $DataDir = $fallback
        }
        $env:AGENT_DATA_DIR = (Resolve-Path -LiteralPath $DataDir).Path
    } else {
        $env:AGENT_DATA_DIR = [System.IO.Path]::GetFullPath($DataDir)
    }
}

# --- 构建目录与二进制 -----------------------------------------------------
if (-not $TargetDir) {
    $TargetDir = if (Test-Path Env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repoRoot 'target' }
}
$profileName = if ($Release) { 'release' } else { 'debug' }
$binaryName = if ($isWindowsHost) { '1h-agent.exe' } else { '1h-agent' }
$binaryPath = Join-Path (Join-Path $TargetDir $profileName) $binaryName

$cargoExe = Resolve-Cargo
$env:CARGO_TARGET_DIR = $TargetDir
$cargoArgs = @()
$patchFile = $null
$lockBackup = $null
$coreFull = $null

if ($LocalCore -and $NoBuild) {
    Write-Warning '-LocalCore 与 -NoBuild 同时使用无意义，已忽略 -LocalCore。'
    $LocalCore = $false
}

if ($BuildOnly -and $NoBuild) {
    Write-Warning '-BuildOnly 与 -NoBuild 同时使用无意义，已忽略 -NoBuild。'
    $NoBuild = $false
}

if ($LocalCore) {
    if (-not $CorePath) { $CorePath = Join-Path (Split-Path -Parent $repoRoot) '1H-Agent-core' }
    if (-not (Test-Path -LiteralPath (Join-Path $CorePath 'Cargo.toml'))) {
        throw "找不到本地 core 源码: $CorePath（可用 -CorePath 指定）"
    }
    $coreFull = (Resolve-Path -LiteralPath $CorePath).Path.Replace('\', '/')
    $patchFile = Join-Path ([System.IO.Path]::GetTempPath()) ("1h-agent-core-patch-{0}.toml" -f $PID)
    $patchContent = "[patch." + [char]34 + "https://github.com/olu-py/1H-Agent-core.git" + [char]34 + "]`nprotium-core = { path = " + [char]34 + $coreFull + [char]34 + " }`n"
    [System.IO.File]::WriteAllText($patchFile, $patchContent, (New-Object System.Text.UTF8Encoding($false)))
    $cargoArgs += @('--config', $patchFile)
    Write-Warning '-LocalCore 会临时改写 Cargo.lock；脚本会在构建后尝试还原。'
    $gitCommand = Get-Command git -ErrorAction SilentlyContinue
    if ($gitCommand) {
        $lockDirty = & $gitCommand.Source -C $repoRoot status --porcelain -- Cargo.lock
        if ($lockDirty) {
            Write-Host '    检测到 Cargo.lock 已有改动：构建后会连同这些改动一并还原。' -ForegroundColor DarkGray
        }
    }
    # 无论是否干净都先备份：Cargo 的 path patch 会改写锁文件，
    # 还原备份才能保住联调前的原始内容（含用户已有改动）。
    $lockBackup = Join-Path ([System.IO.Path]::GetTempPath()) ("1h-agent-Cargo.lock-{0}.bak" -f $PID)
    Copy-Item -LiteralPath $lockPath -Destination $lockBackup -Force
}

$cargoArgs += @('build', '--manifest-path', $manifestPath, '--bin', '1h-agent', '--target-dir', $TargetDir)
if (-not $LocalCore) { $cargoArgs += '--locked' }
if ($Release) { $cargoArgs += '--release' }
if (-not $Online) { $cargoArgs += '--offline' }

$appArgs = @('--workspace', $workspacePath)
if ($Config) {
    if (-not (Test-Path -LiteralPath $Config)) { throw "找不到配置文件: $Config" }
    $appArgs += @('--config', (Resolve-Path -LiteralPath $Config).Path)
}
if ($ExtraArgs) { $appArgs += $ExtraArgs }

$coreCommit = Get-PinnedCoreCommit $lockPath

# --- DryRun ---------------------------------------------------------------
if ($DryRun) {
    Write-Step 'DryRun：不会执行构建或启动'
    Write-Host ("cargo  : " + $cargoExe + " " + ($cargoArgs -join ' '))
    Write-Host ("binary : " + $binaryPath)
    Write-Host ("app    : " + $binaryPath + " " + ($appArgs -join ' '))
    Write-Host ("工作区  : " + $workspacePath)
    Write-Host ("数据目录: " + (Get-DataDirDisplay))
    Write-Host ("配置文件: " + ($(if ($Config) { (Resolve-Path -LiteralPath $Config).Path } else { '默认（%APPDATA%\1h-agent\config.toml）' })))
    Write-Host ("core    : " + ($(if ($LocalCore) { "本地源码 $coreFull" } elseif ($coreCommit) { $coreCommit } else { '未知' })))
    Write-Host ("离线    : " + (-not $Online))
    Write-Host ("仅构建  : " + $BuildOnly)
    if ($patchFile) { Write-Host ("patch   : " + $patchFile) }
    # DryRun does not build, so drop the just-written patch file instead of
    # leaving a stray temp file behind.
    if ($patchFile -and (Test-Path -LiteralPath $patchFile)) {
        Remove-Item -LiteralPath $patchFile -Force -ErrorAction SilentlyContinue
    }
    if ($lockBackup -and (Test-Path -LiteralPath $lockBackup)) {
        Remove-Item -LiteralPath $lockBackup -Force -ErrorAction SilentlyContinue
    }
    exit 0
}

# --- 构建 -----------------------------------------------------------------
if (-not $NoBuild) {
    Write-Step "构建 1h-agent（$profileName）"
    try {
        & $cargoExe @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败，退出码 $LASTEXITCODE" }
    } finally {
        if ($lockBackup -and (Test-Path -LiteralPath $lockBackup)) {
            $restored = $false
            try {
                $before = Get-Sha256 $lockBackup
                $after = Get-Sha256 $lockPath
                if ($before -ne $after) {
                    Copy-Item -LiteralPath $lockBackup -Destination $lockPath -Force
                    $restored = $true
                }
            } catch {
                Write-Warning "Cargo.lock 还原失败，备份保留在 $lockBackup（原因: $($_.Exception.Message)）"
            }
            if ($restored) { Write-Step 'Cargo.lock 已还原为联调前状态' }
            if (Test-Path -LiteralPath $lockBackup) { Remove-Item -LiteralPath $lockBackup -Force -ErrorAction SilentlyContinue }
        }
        if ($patchFile -and (Test-Path -LiteralPath $patchFile)) {
            Remove-Item -LiteralPath $patchFile -Force -ErrorAction SilentlyContinue
        }
    }
}

if ($BuildOnly) {
    if (-not (Test-Path -LiteralPath $binaryPath)) {
        throw "构建结束但找不到二进制: $binaryPath"
    }
    Write-Step "构建完成（未启动）: $binaryPath"
    exit 0
}

if (-not (Test-Path -LiteralPath $binaryPath)) {
    throw "找不到二进制: $binaryPath（去掉 -NoBuild 重新构建）"
}

# --- 启动 -----------------------------------------------------------------
Write-Step '启动 1H-Agent TUI'
Write-Host ("    工作区  : " + $workspacePath)
Write-Host ("    数据目录: " + (Get-DataDirDisplay))
Write-Host ("    二进制  : " + $binaryPath)
Write-Host ("    core    : " + ($(if ($LocalCore) { "本地源码 $coreFull" } elseif ($coreCommit) { $coreCommit } else { '未知' })))
Write-Host ''
Write-Host '按 Ctrl+C 退出；首页输入首条消息后 Enter 进入主界面。' -ForegroundColor DarkGray
Write-Host ''

& $binaryPath @appArgs
exit $LASTEXITCODE
