# iris-setup.ps1 - provision a fresh Stalwart for IRIS through the JMAP
# management API. PowerShell port of packaging/src/usr/lib/iris/iris-setup.py
# (same steps, same order); run by install.ps1 while Stalwart runs
# directly (not as a service) with a one-time recovery admin.
# Standard PowerShell only; no extra modules.
param(
    [Parameter(Mandatory = $true)][string]$Domain,
    [Parameter(Mandatory = $true)][string]$AdminEmail,
    [Parameter(Mandatory = $true)][string]$AdminLocal,
    [Parameter(Mandatory = $true)][string]$AdminPassword,
    [Parameter(Mandatory = $true)][string]$TempPassword,
    [Parameter(Mandatory = $true)][string]$RecoveryPassword,
    [Parameter(Mandatory = $true)][string]$InstallRoot
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Jmap = 'http://127.0.0.1:8080/jmap'
$JmapUsing = @('urn:ietf:params:jmap:core', 'urn:stalwart:jmap')
$InboxDomain = "inbox.$Domain"
$MailHost = "mail.$Domain"

function Write-SetupLog { param([string]$Message) Write-Host "iris-setup: $Message" }
function Fail { param([string]$Message) throw "iris-setup: error: $Message" }

function Get-BasicAuth {
    param([string]$User, [string]$Password)
    $pair = "${User}:${Password}"
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($pair)
    return 'Basic ' + [System.Convert]::ToBase64String($bytes)
}

$RecoveryAuth = Get-BasicAuth -User 'admin' -Password $RecoveryPassword

function Invoke-Jmap {
    param([string]$Method, [hashtable]$Arguments)
    $body = @{
        using = $JmapUsing
        methodCalls = @(, @($Method, $Arguments, '0'))
    } | ConvertTo-Json -Depth 10 -Compress
    try {
        $resp = Invoke-RestMethod -Uri "$Jmap/" -Method Post -Body $body -ContentType 'application/json' `
            -Headers @{ Authorization = $RecoveryAuth } -TimeoutSec 30
    } catch {
        $detail = $_.ErrorDetails.Message
        Fail "$Method failed to answer: $($_.Exception.Message) $detail"
    }
    # A structurally valid JMAP response always carries methodResponses.
    # If it does not, something else answered on 127.0.0.1:8080 (or the
    # server rejected the whole request): fail with the raw body instead
    # of a cryptic StrictMode property error further down.
    $mrProp = $resp.PSObject.Properties['methodResponses']
    if ($null -eq $mrProp -or $mrProp.Value.Count -lt 1) {
        $raw = $resp | ConvertTo-Json -Depth 5 -Compress
        Fail "$Method: unexpected response (no methodResponses): $($raw.Substring(0, [Math]::Min(1000, $raw.Length)))"
    }
    $first = $mrProp.Value[0]
    if ($first[0] -eq 'error') {
        Fail "$Method returned JMAP error: $($first[1] | ConvertTo-Json -Depth 5 -Compress)"
    }
    $result = $first[1]
    # StrictMode is on: a missing property throws, so test for the
    # property's existence first (the Python original uses .get()).
    foreach ($key in @('notCreated', 'notUpdated', 'notDestroyed')) {
        $prop = $result.PSObject.Properties[$key]
        if ($null -ne $prop -and $prop.Value) {
            $refused = $prop.Value | ConvertTo-Json -Depth 5 -Compress
            Fail "$Method refused: $($refused.Substring(0, [Math]::Min(400, $refused.Length)))"
        }
    }
    return $result
}

function Get-CreatedId {
    param($Result, [string]$Method, [string]$ClientId)
    $createdProp = $Result.PSObject.Properties['created']
    if ($null -eq $createdProp) {
        $raw = $Result | ConvertTo-Json -Depth 5 -Compress
        Fail "$Method returned no 'created' object: $($raw.Substring(0, [Math]::Min(1000, $raw.Length)))"
    }
    $entryProp = $createdProp.Value.PSObject.Properties[$ClientId]
    if ($null -eq $entryProp) {
        $raw = $Result | ConvertTo-Json -Depth 5 -Compress
        Fail "$Method did not create '$ClientId': $($raw.Substring(0, [Math]::Min(1000, $raw.Length)))"
    }
    return $entryProp.Value.id
}

# 1. Wait for the management API. A 200 that is not a Stalwart session
# (no capabilities object) means something else owns 127.0.0.1:8080:
# fail at once with the body instead of failing cryptically later.
$ready = $false
$lastSessionBody = ''
for ($i = 0; $i -lt 30; $i++) {
    try {
        $sess = Invoke-RestMethod -Uri "$Jmap/session" -Headers @{ Authorization = $RecoveryAuth } -TimeoutSec 5
        if ($null -ne $sess.PSObject.Properties['capabilities']) {
            $ready = $true
            break
        }
        $lastSessionBody = $sess | ConvertTo-Json -Depth 5 -Compress
        Fail "something other than Stalwart answered /session on 127.0.0.1:8080: $($lastSessionBody.Substring(0, [Math]::Min(500, $lastSessionBody.Length)))"
    } catch {
        if ($_.Exception.Message -match 'something other than Stalwart') { throw }
        Start-Sleep -Seconds 2
    }
}
if (-not $ready) { Fail 'Stalwart management API did not answer on 127.0.0.1:8080' }

# 2. Domains (DKIM keys are generated automatically per domain).
$res = Invoke-Jmap -Method 'x:Domain/set' -Arguments @{
    create = @{
        'd-main'  = @{ name = $Domain }
        'd-inbox' = @{ name = $InboxDomain }
    }
}
$domainIds = @{
    $Domain      = Get-CreatedId -Result $res -Method 'x:Domain/set' -ClientId 'd-main'
    $InboxDomain = Get-CreatedId -Result $res -Method 'x:Domain/set' -ClientId 'd-inbox'
}
Write-SetupLog "domains $Domain=$($domainIds[$Domain]), $InboxDomain=$($domainIds[$InboxDomain])"

# 3. Accounts: admin on the main domain, temp on the inbox domain.
function New-JmapAccount {
    param([string]$Name, [string]$DomainName, [string]$Password, [string]$Description)
    $created = Invoke-Jmap -Method 'x:Account/set' -Arguments @{
        create = @{
            u1 = @{
                '@type' = 'User'
                name = $Name
                domainId = $domainIds[$DomainName]
                description = $Description
                credentials = @{ '0' = @{ '@type' = 'Password'; secret = $Password } }
                roles = @{ '@type' = 'User' }
                permissions = @{ '@type' = 'Inherit' }
                encryptionAtRest = @{ '@type' = 'Disabled' }
            }
        }
    }
    return Get-CreatedId -Result $created -Method 'x:Account/set' -ClientId 'u1'
}
$adminId = New-JmapAccount -Name $AdminLocal -DomainName $Domain -Password $AdminPassword -Description 'IRIS admin account'
$tempId = New-JmapAccount -Name 'temp' -DomainName $InboxDomain -Password $TempPassword -Description 'IRIS temp inbox catch-all account'
Write-SetupLog "accounts $AdminEmail=$adminId, temp@$InboxDomain=$tempId"

# 4. Catch-all on the inbox domain so every temp address lands in temp.
Invoke-Jmap -Method 'x:Domain/set' -Arguments @{
    update = @{ $domainIds[$InboxDomain] = @{ catchAllAddress = "temp@$InboxDomain" } }
} | Out-Null
Write-SetupLog "catch-all $InboxDomain -> temp@$InboxDomain"

# 5. Server identity: hostname + default domain.
Invoke-Jmap -Method 'x:SystemSettings/set' -Arguments @{
    create = @{
        singleton = @{
            defaultHostname = $MailHost
            defaultDomainId = $domainIds[$Domain]
        }
    }
} | Out-Null
Write-SetupLog "default hostname $MailHost"

# 6. TLS: the certificate object reads the files the installer maintains,
# so replacing the files and reloading is enough to rotate the cert.
# Only when the installer actually wrote the files (no openssl and no
# .NET self-signed path on this machine means no files: skip, the
# listeners still work, TLS is added once the files exist).
$tlsDir = Join-Path $InstallRoot 'etc/stalwart/tls'
$fullchain = Join-Path $tlsDir 'fullchain.pem'
$privkey = Join-Path $tlsDir 'privkey.pem'
if ((Test-Path $fullchain) -and (Test-Path $privkey)) {
    $fullchainFwd = $fullchain -replace '\\', '/'
    $privkeyFwd = $privkey -replace '\\', '/'
    $cert = Invoke-Jmap -Method 'x:Certificate/set' -Arguments @{
        create = @{
            le = @{
                certificate = @{ '@type' = 'File'; filePath = $fullchainFwd }
                privateKey = @{ '@type' = 'File'; filePath = $privkeyFwd }
            }
        }
    }
    $certId = Get-CreatedId -Result $cert -Method 'x:Certificate/set' -ClientId 'le'
    Invoke-Jmap -Method 'x:SystemSettings/set' -Arguments @{
        update = @{ singleton = @{ defaultCertificateId = $certId } }
    } | Out-Null
    Invoke-Jmap -Method 'x:Action/set' -Arguments @{
        create = @{ a1 = @{ '@type' = 'ReloadTlsCertificates' } }
    } | Out-Null
    Write-SetupLog "TLS certificate $certId (files in $tlsDir)"
} else {
    Write-SetupLog 'WARNING: no TLS certificate files in etc\stalwart\tls; skipping the certificate object (add the files and re-run provisioning, or set them up in the Stalwart admin UI)'
}

# 7. Listeners: Stalwart creates its default listener set at the first
# start, even in recovery mode (smtp 25, submissions 465, imaps 993,
# pop3s 995, sieve 4190, https 443, http 8080), all bound to every
# interface. The https listener on 443 collides with Caddy, which owns
# 443 on this host: Stalwart wins the bind and answers HTTPS with its
# own self-signed certificate, so the web UI never loads. So the
# defaults must be edited, not created: rebind the management/JMAP
# HTTP listener to loopback only, delete the ones IRIS never uses, and
# leave smtp on 25 for inbound mail. No IMAP, POP3, submission, Sieve,
# or HTTPS listener: IRIS clients use the web UI and the API, never
# Stalwart directly.
$listeners = Invoke-Jmap -Method 'x:NetworkListener/get' -Arguments @{ properties = @('name') }
$byName = @{}
foreach ($entry in $listeners.list) { $byName[$entry.name] = $entry.id }
if ($byName.ContainsKey('http')) {
    Invoke-Jmap -Method 'x:NetworkListener/set' -Arguments @{
        update = @{ $byName['http'] = @{ bind = @{ '127.0.0.1:8080' = $true } } }
    } | Out-Null
} else {
    Invoke-Jmap -Method 'x:NetworkListener/set' -Arguments @{
        create = @{
            'l-http' = @{
                name = 'http'; protocol = 'http'
                bind = @{ '127.0.0.1:8080' = $true }; implicitTls = $false
            }
        }
    } | Out-Null
}
$gone = @()
foreach ($name in @('submissions', 'imaps', 'pop3s', 'sieve', 'https')) {
    if ($byName.ContainsKey($name)) { $gone += $byName[$name] }
}
if ($gone.Count -gt 0) {
    Invoke-Jmap -Method 'x:NetworkListener/set' -Arguments @{ destroy = $gone } | Out-Null
}
if (-not $byName.ContainsKey('smtp')) {
    Invoke-Jmap -Method 'x:NetworkListener/set' -Arguments @{
        create = @{
            'l-smtp' = @{
                name = 'smtp'; protocol = 'smtp'; bind = @{ '[::]:25' = $true }
            }
        }
    } | Out-Null
}
Write-SetupLog 'listeners smtp [::]:25, http 127.0.0.1:8080 (no 443/465/993/995/4190: Caddy owns 443, IRIS needs no IMAP/POP3)'

# 8. DNS zone files for the operator to publish.
$domains = Invoke-Jmap -Method 'x:Domain/get' -Arguments @{ properties = @('name', 'dnsZoneFile') }
$zones = @()
foreach ($d in $domains.list) {
    # dnsZoneFile may be absent for a domain; StrictMode forbids
    # reading a missing property, so guard it like .get() in Python.
    $zoneProp = $d.PSObject.Properties['dnsZoneFile']
    $zoneText = ''
    if ($null -ne $zoneProp) { $zoneText = $zoneProp.Value }
    $zones += "# DNS records for $($d.name)`n$zoneText"
}
$zonePath = Join-Path $InstallRoot 'etc/iris/dns-zone.txt'
[System.IO.File]::WriteAllText($zonePath, ($zones -join "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-SetupLog "DNS zone file written to $zonePath"

# 9. Verify the admin account can log in over JMAP.
$adminAuth = Get-BasicAuth -User $AdminEmail -Password $AdminPassword
try {
    $sess = Invoke-RestMethod -Uri "$Jmap/session" -Headers @{ Authorization = $adminAuth } -TimeoutSec 15
    $acct = $sess.primaryAccounts.'urn:ietf:params:jmap:mail'
} catch {
    Fail "admin JMAP login failed: $($_.Exception.Message)"
}
Write-SetupLog "admin JMAP login OK (account $acct)"
