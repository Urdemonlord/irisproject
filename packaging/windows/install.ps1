#Requires -RunAsAdministrator
# install.ps1 - installer for the official IRIS Windows zip.
# Run as Administrator from the extracted zip:
#   .\scripts\install.ps1
# or non-interactively:
#   .\scripts\install.ps1 -Domain example.com -NonInteractive
# Mirrors the Linux iris-install.sh: asks for one domain, generates all
# secrets, provisions Stalwart through the JMAP management API, registers
# the three services with NSSM, and prints the admin password once.
# Idempotent: safe to run again on upgrade; generated files are created
# once and never overwritten.
param(
    [string]$Domain = '',
    [string]$AdminEmail = '',
    [string]$AdminPassword = '',
    [string]$InstallRoot = (Join-Path $env:ProgramFiles 'IRIS'),
    [switch]$NonInteractive
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The payload lives next to this script: <extracted>\scripts\install.ps1,
# so the zip root (bin\, dist\, etc\) is the parent of $PSScriptRoot.
$PayloadRoot = Split-Path $PSScriptRoot -Parent

function Write-Log { param([string]$Message) Write-Host "iris-install: $Message" }
function Fail { param([string]$Message) throw "iris-install: error: $Message" }

function New-RandomHex {
    param([int]$Bytes)
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $buf = New-Object byte[] $Bytes
    $rng.GetBytes($buf)
    $rng.Dispose()
    return ([System.BitConverter]::ToString($buf) -replace '-', '').ToLowerInvariant()
}

function Test-DomainValid {
    param([string]$Value)
    if ($Value -notmatch '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$') { return $false }
    if ($Value.Contains('..') -or $Value.Contains('_') -or $Value.Contains('://') -or $Value.Contains('/')) { return $false }
    return $true
}

function Protect-SecretFile {
    param([string]$Path)
    & icacls.exe $Path /inheritance:r /grant:r 'Administrators:(F)' 'SYSTEM:(F)' | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "icacls failed on $Path" }
}

# --- domain ----------------------------------------------------------------
if (-not $Domain) {
    if ($NonInteractive) { Fail 'IRIS_DOMAIN is not set (pass -Domain or run interactively)' }
    $Domain = Read-Host 'Domain for this mail server (example: example.com)'
}
$Domain = $Domain.Trim().ToLowerInvariant()
if (-not (Test-DomainValid -Value $Domain)) { Fail "IRIS_DOMAIN '$Domain' is not a plain domain name" }

$MailHost = "mail.$Domain"
$ContentHost = "mail-content.$Domain"
$InboxDomain = "inbox.$Domain"

if (-not $AdminEmail) {
    if ($NonInteractive) {
        $AdminEmail = "admin@$Domain"
    } else {
        $answer = Read-Host "Admin email [admin@$Domain]"
        if (-not $answer) { $answer = "admin@$Domain" }
        $AdminEmail = $answer
    }
}
$AdminEmail = $AdminEmail.Trim().ToLowerInvariant()
if (-not $AdminEmail.EndsWith("@$Domain")) { Fail "Admin email must be an address on $Domain" }
$AdminLocal = ($AdminEmail -split '@')[0]

# --- directories -----------------------------------------------------------
$BinDir = Join-Path $InstallRoot 'bin'
$DistDir = Join-Path $InstallRoot 'dist'
$ScriptsDir = Join-Path $InstallRoot 'scripts'
$EtcIris = Join-Path $InstallRoot 'etc/iris'
$EtcStalwart = Join-Path $InstallRoot 'etc/stalwart'
$EtcStalwartTls = Join-Path $EtcStalwart 'tls'
$EtcCaddy = Join-Path $InstallRoot 'etc/caddy'
$DataIris = Join-Path $InstallRoot 'data/iris'
$DataStalwart = Join-Path $InstallRoot 'data/stalwart'
$LogsStalwart = Join-Path $InstallRoot 'logs/stalwart'
$LogsIris = Join-Path $InstallRoot 'logs/iris'
$LogsCaddy = Join-Path $InstallRoot 'logs/caddy'
foreach ($dir in @($BinDir, $DistDir, $ScriptsDir, $EtcIris, $EtcStalwart, $EtcStalwartTls, $EtcCaddy, $DataIris, $DataStalwart, $LogsStalwart, $LogsIris, $LogsCaddy)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

# --- copy the payload (binaries, web app, scripts) --------------------------
Copy-Item (Join-Path $PayloadRoot 'bin/*.exe') $BinDir -Force
if (Test-Path $DistDir) { Remove-Item $DistDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
Copy-Item (Join-Path $PayloadRoot 'dist/*') $DistDir -Recurse -Force
Copy-Item (Join-Path $PayloadRoot 'scripts/*.ps1') $ScriptsDir -Force
Copy-Item (Join-Path $PayloadRoot 'etc/caddy/Caddyfile.template') (Join-Path $EtcCaddy 'Caddyfile.template') -Force
$Nssm = Join-Path $BinDir 'nssm.exe'
$StalwartExe = Join-Path $BinDir 'stalwart.exe'
$ApiExe = Join-Path $BinDir 'iris-api.exe'
$CaddyExe = Join-Path $BinDir 'caddy.exe'
foreach ($exe in @($Nssm, $StalwartExe, $ApiExe, $CaddyExe)) {
    if (-not (Test-Path $exe)) { Fail "missing payload binary: $exe" }
}

# --- Stalwart store config (static file; the installer fixes the data ------
# --- path when the install root is not the default) ------------------------
$StalwartConfig = Join-Path $EtcStalwart 'config.json'
$wantedData = (($InstallRoot -replace '\\', '/') + '/data/stalwart/data')
$rewriteConfig = $true
if (Test-Path $StalwartConfig) {
    $existing = Get-Content $StalwartConfig -Raw
    if ($existing -match [regex]::Escape($wantedData)) { $rewriteConfig = $false }
}
if ($rewriteConfig) {
    Set-Content -Path $StalwartConfig -Value "{`"@type`":`"RocksDb`",`"path`":`"$wantedData`"}" -Encoding Ascii
    Write-Log "wrote $StalwartConfig"
}

# --- Caddyfile (rendered once from the shipped template) --------------------
$Caddyfile = Join-Path $EtcCaddy 'Caddyfile'
if (-not (Test-Path $Caddyfile)) {
    $template = Get-Content (Join-Path $EtcCaddy 'Caddyfile.template') -Raw
    $distRoot = (($InstallRoot -replace '\\', '/') + '/dist')
    $rendered = $template.Replace('@MAIL_HOST@', $MailHost).Replace('@CONTENT_HOST@', $ContentHost).Replace('@DIST_ROOT@', $distRoot)
    Set-Content -Path $Caddyfile -Value $rendered -Encoding Ascii
    Write-Log "wrote $Caddyfile for $MailHost"
}

# --- self-signed TLS for the mail listeners (replaced once Caddy ---------
# --- has a real certificate; there is no cert-sync on Windows yet) --------
$FullchainPem = Join-Path $EtcStalwartTls 'fullchain.pem'
$PrivkeyPem = Join-Path $EtcStalwartTls 'privkey.pem'
if ((-not (Test-Path $FullchainPem)) -or (-not (Test-Path $PrivkeyPem))) {
    $certDone = $false
    $openssl = Get-Command openssl -ErrorAction SilentlyContinue
    if ($openssl) {
        & openssl req -x509 -newkey rsa:2048 -keyout $PrivkeyPem -out $FullchainPem -days 30 -nodes -subj "/CN=$MailHost" 2>$null
        if ($LASTEXITCODE -eq 0) { $certDone = $true }
    }
    if (-not $certDone) {
        try {
            $rsa = [System.Security.Cryptography.RSA]::Create(2048)
            $request = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
                "CN=$MailHost", $rsa,
                [System.Security.Cryptography.HashAlgorithmName]::SHA256,
                [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
            $cert = $request.CreateSelfSigned(
                [System.DateTimeOffset]::Now.AddDays(-1),
                [System.DateTimeOffset]::Now.AddDays(30))
            $certB64 = [System.Convert]::ToBase64String($cert.RawData, 'InsertLineBreaks')
            $keyB64 = [System.Convert]::ToBase64String($rsa.ExportPkcs8PrivateKey(), 'InsertLineBreaks')
            Set-Content -Path $FullchainPem -Value "-----BEGIN CERTIFICATE-----`n$certB64`n-----END CERTIFICATE-----" -Encoding Ascii
            Set-Content -Path $PrivkeyPem -Value "-----BEGIN PRIVATE KEY-----`n$keyB64`n-----END PRIVATE KEY-----" -Encoding Ascii
            $certDone = $true
        } catch {
            Write-Log "WARNING: could not generate a self-signed certificate ($($_.Exception.Message)). TLS for the mail listeners stays off until certificate files are placed in $EtcStalwartTls"
        }
    }
    if ($certDone) {
        Write-Log "generated a 30-day self-signed certificate for $MailHost (replace the files in $EtcStalwartTls with the Caddy certificate once one is issued)"
    }
}

# --- IRIS env + generated secrets (created once, never overwritten) --------
$EnvFile = Join-Path $InstallRoot '.env'
$AdminCredsFile = Join-Path $EtcIris '.admin_credentials'
$generatedAdminPw = $false
if (-not (Test-Path $EnvFile)) {
    $sessionSecret = New-RandomHex -Bytes 32
    $tempPw = New-RandomHex -Bytes 16
    if ($AdminPassword) {
        $adminPw = $AdminPassword
    } else {
        $adminPw = New-RandomHex -Bytes 12
        $generatedAdminPw = $true
    }
    $envText = @"
# IRIS configuration, generated by the IRIS Windows installer.
# Edit with care; restart the IRISApi service after changes.
IRIS_ENV=production
IRIS_HOST=127.0.0.1
IRIS_PORT=8000
STALWART_API_URL=http://127.0.0.1:8080
DATABASE_URL=sqlite:./data/iris/iris.db
SESSION_SECRET=$sessionSecret
SEND_LOCAL_DOMAINS=$Domain
SEND_EXTERNAL=true
TEMP_INBOX_DOMAIN=$InboxDomain
TEMP_INBOX_DEFAULT_EXPIRY_HOURS=1
TEMP_INBOX_MAIL_USER=temp@$InboxDomain
TEMP_INBOX_MAIL_PASSWORD=$tempPw
MAIL_CONTENT_ORIGIN=https://$ContentHost
IRIS_ADMIN_EMAILS=$AdminEmail
TURNSTILE_SECRET_KEY=
TURNSTILE_SITE_KEY=
TURNSTILE_HOSTNAMES=
IRIS_INTEGRATION_EMAIL_ENABLED=false
IRIS_INTEGRATION_EMAIL_FROM=
RESEND_API_KEY=
"@
    Set-Content -Path $EnvFile -Value $envText -Encoding Ascii
    Protect-SecretFile -Path $EnvFile
    Set-Content -Path $AdminCredsFile -Value "$adminPw`n" -Encoding Ascii -NoNewline:$false
    Protect-SecretFile -Path $AdminCredsFile
    Write-Log "wrote $EnvFile (admin password saved to $AdminCredsFile, Administrators + SYSTEM only)"
}

# --- provision Stalwart (once) ----------------------------------------------
$ProvisionMarker = Join-Path $DataStalwart '.iris-provisioned'
if (-not (Test-Path $ProvisionMarker)) {
    $adminPw = (Get-Content $AdminCredsFile -Raw).Trim()
    $tempPw = $null
    foreach ($line in (Get-Content $EnvFile)) {
        if ($line -match '^TEMP_INBOX_MAIL_PASSWORD=(.+)$') { $tempPw = $Matches[1].Trim() }
    }
    if (-not $tempPw) { Fail 'TEMP_INBOX_MAIL_PASSWORD not found in .env' }
    $recoveryPw = New-RandomHex -Bytes 16
    $setupLog = Join-Path $LogsStalwart 'setup.log'
    $setupErrLog = Join-Path $LogsStalwart 'setup.err.log'
    # Run Stalwart directly (not as a service) with a one-time recovery
    # admin, provision through the JMAP management API, then stop it.
    # The recovery password is never stored; the service runs without
    # it, so it stops working after this step.
    $env:STALWART_RECOVERY_ADMIN = "admin:$recoveryPw"
    try {
        # NOTE: -ArgumentList must be ONE string with the path quoted by
        # hand. Passing '--config', $path as an array lets PowerShell
        # split the space-containing path, so Stalwart received
        # --config C:\Program and started in bootstrap mode (to it, the
        # config file "does not exist"). The NSSM service registration
        # below already uses this single-string form.
        Write-Log "starting Stalwart with config $StalwartConfig (exists: $(Test-Path $StalwartConfig))"
        Write-Log "config content: $(Get-Content $StalwartConfig -Raw)"
        $proc = Start-Process -FilePath $StalwartExe -ArgumentList "--config `"$StalwartConfig`"" `
            -RedirectStandardOutput $setupLog -RedirectStandardError $setupErrLog `
            -PassThru -WindowStyle Hidden
        $cmdLine = (Get-CimInstance Win32_Process -Filter "ProcessId = $($proc.Id)" -ErrorAction SilentlyContinue).CommandLine
        if ($cmdLine) { Write-Log "stalwart command line: $cmdLine" }
        try {
            $setupArgs = @{
                Domain = $Domain
                AdminEmail = $AdminEmail
                AdminLocal = $AdminLocal
                AdminPassword = $adminPw
                TempPassword = $tempPw
                RecoveryPassword = $recoveryPw
                InstallRoot = $InstallRoot
            }
            & (Join-Path $ScriptsDir 'iris-setup.ps1') @setupArgs
            New-Item -ItemType File -Force -Path $ProvisionMarker | Out-Null
            Write-Log "Stalwart provisioned for $Domain"
        } finally {
            if (-not $proc.HasExited) {
                Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
                $proc.WaitForExit(10000) | Out-Null
            }
        }
    } finally {
        Remove-Item Env:\STALWART_RECOVERY_ADMIN -ErrorAction SilentlyContinue
    }
}

# --- register services with NSSM --------------------------------------------
function Install-NssmService {
    param(
        [string]$Name,
        [string]$DisplayName,
        [string]$Exe,
        [string]$Parameters,
        [string]$StdoutLog,
        [string]$StderrLog
    )
    $existing = Get-Service -Name $Name -ErrorAction SilentlyContinue
    if (-not $existing) {
        & $Nssm install $Name $Exe $Parameters | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail "nssm install $Name failed" }
        & $Nssm set $Name DisplayName $DisplayName | Out-Null
        Write-Log "registered service $Name"
    }
    & $Nssm set $Name AppDirectory $InstallRoot | Out-Null
    & $Nssm set $Name AppParameters $Parameters | Out-Null
    & $Nssm set $Name AppStdout $StdoutLog | Out-Null
    & $Nssm set $Name AppStderr $StderrLog | Out-Null
    # Ctrl+C stop: Stalwart stops only on Ctrl+C, and the API and Caddy
    # are console apps too; NSSM sends the console event, waits 5s,
    # then terminates.
    & $Nssm set $Name AppStopMethodConsole 5000 | Out-Null
    & $Nssm set $Name Start SERVICE_AUTO_START | Out-Null
}

Install-NssmService -Name 'IRISStalwart' -DisplayName 'IRIS Stalwart' `
    -Exe $StalwartExe -Parameters "--config `"$StalwartConfig`"" `
    -StdoutLog (Join-Path $LogsStalwart 'stalwart.log') -StderrLog (Join-Path $LogsStalwart 'stalwart.err.log')
Install-NssmService -Name 'IRISApi' -DisplayName 'IRIS API' `
    -Exe $ApiExe -Parameters '' `
    -StdoutLog (Join-Path $LogsIris 'iris-api.log') -StderrLog (Join-Path $LogsIris 'iris-api.err.log')
Install-NssmService -Name 'IRISCaddy' -DisplayName 'IRIS Caddy' `
    -Exe $CaddyExe -Parameters "run --config `"$Caddyfile`" --adapter caddyfile" `
    -StdoutLog (Join-Path $LogsCaddy 'caddy.log') -StderrLog (Join-Path $LogsCaddy 'caddy.err.log')

foreach ($svc in @('IRISStalwart', 'IRISApi', 'IRISCaddy')) {
    $s = Get-Service -Name $svc
    if ($s.Status -eq 'Running') { Restart-Service -Name $svc -Force }
    else { Start-Service -Name $svc }
}

# --- Windows Firewall ---------------------------------------------------------
$fwRules = @(
    @{ Name = 'IRIS (HTTP)'; Port = 80 },
    @{ Name = 'IRIS (HTTPS)'; Port = 443 },
    @{ Name = 'IRIS (SMTP)'; Port = 25 },
    @{ Name = 'IRIS (Submission)'; Port = 587 },
    @{ Name = 'IRIS (IMAPS)'; Port = 993 }
)
foreach ($rule in $fwRules) {
    if (-not (Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -DisplayName $rule.Name -Direction Inbound -Protocol TCP -LocalPort $rule.Port -Action Allow -Profile Any | Out-Null
        Write-Log "firewall rule added: $($rule.Name) (TCP $($rule.Port))"
    }
}

# --- wait for the API and print the summary -----------------------------------
$healthy = $false
for ($i = 0; $i -lt 30; $i++) {
    try {
        Invoke-RestMethod -Uri 'http://127.0.0.1:8000/health' -TimeoutSec 2 | Out-Null
        $healthy = $true
        break
    } catch {
        Start-Sleep -Seconds 2
    }
}
if (-not $healthy) { Fail 'iris-api did not answer /health within 60s; check the IRISApi service and logs/iris/iris-api.log' }

$adminPw = (Get-Content $AdminCredsFile -Raw).Trim()
$dnsZone = Join-Path $EtcIris 'dns-zone.txt'
Write-Host ''
Write-Host 'IRIS is up:'
Write-Host "  Webmail:  https://$MailHost"
Write-Host "  Admin:    $AdminEmail (password in $AdminCredsFile, printed once below)"
Write-Host "  Temp inbox domain: $InboxDomain"
Write-Host ''
Write-Host 'Before mail flows, publish these DNS records (full zone file in dns-zone.txt):'
if (Test-Path $dnsZone) { Get-Content $dnsZone -TotalCount 12 }
Write-Host ''
Write-Host 'Admin password (shown once, also stored in .admin_credentials):'
Write-Host $adminPw
Write-Host ''
if ($generatedAdminPw) {
    Write-Host 'The admin password above was generated. Change it in the Stalwart admin UI or with iris-setup later.'
}
Write-Host 'Caddy issues TLS certificates automatically once the DNS records point here.'
Write-Host 'Outbound mail goes out over port 25 directly; if your provider blocks it, set RESEND_API_KEY in .env.'
Write-Host 'Note: this Windows install has no cert-sync yet; the self-signed certificate in etc\stalwart\tls is used for the mail listeners until you replace it with the Caddy certificate.'
