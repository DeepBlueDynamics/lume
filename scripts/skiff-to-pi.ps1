# Request a readwrite Signal K token for Skiff and print the startup lines.
#
# Run this on the Windows PC that runs Skiff. It does not start Skiff.
# It does not write the token anywhere. Approve the request in the Signal K
# admin under Security > Access Requests while this script polls.
#
#   powershell -NoProfile -File .\scripts\skiff-to-pi.ps1
#   powershell -NoProfile -File .\scripts\skiff-to-pi.ps1 -SignalKHost halos.local:3000
#
# Do not redirect the output into the git repo. The token is a secret.

[CmdletBinding()]
param(
    [string]$SignalKHost = 'halos.local:3000',
    [string]$Description = 'Skiff sailing simulator',
    [int]$TimeoutSec = 300,
    [int]$PollSec = 2
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Format-PsLiteral {
    param([string]$Value)
    return "'" + ($Value -replace "'", "''") + "'"
}

function Get-HttpBase {
    param([string]$HostValue)
    $h = $HostValue.Trim().TrimEnd('/')
    if ($h -match '^(?i)https://') { return $h }
    if ($h -match '^(?i)http://') { return $h }
    if ($h -match '^(?i)wss://') { return 'https://' + $h.Substring(6) }
    if ($h -match '^(?i)ws://') { return 'http://' + $h.Substring(5) }
    return 'http://' + $h
}

function Get-BareHost {
    param([string]$HostValue)
    $h = $HostValue.Trim()
    $h = $h -replace '^(?i)https?://', ''
    $h = $h -replace '^(?i)wss?://', ''
    $slash = $h.IndexOf('/')
    if ($slash -ge 0) { $h = $h.Substring(0, $slash) }
    return $h.TrimEnd('/')
}

function Get-SkiffClientId {
    $dir = Join-Path $env:USERPROFILE '.skiff'
    $file = Join-Path $dir 'signalk-client-id.json'
    $pattern = '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-5][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$'
    if (Test-Path -LiteralPath $file) {
        try {
            $obj = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json
            if ($obj.clientId -match $pattern) {
                return [string]$obj.clientId
            }
        } catch {
            Write-Host "Replacing unreadable client id file: $file"
        }
    }
    $id = [guid]::NewGuid().ToString()
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    @{ clientId = $id } | ConvertTo-Json | Set-Content -LiteralPath $file -Encoding ascii
    return $id
}

function Get-ResponseToken {
    param($Body)
    if ($null -eq $Body) { return $null }
    if ($null -ne $Body.token -and "$($Body.token)".Length -gt 0) {
        return [string]$Body.token
    }
    if ($null -ne $Body.accessRequest -and $null -ne $Body.accessRequest.token -and "$($Body.accessRequest.token)".Length -gt 0) {
        return [string]$Body.accessRequest.token
    }
    return $null
}

function Resolve-RequestHref {
    param([string]$Base, [string]$Href)
    if ($Href -match '^(?i)https?://') { return $Href }
    if ($Href.StartsWith('/')) { return $Base + $Href }
    return ($Base.TrimEnd('/') + '/' + $Href)
}

function Invoke-SkiffJson {
    param(
        [Parameter(Mandatory = $true)][string]$Method,
        [Parameter(Mandatory = $true)][string]$Uri,
        [string]$JsonBody
    )

    $common = @{
        Uri = $Uri
        Method = $Method
        Headers = @{ Accept = 'application/json' }
        TimeoutSec = 20
        UseBasicParsing = $true
    }
    if ($JsonBody) {
        $common.ContentType = 'application/json; charset=utf-8'
        $common.Body = $JsonBody
    }

    $status = 0
    $content = ''
    $cmd = Get-Command Invoke-WebRequest
    if ($cmd.Parameters.ContainsKey('SkipHttpErrorCheck')) {
        $common.SkipHttpErrorCheck = $true
        $resp = Invoke-WebRequest @common
        $status = [int]$resp.StatusCode
        $content = [string]$resp.Content
    } else {
        try {
            $resp = Invoke-WebRequest @common
            $status = [int]$resp.StatusCode
            $content = [string]$resp.Content
        } catch [System.Net.WebException] {
            $http = $_.Exception.Response
            if (-not $http) { throw }
            $status = [int]$http.StatusCode
            $stream = $http.GetResponseStream()
            $reader = New-Object System.IO.StreamReader($stream)
            try { $content = $reader.ReadToEnd() } finally { $reader.Dispose() }
        }
    }

    $body = $null
    if ($content) {
        try { $body = $content | ConvertFrom-Json } catch { $body = $null }
    }
    [pscustomobject]@{
        Status = $status
        Body = $body
        Raw = $content
    }
}

function Show-StartupLines {
    param([string]$BareHost, [string]$Token)
    Write-Host ''
    Write-Host 'Approved. Paste these into the Skiff window. Do not commit the token.'
    Write-Host 'Skiff always connects with ws://. A readonly Lume plugin token will not send deltas.'
    Write-Host ''
    Write-Output ("`$env:SIGNALK_HOST = {0}" -f (Format-PsLiteral $BareHost))
    Write-Output ("`$env:SIGNALK_TOKEN = {0}" -f (Format-PsLiteral $Token))
    Write-Output 'cargo run --bin skiff'
}

if (-not $env:USERPROFILE) {
    throw 'USERPROFILE is not set, so the client id cannot be stored outside the repo.'
}
if ($TimeoutSec -lt 1) { throw '-TimeoutSec must be at least 1.' }
if ($PollSec -lt 1) { throw '-PollSec must be at least 1.' }
if ([string]::IsNullOrWhiteSpace($SignalKHost)) { throw '-SignalKHost is empty.' }
if ([string]::IsNullOrWhiteSpace($Description)) { throw '-Description is empty.' }

$httpBase = Get-HttpBase $SignalKHost
$bareHost = Get-BareHost $SignalKHost
if ([string]::IsNullOrWhiteSpace($bareHost)) { throw '-SignalKHost did not contain a host.' }

if ($SignalKHost -match '^(?i)(https|wss)://') {
    Write-Host "Skiff ignores the scheme and still opens ws://${bareHost}/signalk/v1/stream?subscribe=none"
    Write-Host 'wss:// is not implemented in the Skiff client.'
}

$clientId = Get-SkiffClientId
$clientFile = Join-Path (Join-Path $env:USERPROFILE '.skiff') 'signalk-client-id.json'
Write-Host "Using client id $clientId"
Write-Host "Client id file (not a token): $clientFile"
Write-Host 'Approve "Skiff sailing simulator" with readwrite under Security > Access Requests.'

$payload = @{
    clientId = $clientId
    description = $Description
    permissions = 'readwrite'
} | ConvertTo-Json -Compress
$requestUrl = $httpBase + '/signalk/v1/access/requests'
Write-Host "POST $requestUrl"

$created = Invoke-SkiffJson -Method 'POST' -Uri $requestUrl -JsonBody $payload
if ($created.Status -lt 200 -or $created.Status -ge 300) {
    Write-Host "Access request failed with HTTP $($created.Status)."
    if ($created.Raw) { Write-Host $created.Raw }
    exit 1
}

$state = ''
if ($created.Body -and $created.Body.state) { $state = [string]$created.Body.state }
$token = Get-ResponseToken $created.Body
if ($state.ToUpperInvariant() -eq 'COMPLETED' -and $token) {
    Show-StartupLines -BareHost $bareHost -Token $token
    exit 0
}
if ($state.ToUpperInvariant() -eq 'DENIED') {
    Write-Host 'The administrator denied the access request.'
    exit 1
}

$href = $null
if ($created.Body) { $href = [string]$created.Body.href }
if ([string]::IsNullOrWhiteSpace($href)) {
    Write-Host 'The access request did not return a token or an href to poll.'
    if ($created.Raw) { Write-Host $created.Raw }
    exit 1
}
$pollUrl = Resolve-RequestHref -Base $httpBase -Href $href
Write-Host "Pending. Polling $pollUrl"

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$noteDue = Get-Date
while ((Get-Date) -lt $deadline) {
    if ((Get-Date) -ge $noteDue) {
        Write-Host 'Waiting. In Signal K admin, open Security > Access Requests and approve this client with readwrite.'
        $noteDue = (Get-Date).AddSeconds(15)
    }

    $poll = Invoke-SkiffJson -Method 'GET' -Uri $pollUrl
    if ($poll.Status -ge 200 -and $poll.Status -lt 300 -and $poll.Body) {
        $pollState = ''
        if ($poll.Body.state) { $pollState = [string]$poll.Body.state }
        $pollToken = Get-ResponseToken $poll.Body
        if ($pollState.ToUpperInvariant() -eq 'COMPLETED' -and $pollToken) {
            Show-StartupLines -BareHost $bareHost -Token $pollToken
            exit 0
        }
        if ($pollState.ToUpperInvariant() -eq 'DENIED') {
            Write-Host 'The administrator denied the access request.'
            exit 1
        }
    } elseif ($poll.Status -ge 400) {
        Write-Host "Poll returned HTTP $($poll.Status). Still waiting until the timeout."
    }

    Start-Sleep -Seconds $PollSec
}

Write-Host "Timed out after $TimeoutSec seconds. The request is still pending at $pollUrl"
Write-Host 'Approve it in Security > Access Requests, then run this script again.'
exit 1
