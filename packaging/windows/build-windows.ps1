# build-windows.ps1 - build the official IRIS Windows zip from clean.
# Usage (on Windows, from this directory):
#   pwsh -NoProfile -ExecutionPolicy Bypass -File build-windows.ps1
# Runs on a Windows host or a windows-latest CI runner: it builds the
# IRIS API binary (x86_64-pc-windows-msvc) and the web dist from this
# repo checkout, downloads the pinned Windows Stalwart + Caddy + NSSM
# releases (SHA-256 checked), and assembles
#   build/windows/iris-mail-stack-<VERSION>-windows-x86_64.zip
# IRIS is built from the repo checkout this packaging/ directory sits
# in, never from a fetched tag.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$PkgDir = $PSScriptRoot
$Root = Split-Path (Split-Path $PkgDir -Parent) -Parent
$Version = (Get-Content (Join-Path $Root 'packaging/VERSION') -Raw).Trim()
$PkgName = 'iris-mail-stack'
$ZipName = "$PkgName-$Version-windows-x86_64.zip"
$Downloads = Join-Path $PkgDir 'build/windows/downloads'
$Staging = Join-Path $PkgDir "build/windows/staging/$PkgName-$Version-windows-x86_64"
$ZipPath = Join-Path $PkgDir "build/windows/$ZipName"

function Read-VersionsEnv {
    param([string]$Path)
    $vars = @{}
    foreach ($line in (Get-Content $Path)) {
        if ($line -match '^\s*#' -or $line -notmatch '=') { continue }
        $key, $value = $line -split '=', 2
        $vars[$key.Trim()] = $value.Trim()
    }
    return $vars
}

function Get-Sha256 {
    param([string]$Path)
    return (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
}

function Get-VerifiedDownload {
    param([string]$Url, [string]$Sha256, [string]$OutFile)
    if (-not (Test-Path $OutFile)) {
        Write-Host "== downloading $(Split-Path $OutFile -Leaf)"
        curl.exe -fsSL --retry 5 --retry-delay 10 --retry-all-errors $Url -o $OutFile
    }
    $actual = Get-Sha256 -Path $OutFile
    if ($actual -ne $Sha256.ToLowerInvariant()) {
        throw "SHA-256 mismatch for $(Split-Path $OutFile -Leaf): got $actual, want $Sha256"
    }
    Write-Host "== sha256 OK $(Split-Path $OutFile -Leaf)"
}

function Test-PowerShellSyntax {
    param([string]$Path)
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) {
        foreach ($err in $errors) { Write-Host "parse error in ${Path}: $($err.Message)" }
        throw "PowerShell syntax check failed for $Path"
    }
}

$V = Read-VersionsEnv -Path (Join-Path $PkgDir 'versions-windows.env')
Write-Host "== versions: package $Version, stalwart $($V['STALWART_VERSION']), caddy $($V['CADDY_VERSION']), nssm $($V['NSSM_VERSION']), iris $(git -C $Root rev-parse --short HEAD)"

# --- verify downloads ----------------------------------------------------
New-Item -ItemType Directory -Force -Path $Downloads | Out-Null
Get-VerifiedDownload -Url $V['STALWART_URL'] -Sha256 $V['STALWART_SHA256'] -OutFile (Join-Path $Downloads $V['STALWART_ASSET'])
Get-VerifiedDownload -Url $V['CADDY_URL'] -Sha256 $V['CADDY_SHA256'] -OutFile (Join-Path $Downloads $V['CADDY_ASSET'])
Get-VerifiedDownload -Url $V['NSSM_URL'] -Sha256 $V['NSSM_SHA256'] -OutFile (Join-Path $Downloads $V['NSSM_ASSET'])

# --- IRIS artefacts --------------------------------------------------------
# Built from this repo checkout. A release build must run from the
# release tag (packaging/VERSION must equal the tag version), so refuse
# a dirty tree: a package must never silently bundle uncommitted changes.
# Tracked files only: the downloads this script just fetched live in
# packaging/windows/build/, and build outputs (target/, dist/) are
# untracked too; counting them would make the check fail on its own work.
$dirty = git -C $Root status --porcelain --untracked-files=no
if ($dirty) { throw "irisproject tree is dirty; commit or stash before building a package:`n$dirty" }

$ApiBin = Join-Path $Root 'target/release/iris-api.exe'
if (-not (Test-Path $ApiBin)) {
    Write-Host '== building iris-api (this takes a few minutes)'
    Push-Location $Root
    try { cargo build --release --bin iris-api } finally { Pop-Location }
}
if (-not (Test-Path $ApiBin)) { throw 'iris-api binary missing after build' }
Write-Host "== iris-api: $((Get-Sha256 -Path $ApiBin).Substring(0, 16))..."

$WebDist = Join-Path $Root 'apps/web/dist'
if (-not (Test-Path (Join-Path $WebDist 'index.html'))) {
    Write-Host '== building web dist (this takes a few minutes)'
    Push-Location (Join-Path $Root 'apps/web')
    try { npm run build } finally { Pop-Location }
}
if (-not (Test-Path (Join-Path $WebDist 'index.html'))) { throw 'web dist missing after build' }
$mainJs = Get-ChildItem (Join-Path $WebDist 'assets/main-*.js') | Select-Object -First 1
if (-not $mainJs -or -not (Select-String -Path $mainJs.FullName -Pattern 'Revoke sessions' -Quiet)) {
    throw 'web dist is missing the PR #23 strings'
}

# --- assemble the staging tree --------------------------------------------
# Layout mirrors the Linux package, with Windows differences:
# services are NSSM-wrapped Windows services, not systemd units; the
# installer is install.ps1 (run as Administrator); and paths live
# under C:\Program Files\IRIS instead of /usr/local, /etc, /var/lib.
if (Test-Path $Staging) { Remove-Item $Staging -Recurse -Force }
$dirs = @(
    'bin', 'dist', 'scripts',
    'etc/iris', 'etc/stalwart/tls', 'etc/caddy',
    'data/iris', 'data/stalwart', 'logs/stalwart', 'logs/iris', 'logs/caddy'
)
foreach ($d in $dirs) { New-Item -ItemType Directory -Force -Path (Join-Path $Staging $d) | Out-Null }

# Binaries: extract each zip to a temp dir and take the one file we need.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('iris-win-build-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    Expand-Archive -Path (Join-Path $Downloads $V['STALWART_ASSET']) -DestinationPath (Join-Path $tmp 'stalwart') -Force
    Copy-Item (Join-Path $tmp 'stalwart/stalwart.exe') (Join-Path $Staging 'bin/stalwart.exe')
    Expand-Archive -Path (Join-Path $Downloads $V['CADDY_ASSET']) -DestinationPath (Join-Path $tmp 'caddy') -Force
    Copy-Item (Join-Path $tmp 'caddy/caddy.exe') (Join-Path $Staging 'bin/caddy.exe')
    Expand-Archive -Path (Join-Path $Downloads $V['NSSM_ASSET']) -DestinationPath (Join-Path $tmp 'nssm') -Force
    Copy-Item (Join-Path $tmp 'nssm/nssm-2.24/win64/nssm.exe') (Join-Path $Staging 'bin/nssm.exe')
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
Copy-Item $ApiBin (Join-Path $Staging 'bin/iris-api.exe')

# Web dist.
Copy-Item (Join-Path $WebDist '*') (Join-Path $Staging 'dist') -Recurse -Force

# Stalwart store config (the installer rewrites the data path if the
# install root is not the default C:\Program Files\IRIS).
$defaultData = 'C:/Program Files/IRIS/data/stalwart/data'
Set-Content -Path (Join-Path $Staging 'etc/stalwart/config.json') -Value "{`"@type`":`"RocksDb`",`"path`":`"$defaultData`"}" -Encoding Ascii

# Scripts and templates.
Copy-Item (Join-Path $PkgDir 'install.ps1') (Join-Path $Staging 'scripts/install.ps1')
Copy-Item (Join-Path $PkgDir 'iris-setup.ps1') (Join-Path $Staging 'scripts/iris-setup.ps1')
Copy-Item (Join-Path $PkgDir 'check-windows.ps1') (Join-Path $Staging 'scripts/check-windows.ps1')
Copy-Item (Join-Path $PkgDir 'Caddyfile.windows.template') (Join-Path $Staging 'etc/caddy/Caddyfile.template')
Copy-Item (Join-Path $PkgDir 'README-Windows.txt') (Join-Path $Staging 'README-Windows.txt')

# --- static checks ---------------------------------------------------------
foreach ($script in @('install.ps1', 'iris-setup.ps1', 'check-windows.ps1')) {
    Test-PowerShellSyntax -Path (Join-Path $Staging "scripts/$script")
}
Write-Host '== PowerShell syntax checks passed'

# --- zip -------------------------------------------------------------------
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path $Staging -DestinationPath $ZipPath -CompressionLevel Optimal
Write-Host "== wrote $ZipPath"
Write-Host "== sha256: $(Get-Sha256 -Path $ZipPath)"
