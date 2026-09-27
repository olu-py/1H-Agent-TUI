[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Branch,

    [string] $ExpectedSha = '',

    [switch] $AllowMain,

    [switch] $ForceWithLease,

    [string] $Remote = 'origin',

    [switch] $DryRun
)

$ErrorActionPreference = 'Stop'

function Invoke-Git {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]] $Args)
    $output = & git @Args 2>&1
    return [pscustomobject]@{ Output = $output; Code = $LASTEXITCODE }
}

function Assert-Git {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]] $Args)
    $r = Invoke-Git @Args
    if ($r.Code -ne 0) {
        throw "git $($Args -join ' ') failed ($($r.Code)): $($r.Output -join '; ')"
    }
    return ($r.Output | ForEach-Object { $_.ToString() })
}

function Get-Sha([string] $Ref) {
    $r = Invoke-Git rev-parse --verify $Ref
    if ($r.Code -ne 0) { return $null }
    return ($r.Output | Select-Object -First 1).ToString().Trim()
}

$root = (& git rev-parse --show-toplevel 2>&1)
if ($LASTEXITCODE -ne 0) { throw 'not inside a git repository' }
Set-Location $root

if ($ExpectedSha -and $ExpectedSha -notmatch '^[0-9a-fA-F]{40}$') {
    throw 'ExpectedSha must be a 40-character hexadecimal commit SHA.'
}
if ($ForceWithLease -and -not $ExpectedSha) {
    throw 'ForceWithLease requires ExpectedSha (used as the lease expectation).'
}
if ($ForceWithLease -and $Branch -eq 'main') {
    throw 'ForceWithLease is never allowed for main.'
}

Write-Host "[push] repository : $root"
Write-Host "[push] remote     : $Remote"
Write-Host "[push] branch     : $Branch"

Assert-Git fetch $Remote --prune | Out-Null

$localSha = Get-Sha "refs/heads/$Branch"
if (-not $localSha) { throw "local branch does not exist: $Branch" }
Write-Host "[push] local sha  : $localSha"

if ($ExpectedSha -and $localSha -ne $ExpectedSha.ToLower()) {
    throw "local $Branch is $localSha but expected $($ExpectedSha.ToLower()). Refusing to push."
}

if ($Branch -eq 'main') {
    if (-not $AllowMain) {
        throw 'refusing to push main without -AllowMain (documentation-only rule).'
    }
    $remoteMain = Get-Sha "refs/remotes/$Remote/main"
    if ($remoteMain -and $remoteMain -ne $localSha) {
        $ff = Invoke-Git merge-base --is-ancestor $remoteMain $localSha
        if ($ff.Code -ne 0) {
            throw "main is not a fast-forward: local=$localSha remote=$remoteMain. Fetch and reconcile first."
        }
    }
}

if ($DryRun) {
    Write-Host '[push] dry run: no push performed.'
    $base = Get-Sha "refs/remotes/$Remote/$Branch"
    if ($base -and $base -ne $localSha) {
        Write-Host "[push] would update $Branch from $base to $localSha"
        Assert-Git diff --name-only "$base..$localSha" | ForEach-Object { Write-Host "  $_" }
    } else {
        Write-Host "[push] would publish $Branch at $localSha"
    }
    exit 0
}

$refspec = "refs/heads/$Branch`:refs/heads/$Branch"
$pushArgs = @('push', $Remote, $refspec)
if ($ForceWithLease) {
    $pushArgs = @('push', "--force-with-lease=refs/heads/$Branch`:$($ExpectedSha.ToLower())", $Remote, $refspec)
}

Write-Host "[push] pushing    : $refspec"
$pushResult = Invoke-Git @pushArgs
$pushResult.Output | ForEach-Object { Write-Host $_ }
if ($pushResult.Code -ne 0) { exit $pushResult.Code }

$ls = Assert-Git ls-remote --heads $Remote "refs/heads/$Branch"
$remoteSha = ($ls | Select-Object -First 1).Split("`t")[0].Trim()
Write-Host "[push] local sha  : $localSha"
Write-Host "[push] remote sha : $remoteSha"
if ($remoteSha -ne $localSha) {
    throw "remote $Branch ($remoteSha) does not match local ($localSha) after push."
}
Write-Host '[push] verified remote branch matches the local commit.'