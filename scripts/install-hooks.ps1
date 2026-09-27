[CmdletBinding()]
param(
    [string] $HooksPath = '.githooks'
)

$ErrorActionPreference = 'Stop'

$root = (& git rev-parse --show-toplevel 2>&1)
if ($LASTEXITCODE -ne 0) { throw 'not inside a git repository' }
Set-Location $root

$hookFile = Join-Path $root (Join-Path $HooksPath 'pre-push')
if (-not (Test-Path $hookFile)) {
    throw "pre-push hook not found: $hookFile"
}

& git config core.hooksPath $HooksPath
if ($LASTEXITCODE -ne 0) { throw 'failed to set core.hooksPath' }

$current = (& git config --get core.hooksPath 2>&1)
Write-Host "core.hooksPath = $current"
Write-Host "installed hook = $hookFile"
Write-Host 'note: keep the hook file on LF line endings (see .gitattributes); Git for Windows runs it via bundled bash.'