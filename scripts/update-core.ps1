[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Rev
)

$ErrorActionPreference = 'Stop'

if ($Rev -notmatch '^[0-9a-fA-F]{40}$') {
    throw 'Rev must be a 40-character hexadecimal Git SHA.'
}

$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
$lock = Join-Path $root 'Cargo.lock'

$diffOutput = git -C $root diff --quiet -- Cargo.toml Cargo.lock
$diffExit = $LASTEXITCODE
$untracked = git -C $root ls-files --others --exclude-standard -- Cargo.toml Cargo.lock
if ($diffExit -ne 0 -or $untracked) {
    throw 'Cargo.toml or Cargo.lock is dirty; commit or preserve those changes before updating core.'
}

$manifestBackup = [IO.Path]::GetTempFileName()
$lockBackup = [IO.Path]::GetTempFileName()
Copy-Item $manifest $manifestBackup -Force
Copy-Item $lock $lockBackup -Force

try {
    $text = [IO.File]::ReadAllText($manifest)
    $pattern = '(?m)(protium-core\s*=\s*\{\s*git\s*=\s*"[^"]+"\s*,\s*)(?:branch\s*=\s*"main"|rev\s*=\s*"[0-9a-fA-F]{40}")'
    $updated = [Text.RegularExpressions.Regex]::Replace($text, $pattern, "`$1rev = `"$Rev`"")
    if ($updated -eq $text) {
        throw 'Could not find the protium-core Git dependency declaration.'
    }
    [IO.File]::WriteAllText($manifest, $updated, [Text.UTF8Encoding]::new($false))

    cargo update -p protium-core --precise $Rev
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo update failed.'
    }

    $metadata = cargo metadata --locked --format-version 1 | ConvertFrom-Json
    $core = $metadata.packages | Where-Object { $_.name -eq 'protium-core' } | Select-Object -First 1
    if ($null -eq $core -or $null -eq $core.source -or $core.source -match 'path\+file' -or $core.source -notmatch "#$Rev`$") {
        throw "protium-core metadata source is not the requested Git revision: $($core.source)"
    }
} catch {
    Copy-Item $manifestBackup $manifest -Force
    Copy-Item $lockBackup $lock -Force
    throw
} finally {
    Remove-Item $manifestBackup, $lockBackup -Force -ErrorAction SilentlyContinue
}

Write-Host "Updated protium-core to $Rev. Review the diff; this script does not commit or push."
