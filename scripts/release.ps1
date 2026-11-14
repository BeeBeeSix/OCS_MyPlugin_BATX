<#
.SYNOPSIS
    Publishes a new version of Raster Tools.

.DESCRIPTION
    1. checks the version number and that the tag does not exist yet;
    2. updates the version in Cargo.toml and plugin.toml;
    3. in CHANGELOG.md moves the entries under "[Non rilasciato]" into a new
       section "## [version] - date" (it does the same for CHANGELOG_en.md,
       under "[Unreleased]", if that file exists);
    4. runs the tests and builds the plugin;
    5. after your confirmation, creates the commit and the tag and pushes them to GitHub.
    The tag starts the release.yml workflow, which builds for Windows, Linux and
    macOS and publishes the release with the notes taken from CHANGELOG.md.

    If you answer "no" at the confirmation, the files go back as they were.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\release.ps1 0.10.2

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\release.ps1 0.10.2 -SkipBuild
#>
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Version,
    # Skip the local tests and build (GitHub runs them anyway).
    [switch]$SkipBuild,
    # Do not ask for confirmation before pushing to GitHub.
    [switch]$Yes
)

Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$Utf8 = New-Object System.Text.UTF8Encoding($false)

function Info($msg) { Write-Host $msg -ForegroundColor Cyan }
function Ok($msg) { Write-Host $msg -ForegroundColor Green }
function Fail($msg) { Write-Host "ERROR: $msg" -ForegroundColor Red; exit 1 }

function Read-Text($path) { [System.IO.File]::ReadAllText((Join-Path $Root $path), $Utf8) }
function Write-Text($path, $text) { [System.IO.File]::WriteAllText((Join-Path $Root $path), $text, $Utf8) }

# Runs an external program and stops if it fails.
function Run {
    param([string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "'$Exe $($Arguments -join ' ')' failed (exit code $LASTEXITCODE)." }
}

# ── Checks ───────────────────────────────────────────────────────────────────

$Version = $Version.Trim().TrimStart('v', 'V')
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    Fail "invalid version '$Version': use three numbers, for example 0.10.2"
}
$Tag = "v$Version"

foreach ($tool in 'git', 'cargo') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        if ($tool -eq 'cargo' -and $SkipBuild) { continue }
        Fail "the command '$tool' is not available. Close and reopen PowerShell, or install it."
    }
}
foreach ($f in 'Cargo.toml', 'plugin.toml', 'CHANGELOG.md') {
    if (-not (Test-Path (Join-Path $Root $f))) { Fail "the file $f is missing from the project folder." }
}

$branch = (& git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne 'main') { Fail "you are on branch '$branch': switch to 'main' (git checkout main)." }

$prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
& git rev-parse -q --verify "refs/tags/$Tag" 2>&1 | Out-Null
$exists = ($LASTEXITCODE -eq 0)
$ErrorActionPreference = $prev
if ($exists) { Fail "the tag $Tag already exists. Use a new version number." }

Info "Checking GitHub…"
$remoteTag = & git ls-remote --tags origin "refs/tags/$Tag"
if ($LASTEXITCODE -ne 0) { Fail "cannot reach GitHub (git ls-remote). Check your connection." }
if ($remoteTag) { Fail "the tag $Tag already exists on GitHub. Use a new version number." }

& git fetch -q origin main
if ($LASTEXITCODE -eq 0) {
    $behind = (& git rev-list --count HEAD..origin/main).Trim()
    if ([int]$behind -gt 0) {
        Fail "GitHub has $behind change(s) you do not have on this PC. Run first: git pull"
    }
}

$cargo = Read-Text 'Cargo.toml'
$plugin = Read-Text 'plugin.toml'
$changelog = Read-Text 'CHANGELOG.md'
$verRe = '(?m)^(version\s*=\s*")([^"]*)(")'

$m = [regex]::Match($cargo, $verRe)
if (-not $m.Success) { Fail "cannot find 'version = ...' in Cargo.toml." }
$Current = $m.Groups[2].Value
if ([version]$Version -lt [version]$Current) {
    Fail "the new version ($Version) cannot be lower than the current one ($Current)."
}
if ([version]$Version -eq [version]$Current) {
    Info "Cargo.toml and plugin.toml are already at version ${Version}: publishing that one."
}

# ── CHANGELOG ────────────────────────────────────────────────────────────────

$nl = "`n"
if ($changelog.Contains("`r`n")) { $nl = "`r`n" }
$date = Get-Date -Format 'yyyy-MM-dd'

if ($changelog -match ('(?m)^## \[' + [regex]::Escape($Version) + '\]')) {
    Info "CHANGELOG.md already has section ${Version}: using it as it is."
    $newChangelog = $changelog
}
else {
    $head = [regex]::Match($changelog, '(?m)^## \[Non rilasciato\][ \t]*(?=\r?$)')
    if (-not $head.Success) {
        Fail "CHANGELOG.md has no '## [Non rilasciato]' heading."
    }
    $after = $changelog.Substring($head.Index + $head.Length)
    $next = [regex]::Match($after, '(?m)^## \[')
    $pending = $after
    if ($next.Success) { $pending = $after.Substring(0, $next.Index) }
    if ($pending.Trim().Length -eq 0) {
        Fail ("The [Non rilasciato] section of CHANGELOG.md is empty.$nl" +
              "Write this version's news there (e.g. '### Aggiunto' and a '- ...' list) and run again.")
    }
    $insertAt = $head.Index + $head.Length
    $newChangelog = $changelog.Substring(0, $insertAt) + $nl + $nl + "## [$Version] - $date" + $changelog.Substring($insertAt)
}

# CHANGELOG_en.md (English copy): same move, from "[Unreleased]". Never blocks the release.
$enPath = Join-Path $Root 'CHANGELOG_en.md'
$changelogEn = $null
$newChangelogEn = $null
if (Test-Path $enPath) {
    $changelogEn = Read-Text 'CHANGELOG_en.md'
    $newChangelogEn = $changelogEn
    $nlEn = "`n"
    if ($changelogEn.Contains("`r`n")) { $nlEn = "`r`n" }
    if ($changelogEn -match ('(?m)^## \[' + [regex]::Escape($Version) + '\]')) {
        Info "CHANGELOG_en.md already has section ${Version}."
    }
    else {
        $headEn = [regex]::Match($changelogEn, '(?m)^## \[Unreleased\][ \t]*(?=\r?$)')
        if (-not $headEn.Success) {
            Write-Host "WARNING: CHANGELOG_en.md has no '## [Unreleased]' heading: the file is not updated." -ForegroundColor Yellow
        }
        else {
            $insertEn = $headEn.Index + $headEn.Length
            $newChangelogEn = $changelogEn.Substring(0, $insertEn) + $nlEn + $nlEn + "## [$Version] - $date" + $changelogEn.Substring($insertEn)
        }
    }
}

# ── Updating the files ───────────────────────────────────────────────────────

$originals = @{
    'Cargo.toml'   = $cargo
    'plugin.toml'  = $plugin
    'CHANGELOG.md' = $changelog
}
if ($null -ne $changelogEn) { $originals['CHANGELOG_en.md'] = $changelogEn }
$lockPath = Join-Path $Root 'Cargo.lock'
$lockOriginal = $null
if (Test-Path $lockPath) { $lockOriginal = Read-Text 'Cargo.lock' }

function Restore-Files {
    foreach ($k in $originals.Keys) { Write-Text $k $originals[$k] }
    if ($null -ne $lockOriginal) { Write-Text 'Cargo.lock' $lockOriginal }
    Info "Files restored as they were: nothing was pushed."
}

$rx = New-Object System.Text.RegularExpressions.Regex($verRe)
Write-Text 'Cargo.toml' ($rx.Replace($cargo, ('${1}' + $Version + '${3}'), 1))
Write-Text 'plugin.toml' ($rx.Replace($plugin, ('${1}' + $Version + '${3}'), 1))
Write-Text 'CHANGELOG.md' $newChangelog
if ($null -ne $newChangelogEn) { Write-Text 'CHANGELOG_en.md' $newChangelogEn }
Ok "Version updated: $Current -> $Version (Cargo.toml, plugin.toml, changelogs)."

try {
    if (-not $SkipBuild) {
        Info "Tests…"
        Run 'cargo' @('test', '--release')
        Info "Building…"
        Run 'cargo' @('build', '--release')
        Ok "Tests and build succeeded."
    }
}
catch {
    Write-Host $_ -ForegroundColor Red
    Restore-Files
    exit 1
}

# ── Summary and confirmation ─────────────────────────────────────────────────

Write-Host ""
Info "Release notes for version $Version (from CHANGELOG.md):"
$sec = [regex]::Match($newChangelog, '(?ms)^## \[' + [regex]::Escape($Version) + '\][^\n]*\n(.*?)(?=^## \[|\z)')
Write-Host $sec.Groups[1].Value.Trim()
Write-Host ""
Info "Modified files:"
& git status --short
Write-Host ""

if (-not $Yes) {
    $answer = Read-Host "Publish version $Tag to GitHub? (y/N)"
    if ($answer -notmatch '^(s|si|sì|y|yes)$') {
        Restore-Files
        exit 0
    }
}

# ── Commit, tag, push ────────────────────────────────────────────────────────

try {
    Run 'git' @('add', '-A')
    if (& git status --porcelain) {
        Run 'git' @('commit', '-m', "Version $Version")
        Run 'git' @('push', 'origin', 'main')
    }
    else {
        Info "Nothing to commit: the commit is already in place."
    }
    Run 'git' @('tag', '-a', $Tag, '-m', "Raster Tools $Version")
    Run 'git' @('push', 'origin', $Tag)
}
catch {
    Write-Host $_ -ForegroundColor Red
    Fail "publishing stopped. Check the message above; if the commit was created but not pushed, retry with: git push origin main"
}

$url = ((& git remote get-url origin) -replace '\.git$', '').Trim()
Write-Host ""
Ok "Version $Tag pushed."
Write-Host "The build takes about 15-20 minutes:"
Write-Host "  $url/actions"
Write-Host "Then the release will be here:"
Write-Host "  $url/releases/tag/$Tag"
