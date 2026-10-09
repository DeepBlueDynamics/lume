# Request a readwrite Signal K token for Skiff.
#
# Run this on the Windows PC that runs Skiff. It does not start Skiff.
# It never prints the token. The token is saved outside the repo, and the
# file DACL grants only the current user.
#
#   powershell -NoProfile -File .\scripts\skiff-token.ps1
#   powershell -NoProfile -File .\scripts\skiff-token.ps1 -SignalKHost halos.local:3000
#
# Ctrl+C stops the wait. A denied request exits without writing a token.

[CmdletBinding()]
param(
    [string]$SignalKHost = 'halos.local:3000',
    [string]$Description = 'Skiff sailing simulator',
    [int]$PollSec = 2
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$ApproveText = 'approve it in Signal K > Security > Access Requests'

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
    param([string]$Dir)
    $file = Join-Path $Dir 'signalk-client-id.json'
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
    }
}

function Set-UserOnlyAcl {
    param([Parameter(Mandatory = $true)][string]$Path)
    $me = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $account = $me.Name
    $acl = New-Object System.Security.AccessControl.FileSecurity
    $acl.SetAccessRuleProtection($true, $false)
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
        $account,
        'FullControl',
        'Allow')
    $acl.SetAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl

    $check = Get-Acl -LiteralPath $Path
    $others = @($check.Access | Where-Object {
        $_.IdentityReference.Value -ne $account -and
        $_.IdentityReference.Value -ne $me.User.Value
    })
    if ($others.Count -ne 0) {
        $names = ($others | ForEach-Object { $_.IdentityReference.Value }) -join ', '
        throw "Token file ACL still grants: $names"
    }
}

function Save-SkiffToken {
    param(
        [Parameter(Mandatory = $true)][string]$Dir,
        [Parameter(Mandatory = $true)][string]$Token
    )
    $path = Join-Path $Dir 'signalk-token'
    if (-not (Test-Path -LiteralPath $path)) {
        New-Item -ItemType File -Path $path | Out-Null
    }
    # Lock the DACL before the secret is written.
    Set-UserOnlyAcl -Path $path
    [System.IO.File]::WriteAllText($path, $Token)
    return $path
}

function Show-StartupLines {
    param([string]$BareHost, [string]$TokenPath)
    Write-Host ''
    Write-Host "Token saved for the current user only: $TokenPath"
    Write-Host 'The token was not printed. Start Skiff with:'
    Write-Host ''
    Write-Output ("`$env:SIGNALK_HOST = `"{0}`"" -f $BareHost)
    Write-Output '$env:SIGNALK_TOKEN = (Get-Content -Raw "$env:USERPROFILE\.skiff\signalk-token").Trim()'
    Write-Output 'cargo run --bin skiff'
}

if (-not $env:USERPROFILE) {
    throw 'USERPROFILE is not set, so the token cannot be stored outside the repo.'
}
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

$dir = Join-Path $env:USERPROFILE '.skiff'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$clientId = Get-SkiffClientId -Dir $dir
Write-Host "Using client id $clientId"
Write-Host $ApproveText

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
    exit 1
}

$state = ''
if ($created.Body -and $created.Body.state) { $state = [string]$created.Body.state }
$token = Get-ResponseToken $created.Body
if ($state.ToUpperInvariant() -eq 'DENIED') {
    Write-Host 'The administrator denied the access request.'
    exit 1
}
if ($token) {
    $saved = Save-SkiffToken -Dir $dir -Token $token
    Show-StartupLines -BareHost $bareHost -TokenPath $saved
    exit 0
}

$href = $null
if ($created.Body) { $href = [string]$created.Body.href }
if ([string]::IsNullOrWhiteSpace($href)) {
    Write-Host 'The access request did not return a token or an href to poll.'
    exit 1
}
$pollUrl = Resolve-RequestHref -Base $httpBase -Href $href
Write-Host "Pending. Polling $pollUrl"
Write-Host $ApproveText

$noteDue = (Get-Date).AddSeconds(15)
while ($true) {
    if ((Get-Date) -ge $noteDue) {
        Write-Host $ApproveText
        $noteDue = (Get-Date).AddSeconds(15)
    }

    try {
        $poll = Invoke-SkiffJson -Method 'GET' -Uri $pollUrl
    } catch {
        Write-Host 'Poll failed. Still waiting.'
        Start-Sleep -Seconds $PollSec
        continue
    }

    if ($poll.Status -ge 200 -and $poll.Status -lt 300 -and $poll.Body) {
        $pollState = ''
        if ($poll.Body.state) { $pollState = [string]$poll.Body.state }
        if ($pollState.ToUpperInvariant() -eq 'DENIED') {
            Write-Host 'The administrator denied the access request.'
            exit 1
        }
        $pollToken = Get-ResponseToken $poll.Body
        if ($pollToken -and ($pollState.Length -eq 0 -or $pollState.ToUpperInvariant() -eq 'COMPLETED')) {
            $saved = Save-SkiffToken -Dir $dir -Token $pollToken
            Show-StartupLines -BareHost $bareHost -TokenPath $saved
            exit 0
        }
    } elseif ($poll.Status -ge 400) {
        Write-Host "Poll returned HTTP $($poll.Status). Still waiting."
    }

    Start-Sleep -Seconds $PollSec
}
