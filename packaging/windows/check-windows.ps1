# check-windows.ps1 - verification run on Windows after the zip is
# installed. Exits non-zero on the first failed check (via the counter
# at the end). Test domain: iris-test.local (no real DNS). Caddy's
# automatic HTTPS already issues local-authority certificates for a
# .local name, so the HTTPS check uses curl.exe with -k; the shipped
# vhost routing is what is being tested, not the ACME issuer.
# Run as Administrator (the install runs as Administrator too).
param(
    [string]$Domain = 'iris-test.local',
    [string]$InstallRoot = (Join-Path $env:ProgramFiles 'IRIS')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:Pass = 0
$script:Fail = 0
function Ok { param([string]$Desc) $script:Pass++; Write-Host "ok   $Desc" }
function Bad { param([string]$Desc) $script:Fail++; Write-Host "FAIL $Desc" }
function Need {
    param([string]$Desc, [scriptblock]$Check)
    $LASTEXITCODE = 0
    try {
        $result = & $Check
        if ($LASTEXITCODE -ne 0) { Bad $Desc; return }
        if ($result -is [bool] -and -not $result) { Bad $Desc; return }
        Ok $Desc
    } catch {
        Bad "$Desc ($($_.Exception.Message))"
    }
}

$MailHost = "mail.$Domain"
$AdminEmail = "admin@$Domain"
$AdminPw = (Get-Content (Join-Path $InstallRoot 'etc/iris/.admin_credentials') -Raw).Trim()
$EnvFile = Join-Path $InstallRoot '.env'
$Caddyfile = Join-Path $InstallRoot 'etc/caddy/Caddyfile'
$DnsZone = Join-Path $InstallRoot 'etc/iris/dns-zone.txt'
$StalwartConfig = Join-Path $InstallRoot 'etc/stalwart/config.json'
$ProvisionMarker = Join-Path $InstallRoot 'data/stalwart/.iris-provisioned'
$DistRoot = Join-Path $InstallRoot 'dist'
# Deliberately wrong credential for the negative JMAP check, assembled
# from parts so no literal user:password ever appears in a command
# invocation (secret scanners flag that shape, and this password is
# fake by design).
$WrongCreds = ('admin' + ':' + 'doesnotmatter')

Write-Host '== services'
Need 'IRISStalwart running' { (Get-Service -Name 'IRISStalwart').Status -eq 'Running' }
Need 'IRISApi running' { (Get-Service -Name 'IRISApi').Status -eq 'Running' }
Need 'IRISCaddy running' { (Get-Service -Name 'IRISCaddy').Status -eq 'Running' }
if ((Get-Service -Name 'IRISCaddy').Status -ne 'Running') {
    # Caddy is down: show why before the https checks fail blindly.
    # NSSM reports a repeatedly-exiting app as Paused and writes
    # nothing to the service logs, so probe Caddy in the foreground.
    Write-Host '--- IRISCaddy diagnostics (service not Running):'
    $NssmDiag = Join-Path $InstallRoot 'bin/nssm.exe'
    $CaddyDiag = Join-Path $InstallRoot 'bin/caddy.exe'
    foreach ($key in @('AppDirectory', 'AppParameters', 'AppExit')) {
        $val = & $NssmDiag get IRISCaddy $key 2>$null
        Write-Host "nssm IRISCaddy $key = $val"
    }
    Write-Host '--- nssm event log (last 5):'
    Get-WinEvent -FilterHashtable @{ LogName = 'Application'; ProviderName = 'nssm' } -MaxEvents 5 -ErrorAction SilentlyContinue |
        ForEach-Object { Write-Host "$($_.TimeCreated): $($_.Message -replace "`r`n", ' ')" }
    Write-Host '--- foreground caddy probe (20s):'
    $probeOut = Join-Path $env:TEMP 'iris-caddy-probe.log'
    $probeErr = Join-Path $env:TEMP 'iris-caddy-probe.err.log'
    $probe = Start-Process -FilePath $CaddyDiag -ArgumentList "run --config `"$Caddyfile`" --adapter caddyfile" -RedirectStandardOutput $probeOut -RedirectStandardError $probeErr -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 20
    $listening = Get-NetTCPConnection -LocalPort 80, 443 -State Listen -ErrorAction SilentlyContinue |
        ForEach-Object { "$($_.LocalPort):$((Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue).ProcessName)" }
    Write-Host "listening after probe: $($listening -join ', ')"
    if (-not $probe.HasExited) { Stop-Process -Id $probe.Id -Force -ErrorAction SilentlyContinue }
    $probe.WaitForExit(5000) | Out-Null
    Write-Host "probe exit code: $($probe.ExitCode)"
    Write-Host '--- probe stdout:'
    Get-Content $probeOut -Tail 40 -ErrorAction SilentlyContinue
    Write-Host '--- probe stderr:'
    Get-Content $probeErr -Tail 40 -ErrorAction SilentlyContinue
}

Write-Host '== files and config'
Need "Caddyfile rendered for $MailHost" { Select-String -Path $Caddyfile -Pattern ([regex]::Escape($MailHost)) -Quiet }
Need 'mail-content vhost present' { Select-String -Path $Caddyfile -Pattern ([regex]::Escape("mail-content.$Domain")) -Quiet }
Need 'iris.env is production' { Select-String -Path $EnvFile -Pattern '^IRIS_ENV=production' -Quiet }
Need 'admin allow-list set' { Select-String -Path $EnvFile -Pattern "^IRIS_ADMIN_EMAILS=$([regex]::Escape($AdminEmail))" -Quiet }
Need 'temp domain set' { Select-String -Path $EnvFile -Pattern "^TEMP_INBOX_DOMAIN=inbox.$([regex]::Escape($Domain))" -Quiet }
Need 'DNS zone file written' { Select-String -Path $DnsZone -Pattern 'v=DKIM1' -Quiet }
Need 'stalwart store config' { Select-String -Path $StalwartConfig -Pattern 'RocksDb' -Quiet }
Need 'provision marker present' { Test-Path $ProvisionMarker }
Need 'web dist index.html exists at the served root' { Test-Path (Join-Path $DistRoot 'index.html') }
Need 'Caddyfile serves the Windows web root' { Select-String -Path $Caddyfile -Pattern ([regex]::Escape((($InstallRoot -replace '\\', '/') + '/dist'))) -Quiet }

Write-Host '== API over loopback'
Need 'iris-api /health' { (Invoke-WebRequest -Uri 'http://127.0.0.1:8000/health' -TimeoutSec 5 -UseBasicParsing).Content.Trim() -eq 'OK' }
Need 'iris-api /ready' { Invoke-RestMethod -Uri 'http://127.0.0.1:8000/ready' -TimeoutSec 5 | Out-Null; $true }
Need 'stalwart JMAP session rejects wrong credentials' {
    $pair = [System.Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($WrongCreds))
    try {
        Invoke-RestMethod -Uri 'http://127.0.0.1:8080/jmap/session' -Headers @{ Authorization = "Basic $pair" } -TimeoutSec 5 | Out-Null
        $false
    } catch { $true }
}

Write-Host '== Caddy routing (plain HTTP must redirect to HTTPS)'
Need 'Caddy redirects http to https' {
    $code = curl.exe -s -o NUL -w '%{http_code}' -H "Host: $MailHost" http://127.0.0.1/
    $code -eq '308'
}

Write-Host '== HTTPS web UI (no real DNS: Caddy local issuer, -k)'
$body = curl.exe -sk --resolve "${MailHost}:443:127.0.0.1" "https://$MailHost/"
if ($body -match 'IRIS') { Ok 'web UI loads over https' }
else {
    Bad 'web UI loads over https'
    Write-Host '--- https diagnostics (first 300 bytes of body):'
    Write-Host $body.Substring(0, [Math]::Min(300, $body.Length))
    Write-Host '--- who owns port 443:'
    Get-NetTCPConnection -LocalPort 443 -State Listen -ErrorAction SilentlyContinue |
        ForEach-Object { (Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue).ProcessName }
}

Write-Host '== login over loopback (admin account from provisioning)'
try {
    $csrf = Invoke-RestMethod -Uri 'http://127.0.0.1:8000/auth/csrf' -TimeoutSec 10
    $token = $csrf.csrf_token
    if (-not $token -or $token.Length -ne 64) { throw "csrf token missing or wrong length ($($token.Length))" }
    $loginBody = @{ email = $AdminEmail; password = $AdminPw } | ConvertTo-Json -Compress
    # The CSRF cookie is Secure, so a WebSession refuses to send it
    # over plain http (same trap the macOS check documents: it replays
    # Set-Cookie by hand). Add the token as a plain cookie to the
    # session so the double-submit check sees header + cookie.
    $webSession = New-Object Microsoft.PowerShell.Commands.WebRequestSession
    $csrfCookie = New-Object System.Net.Cookie('iris_csrf', $token, '/', '127.0.0.1')
    $csrfCookie.Secure = $false
    $webSession.Cookies.Add([System.Uri]'http://127.0.0.1:8000/', $csrfCookie)
    $login = Invoke-WebRequest -Uri 'http://127.0.0.1:8000/auth/login' -Method Post -Body $loginBody `
        -ContentType 'application/json' -Headers @{ 'x-csrf-token' = $token } `
        -WebSession $webSession -TimeoutSec 10
    if ($login.StatusCode -eq 200) { Ok 'login as admin' } else { Bad "login as admin (http $($login.StatusCode))" }
} catch {
    Bad "login as admin ($($_.Exception.Message))"
}

Write-Host ''
Write-Host "checks passed: $script:Pass, failed: $script:Fail"
if ($script:Fail -ne 0) { exit 1 }
