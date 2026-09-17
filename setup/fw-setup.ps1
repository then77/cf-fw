#requires -Version 5.1
<#
.SYNOPSIS
    Interactive Windows setup for FW + a locally managed Cloudflare Tunnel.

.DESCRIPTION
    This script is intended to be downloaded, SHA-256 verified, and launched by
    `fw.exe --setup`. The executable remains only the trusted bootstrapper; this
    script owns OAuth, Cloudflare API calls, file installation, validation,
    testing, rollback, and silent OAuth revocation.

.PARAMETER FWPath
    Absolute path to the FW executable that launched this script. Its parent
    directory is used as the installation root, regardless of the executable's
    filename.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string] $FWPath,
    [string] $OAuthClientId = 'c5ddfa8b3cab280893b3bcc428dcc7c9',
    [string[]] $BaseScopes = @(
        'account-settings.read',
        'zone.read',
        'dns.write',
        'argotunnel.write',
        'ssl-and-certificates.read'
    ),
    [string] $WorkersScope = 'workers-scripts.write'
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$script:ApiBase = 'https://api.cloudflare.com/client/v4'
$script:AuthorizeEndpoint = 'https://dash.cloudflare.com/oauth2/auth'
$script:TokenEndpoint = 'https://dash.cloudflare.com/oauth2/token'
$script:RevokeEndpoint = 'https://dash.cloudflare.com/oauth2/revoke'
$script:OAuthCallbackPorts = @(8976, 8977, 8978)
$script:RedirectUri = $null
$script:CloudflaredVersion = '2026.9.1'
$script:CloudflaredUrl = $null
$script:CloudflaredSha256 = $null
$script:WorkerCompatibilityDate = '2026-09-17'
$script:CertificatePollSeconds = 15
$script:CertificatePollAttempts = 40
$script:WorkerCertificateAttempts = 4
$script:CertificateFailureStates = @(
    'validation_timed_out',
    'issuance_timed_out',
    'deployment_timed_out',
    'deletion_timed_out',
    'initializing_timed_out'
)

$script:AccessToken = $null
$script:AllTokens = New-Object System.Collections.Generic.List[string]
$script:Journal = [ordered]@{
    AccountId       = $null
    ZoneId          = $null
    TunnelId        = $null
    DnsRecordId     = $null
    WorkerName      = $null
    WorkerDomainId  = $null
    WorkerCreated   = $false
    TestServer      = $null
    TestProcess     = $null
    TemporaryFiles  = New-Object System.Collections.Generic.List[string]
    CreatedFiles    = New-Object System.Collections.Generic.List[string]
}
$script:Committed = $false
$script:StatusSpinner = $null
$hostSupportsAnsi = ($Host.UI.PSObject.Properties.Name -contains 'SupportsVirtualTerminal') -and $Host.UI.SupportsVirtualTerminal
$script:Ansi = $hostSupportsAnsi -or ($null -ne $env:WT_SESSION) -or ($null -ne $env:TERM_PROGRAM)
$Esc = [char]27
$Bold = "${Esc}[1m"
$ErrorLabel = "${Esc}[41;1m"
$Reset = "${Esc}[0m"

function Write-Bold {
    param(
        [Parameter(Mandatory)][string] $Text,
        [switch] $NoNewline,
        [ValidateSet('Blue', 'Green', 'Yellow')][string] $ForegroundColor
    )
    if ($script:Ansi) {
        $colorCode = switch ($ForegroundColor) {
            'Blue' { '34' }
            'Green' { '32' }
            'Yellow' { '33' }
            default { $null }
        }
        $prefix = if ($colorCode) { "${Esc}[${colorCode};1m" } else { $Bold }
        Write-Host "${prefix}${Text}${Reset}" -NoNewline:$NoNewline
    } elseif ($ForegroundColor) {
        Write-Host $Text -ForegroundColor $ForegroundColor -NoNewline:$NoNewline
    } else {
        Write-Host $Text -NoNewline:$NoNewline
    }
}

function Write-ErrorLine {
    param([Parameter(Mandatory)][string] $Message)
    if ($script:Ansi) {
        Write-Host "${ErrorLabel} ERROR ${Reset} $Message"
    } else {
        Write-Host " ERROR  $Message" -ForegroundColor Red
    }
}

function Stop-StatusAnimation {
    if ($null -eq $script:StatusSpinner) { return }

    $spinner = $script:StatusSpinner
    $script:StatusSpinner = $null
    try {
        [void]$spinner.StopEvent.Set()
        try { [void]$spinner.PowerShell.EndInvoke($spinner.Handle) } catch { }
    } finally {
        $spinner.PowerShell.Dispose()
        $spinner.Runspace.Dispose()
        $spinner.StopEvent.Dispose()
    }
}

function Start-Status {
    param([Parameter(Mandatory)][string] $Message)

    Stop-StatusAnimation
    Write-Host "⠋ $Message" -NoNewline

    $stopEvent = [Threading.ManualResetEvent]::new($false)
    $runspace = [Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
    $runspace.Open()
    $worker = [Management.Automation.PowerShell]::Create()
    $worker.Runspace = $runspace
    $spinnerScript = {
        param($StopEvent, $StatusMessage)
        $frames = @('⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏')
        $originalEncoding = [Console]::OutputEncoding
        $index = 1
        try {
            [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
            while (-not $StopEvent.WaitOne(80)) {
                [Console]::Write("`r{0} {1}", $frames[$index], $StatusMessage)
                $index = ($index + 1) % $frames.Count
            }
        } finally {
            [Console]::OutputEncoding = $originalEncoding
        }
    }
    [void]$worker.AddScript($spinnerScript.ToString()).AddArgument($stopEvent).AddArgument($Message)
    $script:StatusSpinner = [pscustomobject]@{
        StopEvent = $stopEvent
        Runspace = $runspace
        PowerShell = $worker
        Handle = $worker.BeginInvoke()
    }
}

function Complete-Status {
    param([string] $Message = 'SUCCESS')
    Stop-StatusAnimation
    Write-Host " $Message" -ForegroundColor Green
}

function Fail-Status {
    Stop-StatusAnimation
    Write-Host ' FAILED' -ForegroundColor Red
}

function Read-YesNo {
    param([Parameter(Mandatory)][string] $Prompt, [bool] $DefaultYes = $true)
    $answer = Read-Host $Prompt
    if ([string]::IsNullOrWhiteSpace($answer)) { return $DefaultYes }
    return $answer.Trim() -match '^(?i:y|yes)$'
}

function New-SecureRandomBytes {
    param([Parameter(Mandatory)][ValidateRange(1, 1024)][int] $ByteCount)
    $bytes = New-Object byte[] $ByteCount
    $generator = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $generator.GetBytes($bytes)
        return ,$bytes
    } finally {
        $generator.Dispose()
    }
}

function New-RandomHex {
    return [BitConverter]::ToString((New-SecureRandomBytes 2)).Replace('-', '').ToLowerInvariant()
}

function New-RandomBase64Url {
    param([int] $ByteCount = 32)
    return [Convert]::ToBase64String((New-SecureRandomBytes $ByteCount)).TrimEnd('=').Replace('+', '-').Replace('/', '_')
}

function New-TunnelSecret {
    return [Convert]::ToBase64String((New-SecureRandomBytes 32))
}

function ConvertTo-QueryString {
    param([Parameter(Mandatory)][hashtable] $Values)
    $pairs = foreach ($key in $Values.Keys) {
        '{0}={1}' -f [Uri]::EscapeDataString([string]$key), [Uri]::EscapeDataString([string]$Values[$key])
    }
    return ($pairs -join '&')
}

function ConvertTo-FormBody {
    param([Parameter(Mandatory)][hashtable] $Values)
    return ConvertTo-QueryString $Values
}

function Get-HttpErrorMessage {
    param([Parameter(Mandatory)] $ErrorRecord)
    $message = $ErrorRecord.Exception.Message
    try {
        $response = $ErrorRecord.Exception.Response
        if ($response) {
            $stream = $response.GetResponseStream()
            if ($stream) {
                $reader = [IO.StreamReader]::new($stream)
                try {
                    $body = $reader.ReadToEnd()
                } finally {
                    $reader.Dispose()
                }
                if ($body) {
                    try {
                        $parsed = $body | ConvertFrom-Json
                        if ($parsed.errors) {
                            $details = @($parsed.errors | ForEach-Object { "[$($_.code)] $($_.message)" }) -join '; '
                            if ($details) { return $details }
                        }
                        if ($parsed.error_description) { return [string]$parsed.error_description }
                        if ($parsed.error) { return [string]$parsed.error }
                    } catch { }
                    return $body
                }
            }
        }
    } catch { }
    return $message
}

function Test-ObjectProperty {
    param(
        [Parameter(Mandatory)] $InputObject,
        [Parameter(Mandatory)][string] $Name
    )
    return $null -ne $InputObject.PSObject.Properties[$Name]
}

function Invoke-CfApi {
    param(
        [Parameter(Mandatory)][ValidateSet('GET','POST','PUT','PATCH','DELETE')] [string] $Method,
        [Parameter(Mandatory)][string] $Path,
        [object] $Body,
        [switch] $ReturnEnvelope
    )
    if (-not $script:AccessToken) { throw 'Cloudflare access token is unavailable.' }
    $headers = @{ Authorization = "Bearer $($script:AccessToken)" }
    $uri = if ($Path.StartsWith('http')) { $Path } else { "$($script:ApiBase)$Path" }
    $args = @{
        Uri = $uri
        Method = $Method
        Headers = $headers
        UseBasicParsing = $true
        ErrorAction = 'Stop'
    }
    if ($null -ne $Body) {
        $args.ContentType = 'application/json'
        $args.Body = $Body | ConvertTo-Json -Depth 20 -Compress
    }
    try {
        $response = Invoke-RestMethod @args
    } catch {
        throw (Get-HttpErrorMessage $_)
    }
    if ($null -eq $response -or $response.success -ne $true) {
        $details = @($response.errors | ForEach-Object { "[$($_.code)] $($_.message)" }) -join '; '
        if (-not $details) { $details = 'Cloudflare returned an unsuccessful API response.' }
        throw $details
    }
    if ($ReturnEnvelope) { return $response }
    return $response.result
}

function Invoke-OAuthTokenRequest {
    param([Parameter(Mandatory)][hashtable] $Fields)
    try {
        return Invoke-RestMethod -Uri $script:TokenEndpoint -Method POST -UseBasicParsing `
            -ContentType 'application/x-www-form-urlencoded' -Body (ConvertTo-FormBody $Fields)
    } catch {
        throw (Get-HttpErrorMessage $_)
    }
}

function New-OAuthResponsePage {
    param(
        [Parameter(Mandatory)][ValidateSet('Failed', 'Completed')] [string] $Type,
        [Parameter(Mandatory)][string] $Message
    )

    $encodedMessage = [Net.WebUtility]::HtmlEncode($Message)
    if ($Type -eq 'Completed') {
        $title = 'OAuth completed'
        $heading = 'OAuth completed'
        $background = '#0f111a'
    } else {
        $title = 'OAuth Failed'
        $heading = 'OAuth failed'
        $background = '#1a120f'
    }

    return '<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><title>{0}</title><style>:root{{color-scheme:light dark;font-family:system-ui,sans-serif}}body{{min-height:100vh;margin:0;display:grid;place-items:center;background:{1};color:#f9fafb}}main{{max-width:32rem;padding:2rem;text-align:center}}h1{{font-size:clamp(2rem, 6vw, 3rem);margin:0 0 1rem}}p{{color:#9ca3af;line-height:1.6}}</style></head><body><main><h1>{2}</h1><p>{3}</p></main></body></html>' -f $title, $background, $heading, $encodedMessage
}

function Send-LoopbackResponse {
    param(
        [Parameter(Mandatory)][Net.Sockets.NetworkStream] $Stream,
        [Parameter(Mandatory)][string] $Status,
        [Parameter(Mandatory)][string] $Body
    )
    $bodyBytes = [Text.Encoding]::UTF8.GetBytes($Body)
    $headers = "HTTP/1.1 $Status`r`nContent-Type: text/html; charset=utf-8`r`nContent-Length: $($bodyBytes.Length)`r`nConnection: close`r`n`r`n"
    $headerBytes = [Text.Encoding]::ASCII.GetBytes($headers)
    $Stream.Write($headerBytes, 0, $headerBytes.Length)
    $Stream.Write($bodyBytes, 0, $bodyBytes.Length)
    $Stream.Flush()
}

function Start-OAuthCallbackListener {
    foreach ($port in $script:OAuthCallbackPorts) {
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $port)
        try {
            $listener.Start()
            $script:RedirectUri = "http://127.0.0.1:$port/oauth/callback"
            return $listener
        } catch {
            try { $listener.Stop() } catch { }
        }
    }
    throw 'FW could not listen on OAuth callback ports 8976, 8977, or 8978. Close the programs using those ports and try again.'
}

function Receive-OAuthCallback {
    param(
        [Parameter(Mandatory)][string] $ExpectedState,
        [Parameter(Mandatory)][Net.Sockets.TcpListener] $Listener
    )
    $accept = $Listener.AcceptTcpClientAsync()
    $deadline = [DateTime]::UtcNow.AddMinutes(5)
    while (-not $accept.IsCompleted) {
        if ([DateTime]::UtcNow -ge $deadline) {
            throw 'Timed out waiting for Cloudflare authorization.'
        }
        Start-Sleep -Milliseconds 100
    }
    $client = $accept.Result
    try {
        $stream = $client.GetStream()
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::ASCII, $false, 4096, $true)
        $requestLine = $reader.ReadLine()
        while ($reader.ReadLine()) { }
        if ($requestLine -notmatch '^GET\s+([^\s]+)\s+HTTP/') {
            Send-LoopbackResponse $stream '400 Bad Request' (New-OAuthResponsePage -Type Failed -Message 'FW setup could not read the OAuth response.')
            throw 'Invalid OAuth callback request.'
        }
        $callback = [Uri]("http://127.0.0.1" + $Matches[1])
        if ($callback.AbsolutePath -ne '/oauth/callback') {
            Send-LoopbackResponse $stream '404 Not Found' (New-OAuthResponsePage -Type Failed -Message 'Not found.')
            throw 'Unexpected OAuth callback path.'
        }
        $query = @{}
        foreach ($part in $callback.Query.TrimStart('?').Split('&')) {
            if (-not $part) { continue }
            $pieces = $part.Split('=', 2)
            $key = [Uri]::UnescapeDataString($pieces[0].Replace('+', ' '))
            $value = if ($pieces.Count -gt 1) { [Uri]::UnescapeDataString($pieces[1].Replace('+', ' ')) } else { '' }
            $query[$key] = $value
        }
        if ($query['state'] -ne $ExpectedState) {
            Send-LoopbackResponse $stream '400 Bad Request' (New-OAuthResponsePage -Type Failed -Message 'FW setup rejected this OAuth response.')
            throw 'OAuth state validation failed.'
        }
        if ($query['error']) {
            $oauthError = if ($query['error_description']) { $query['error_description'] } else { $query['error'] }
            Send-LoopbackResponse $stream '400 Bad Request' (New-OAuthResponsePage -Type Failed -Message $oauthError)
            throw $oauthError
        }
        if (-not $query['code']) {
            Send-LoopbackResponse $stream '400 Bad Request' (New-OAuthResponsePage -Type Failed -Message 'No authorization code was returned.')
            throw 'Cloudflare returned no authorization code.'
        }
        Send-LoopbackResponse $stream '200 OK' (New-OAuthResponsePage -Type Completed -Message 'You can close this window and return to FW Setup.')
        return [string]$query['code']
    } finally {
        if ($client) { $client.Dispose() }
    }
}

function Invoke-CloudflareLogin {
    param([Parameter(Mandatory)][string[]] $Scopes)
    $state = New-RandomBase64Url 24
    $verifier = New-RandomBase64Url 64
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $challenge = [Convert]::ToBase64String($sha.ComputeHash([Text.Encoding]::ASCII.GetBytes($verifier))).TrimEnd('=').Replace('+','-').Replace('/','_')
    } finally { $sha.Dispose() }

    $listener = Start-OAuthCallbackListener
    try {
        $query = ConvertTo-QueryString @{
            response_type = 'code'
            client_id = $OAuthClientId
            redirect_uri = $script:RedirectUri
            scope = ($Scopes -join ' ')
            state = $state
            code_challenge = $challenge
            code_challenge_method = 'S256'
        }
        $authUri = "$($script:AuthorizeEndpoint)?$query"
        Write-Host 'Open this URL in your browser to authorize to Cloudflare:'
        Write-Host $authUri -ForegroundColor DarkGray
        Write-Host ''
        Start-Process $authUri | Out-Null
        $code = Receive-OAuthCallback -ExpectedState $state -Listener $listener
    } finally {
        try { $listener.Stop() } catch { }
    }
    $token = Invoke-OAuthTokenRequest @{
        grant_type = 'authorization_code'
        client_id = $OAuthClientId
        code = $code
        redirect_uri = $script:RedirectUri
        code_verifier = $verifier
    }
    if (-not $token.access_token) { throw 'Cloudflare returned no access token.' }
    if ((Test-ObjectProperty $token 'scope') -and $token.scope) {
        $granted = @(([string]$token.scope).Split(' ', [StringSplitOptions]::RemoveEmptyEntries))
        $missing = @($Scopes | Where-Object { $granted -notcontains $_ })
        if ($missing.Count -gt 0) {
            throw "Cloudflare authorization did not grant every required permission: $($missing -join ', ')."
        }
    }
    $script:AccessToken = [string]$token.access_token
    $script:AllTokens.Add($script:AccessToken)
    return $token
}

function Get-AllPages {
    param([Parameter(Mandatory)][string] $Path)
    $items = New-Object System.Collections.Generic.List[object]
    $page = 1
    $pageSize = 50
    do {
        $separator = if ($Path.Contains('?')) { '&' } else { '?' }
        $envelope = Invoke-CfApi GET "$Path${separator}page=$page&per_page=$pageSize" -ReturnEnvelope
        $result = @($envelope.result)
        foreach ($item in $result) { $items.Add($item) }

        $totalPages = $page
        if ((Test-ObjectProperty $envelope 'result_info') -and $envelope.result_info) {
            $resultInfo = $envelope.result_info
            if ((Test-ObjectProperty $resultInfo 'total_pages') -and $resultInfo.total_pages) {
                $totalPages = [int]$resultInfo.total_pages
            } elseif ((Test-ObjectProperty $resultInfo 'total_count') -and $resultInfo.total_count) {
                $effectivePageSize = if ((Test-ObjectProperty $resultInfo 'per_page') -and $resultInfo.per_page) { [int]$resultInfo.per_page } else { $pageSize }
                $totalPages = [Math]::Ceiling([double]$resultInfo.total_count / $effectivePageSize)
            } elseif ($result.Count -eq $pageSize) {
                $totalPages = $page + 1
            }
        } elseif ($result.Count -eq $pageSize) {
            $totalPages = $page + 1
        }
        $page++
    } while ($page -le $totalPages)
    return $items.ToArray()
}

function Select-Account {
    param([Parameter(Mandatory)][object[]] $Accounts)
    $accounts = @($Accounts)
    if ($accounts.Count -eq 0) { throw 'No Cloudflare accounts are available to this authorization.' }
    if ($accounts.Count -eq 1) { return $accounts[0] }
    Write-Host ''
    Write-Bold 'Select a Cloudflare account:'
    for ($i = 0; $i -lt $accounts.Count; $i++) { Write-Host ("  [{0}] {1}" -f ($i + 1), $accounts[$i].name) }
    while ($true) {
        $choice = Read-Host 'Account number'
        $number = 0
        if ([int]::TryParse($choice, [ref]$number) -and $number -ge 1 -and $number -le $accounts.Count) {
            return $accounts[$number - 1]
        }
        Write-ErrorLine 'Choose one of the account numbers shown above.'
    }
}

function Read-WildcardHostname {
    while ($true) {
        $value = (Read-Host 'Wildcard domain (for example, *.fw.example.com)').Trim().ToLowerInvariant().TrimEnd('.')
        if ($value -notmatch '^\*\.(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$') {
            Write-ErrorLine 'Enter a valid wildcard hostname beginning with *.'
            continue
        }
        return $value
    }
}

function Find-ZoneForHostname {
    param([Parameter(Mandatory)][string] $Hostname, [Parameter(Mandatory)][string] $AccountId)
    $encoded = [Uri]::EscapeDataString($AccountId)
    $zones = @(Get-AllPages "/zones?account.id=$encoded") | Where-Object { $_.status -eq 'active' }
    $plain = $Hostname.Substring(2)
    return $zones | Where-Object {
        $plain -eq $_.name -or $plain.EndsWith(".$($_.name)", [StringComparison]::OrdinalIgnoreCase)
    } | Sort-Object { $_.name.Length } -Descending | Select-Object -First 1
}

function Get-DnsRecordsByName {
    param([string] $ZoneId, [string] $Name)
    $encoded = [Uri]::EscapeDataString($Name)
    return @(Get-AllPages "/zones/$ZoneId/dns_records?name.exact=$encoded")
}

function Get-CertificatePacks {
    param([Parameter(Mandatory)][string] $ZoneId)
    return @(Get-AllPages "/zones/$ZoneId/ssl/certificate_packs?status=all")
}

function Find-WildcardCertificate {
    param([Parameter(Mandatory)][object[]] $Packs, [Parameter(Mandatory)][string] $RequestedHostname)
    foreach ($pack in $Packs) {
        foreach ($packHost in @($pack.hosts)) {
            if ($packHost -and ([string]$packHost).Equals($RequestedHostname, [StringComparison]::OrdinalIgnoreCase)) {
                return $pack
            }
        }
    }
    return $null
}

function Test-CertificateActive {
    param([Parameter(Mandatory)] $Pack)
    if ($Pack.status -eq 'active') { return $true }
    foreach ($certificate in @($Pack.certificates)) {
        if ($certificate.status -eq 'active') { return $true }
    }
    return $false
}

function Test-CertificateFailed {
    param([Parameter(Mandatory)] $Pack)
    if ($script:CertificateFailureStates -contains [string]$Pack.status) { return $true }
    foreach ($certificate in @($Pack.certificates)) {
        if ($script:CertificateFailureStates -contains [string]$certificate.status) { return $true }
    }
    return $false
}

function New-WorkerScript {
    param([string] $AccountId, [string] $WorkerName)
    Add-Type -AssemblyName System.Net.Http
    $client = New-Object Net.Http.HttpClient
    $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $script:AccessToken)
    $multipart = New-Object Net.Http.MultipartFormDataContent
    try {
        $metadata = @{ main_module = 'worker.js'; compatibility_date = $script:WorkerCompatibilityDate } | ConvertTo-Json -Compress
        $metadataContent = [Net.Http.StringContent]::new($metadata, [Text.Encoding]::UTF8, 'application/json')
        $multipart.Add($metadataContent, 'metadata')
        $code = @'
export default {
  async fetch(request, env, ctx) {
    return new Response("I'm here!");
  }
};
'@
        $codeContent = [Net.Http.StringContent]::new($code, [Text.Encoding]::UTF8, 'application/javascript+module')
        $multipart.Add($codeContent, 'worker.js', 'worker.js')
        $uri = "$($script:ApiBase)/accounts/$AccountId/workers/scripts/$WorkerName"
        $response = $client.PutAsync($uri, $multipart).GetAwaiter().GetResult()
        $body = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) { throw "Worker upload failed: $body" }
        $parsed = $body | ConvertFrom-Json
        if ($parsed.success -ne $true) {
            $details = @($parsed.errors | ForEach-Object { "[$($_.code)] $($_.message)" }) -join '; '
            throw "Worker upload failed: $details"
        }
    } finally {
        $multipart.Dispose()
        $client.Dispose()
    }
}

function Invoke-WorkerCertificateWorkaround {
    param(
        [string] $AccountId,
        [string] $ZoneId,
        [string] $ZoneName,
        [string] $WildcardHostname,
        [string] $SetupHex
    )
    $customHostname = $WildcardHostname.Substring(2)
    if (@(Get-DnsRecordsByName $ZoneId $customHostname).Count -gt 0) {
        throw "A DNS record already exists for $customHostname, which conflicts with the Worker certificate workaround."
    }

    Write-Host ''
    Write-Host 'The domain you chose: ' -NoNewline
    Write-Bold $WildcardHostname -NoNewline
    Write-Host ' requires Cloudflare Advanced Certificate Management, which can cost around ' -NoNewline
    Write-Bold '$10/month' -NoNewline
    Write-Host '.'
    Write-Host ''
    Write-Host 'FW can try a Cloudflare Workers provisioning workaround to generate the required wildcard certificate. This behavior is not guaranteed by Cloudflare and may took a while to complete. FW will verify that the requested wildcard certificate was actually created and ready to use.'
    Write-Host ''
    Write-Host "If you continue, you'll be redirected to Cloudflare again to approve the additional permission."
    Write-Host ''
    if (-not (Read-YesNo "Continue with the Worker approach? (Y/n)")) {
        throw 'Setup stopped. Create the required wildcard certificate with Advanced Certificate Manager, then run setup again.'
    }

    Write-Host ''
    [void](Invoke-CloudflareLogin (@($BaseScopes) + @($WorkersScope) | Select-Object -Unique))
    Write-Host ''
    Start-Status 'Checking scopes...'
    $existingWorkers = @(Get-AllPages "/accounts/$AccountId/workers/scripts")
    $existingWorkerDomains = @(Get-AllPages "/accounts/$AccountId/workers/domains")
    Complete-Status

    if ($existingWorkerDomains | Where-Object {
        (Test-ObjectProperty $_ 'hostname') -and $_.hostname -and $_.hostname.Equals($customHostname, [StringComparison]::OrdinalIgnoreCase)
    } | Select-Object -First 1) {
        throw "A Worker custom domain already exists for $customHostname. FW will not overwrite it."
    }
    $workerName = "proxy-fw-$SetupHex"
    if ($existingWorkers | Where-Object {
        ((Test-ObjectProperty $_ 'id') -and $_.id -eq $workerName) -or
            ((Test-ObjectProperty $_ 'name') -and $_.name -eq $workerName)
    } | Select-Object -First 1) {
        throw "The generated Worker name already exists: $workerName. Run setup again to generate a new identifier."
    }
    Start-Status "Creating Worker $workerName..."
    New-WorkerScript $AccountId $workerName
    $script:Journal.WorkerName = $workerName
    $script:Journal.WorkerCreated = $true
    Complete-Status

    Start-Status "Attaching Worker domain $customHostname..."
    $domain = Invoke-CfApi PUT "/accounts/$AccountId/workers/domains" @{
        hostname = $customHostname
        service = $workerName
        zone_id = $ZoneId
        zone_name = $ZoneName
    }
    $script:Journal.WorkerDomainId = [string]$domain.id
    Complete-Status

    for ($attempt = 1; $attempt -le $script:WorkerCertificateAttempts; $attempt++) {
        Start-Status "Checking domain ACM ($attempt/$($script:WorkerCertificateAttempts))..."
        Start-Sleep -Seconds $script:CertificatePollSeconds
        $pack = Find-WildcardCertificate (Get-CertificatePacks $ZoneId) $WildcardHostname
        if ($pack) {
            if (Test-CertificateFailed $pack) {
                Fail-Status
                throw "The Worker approach created a certificate pack in a failed state: $($pack.status)."
            }
            Complete-Status

            $acmStatus = ([string]$pack.status).ToUpperInvariant()
            $statusColor = if (Test-CertificateActive $pack) { 'Green' } else { 'Yellow' }
            Write-Host ''
            Write-Host 'ACM created successfully via Worker approach.' -ForegroundColor Green
            Write-Host 'ACM status: ' -NoNewline
            Write-Host $acmStatus -ForegroundColor $statusColor
            Write-Host ''

            return $pack
        }
        Complete-Status 'NOT READY'
    }
    throw 'The Worker domain was created, but Cloudflare did not create the requested wildcard certificate. Use Advanced Certificate Manager and try again.'
}

function Get-FreeTestPort {
    $firstPort = (New-Object Random).Next(9000, 10000)
    for ($offset = 0; $offset -lt 1000; $offset++) {
        $port = 9000 + (($firstPort - 9000 + $offset) % 1000)
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $port)
        try {
            $listener.Start()
            return $port
        } catch { } finally { try { $listener.Stop() } catch { } }
    }
    throw 'No unused test port is available between 9000 and 9999.'
}

function Write-Utf8FileAtomic {
    param([string] $Path, [string] $Content)
    $directory = Split-Path -Parent $Path
    $temp = Join-Path $directory ('.fw-' + [IO.Path]::GetRandomFileName())
    $script:Journal.TemporaryFiles.Add($temp)
    [IO.File]::WriteAllText($temp, $Content, [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temp -Destination $Path -Force
    [void]$script:Journal.TemporaryFiles.Remove($temp)
}

function Protect-CredentialsFile {
    param([Parameter(Mandatory)][string] $Path)
    try {
        $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $acl = Get-Acl -LiteralPath $Path
        $acl.SetAccessRuleProtection($true, $false)
        $rule = [Security.AccessControl.FileSystemAccessRule]::new(
            $currentSid,
            [Security.AccessControl.FileSystemRights]::FullControl,
            [Security.AccessControl.AccessControlType]::Allow
        )
        $acl.SetAccessRule($rule)
        Set-Acl -LiteralPath $Path -AclObject $acl
    } catch {
        throw "FW could not restrict access to the tunnel credentials file: $($_.Exception.Message)"
    }
}

function Install-Cloudflared {
    param([string] $Destination)
    if (Test-Path -LiteralPath $Destination) {
        $current = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($current -eq $script:CloudflaredSha256) { return }
        throw "An existing cloudflared.exe has an unexpected SHA-256 hash. FW will not overwrite it: $Destination"
    }
    $temp = "$Destination.download"
    $script:Journal.TemporaryFiles.Add($temp)
    Write-Host 'Cloudflared URL: ' -NoNewline
    Write-Host $script:CloudflaredUrl -ForegroundColor DarkGray
    Start-Status 'Downloading cloudflared.exe...'
    try {
        Invoke-WebRequest -Uri $script:CloudflaredUrl -OutFile $temp -UseBasicParsing
        $actual = (Get-FileHash -LiteralPath $temp -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $script:CloudflaredSha256) {
            throw "cloudflared.exe integrity check failed. Expected $($script:CloudflaredSha256), received $actual."
        }
        Move-Item -LiteralPath $temp -Destination $Destination -Force
        [void]$script:Journal.TemporaryFiles.Remove($temp)
        $script:Journal.CreatedFiles.Add($Destination)
        Complete-Status
    } catch {
        Fail-Status
        throw
    }
}

function Wait-TunnelConnected {
    param([string] $AccountId, [string] $TunnelId)
    for ($attempt = 1; $attempt -le 20; $attempt++) {
        $tunnel = Invoke-CfApi GET "/accounts/$AccountId/cfd_tunnel/$TunnelId"
        if (@('healthy','degraded') -contains [string]$tunnel.status) { return $tunnel }
        Start-Sleep -Seconds 3
    }
    throw 'The Cloudflare Tunnel did not connect in time.'
}

function Wait-CertificateActive {
    param([string] $ZoneId, [string] $Hostname)
    for ($attempt = 1; $attempt -le $script:CertificatePollAttempts; $attempt++) {
        Start-Status "Waiting for certificate ($attempt/$($script:CertificatePollAttempts))..."
        Start-Sleep -Seconds $script:CertificatePollSeconds
        $pack = Find-WildcardCertificate (Get-CertificatePacks $ZoneId) $Hostname
        if ($pack -and (Test-CertificateActive $pack)) {
            Complete-Status
            return $pack
        }
        if ($pack -and (Test-CertificateFailed $pack)) {
            Fail-Status
            throw "Certificate provisioning failed with status '$($pack.status)'."
        }
        Complete-Status 'PENDING'
    }
    throw 'The wildcard certificate was not active after 10 minutes.'
}

function Test-TunnelHttps {
    param([string] $Hostname)
    $curlPath = Join-Path $env:SystemRoot 'System32\curl.exe'
    if (-not (Test-Path -LiteralPath $curlPath)) {
        Write-Host '  Windows curl.exe is unavailable; skipping the external HTTPS test.' -ForegroundColor Yellow
        return
    }
    Start-Status "Resolving $Hostname with Cloudflare DNS..."
    $dohResponse = & $curlPath `
        --silent `
        --show-error `
        --fail-with-body `
        --connect-timeout 10 `
        --max-time 30 `
        --header 'Accept: application/dns-json' `
        "https://cloudflare-dns.com/dns-query?name=${Hostname}&type=A"
    if ($LASTEXITCODE -ne 0) { Fail-Status; throw "Failed to resolve $Hostname using Cloudflare DoH." }
    $dohJson = $dohResponse | ConvertFrom-Json
    if ([int]$dohJson.Status -ne 0) { Fail-Status; throw "Cloudflare DoH returned status $($dohJson.Status) for $Hostname." }
    $cloudflareIp = $dohJson.Answer | Where-Object { $_.type -eq 1 } | Select-Object -First 1 -ExpandProperty data
    if (-not $cloudflareIp) { Fail-Status; throw "Cloudflare DoH returned no A record for $Hostname." }
    Complete-Status

    Start-Status "Testing https://$Hostname/..."
    & $curlPath `
        --silent `
        --show-error `
        --fail-with-body `
        --connect-timeout 10 `
        --max-time 30 `
        --output NUL `
        --resolve "${Hostname}:443:${cloudflareIp}" `
        "https://${Hostname}/"
    if ($LASTEXITCODE -ne 0) { Fail-Status; throw "Tunnel test failed for https://${Hostname}/." }
    Complete-Status
    Write-Host ''
    Write-Host 'Tunnel test completed successfully.' -ForegroundColor Green
    Write-Host ''
}

function Revoke-OAuthTokensSilently {
    foreach ($token in @($script:AllTokens)) {
        try {
            Invoke-RestMethod -Uri $script:RevokeEndpoint -Method POST -UseBasicParsing `
                -ContentType 'application/x-www-form-urlencoded' `
                -Body (ConvertTo-FormBody @{ token = $token; client_id = $OAuthClientId }) | Out-Null
        } catch { }
    }
    $script:AccessToken = $null
    $script:AllTokens.Clear()
}

function Start-TestWebServer {
    param([Parameter(Mandatory)][int] $Port)

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $Port)
    try {
        $listener.Start()
    } catch {
        try { $listener.Stop() } catch { }
        throw "FW could not start the local test server on port $Port."
    }

    $stopEvent = [Threading.ManualResetEvent]::new($false)
    $runspace = [Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
    $runspace.Open()
    $worker = [Management.Automation.PowerShell]::Create()
    $worker.Runspace = $runspace
    $serverScript = {
        param($StopEvent, $Listener)
        while (-not $StopEvent.WaitOne(0)) {
            try {
                $accept = $Listener.AcceptTcpClientAsync()
                while (-not $accept.IsCompleted) {
                    if ($StopEvent.WaitOne(50)) { return }
                }
                $client = $accept.Result
                try {
                    $stream = $client.GetStream()
                    $stream.ReadTimeout = 5000
                    $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::ASCII, $false, 1024, $true)
                    try {
                        while (($line = $reader.ReadLine()) -ne $null -and $line.Length -gt 0) { }
                        $body = [Text.Encoding]::UTF8.GetBytes('ok')
                        $headers = "HTTP/1.1 200 OK`r`nContent-Type: text/plain; charset=utf-8`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n"
                        $headerBytes = [Text.Encoding]::ASCII.GetBytes($headers)
                        $stream.Write($headerBytes, 0, $headerBytes.Length)
                        $stream.Write($body, 0, $body.Length)
                        $stream.Flush()
                    } finally {
                        $reader.Dispose()
                    }
                } finally {
                    $client.Dispose()
                }
            } catch {
                if (-not $StopEvent.WaitOne(0)) { throw }
            }
        }
    }
    [void]$worker.AddScript($serverScript.ToString()).AddArgument($stopEvent).AddArgument($listener)

    return [pscustomobject]@{
        Listener = $listener
        StopEvent = $stopEvent
        Runspace = $runspace
        PowerShell = $worker
        Handle = $worker.BeginInvoke()
    }
}

function Stop-TestWebServer {
    param($Server)
    if (-not $Server) { return }
    try {
        [void]$Server.StopEvent.Set()
        $Server.Listener.Stop()
        try { [void]$Server.PowerShell.EndInvoke($Server.Handle) } catch { }
    } finally {
        $Server.PowerShell.Dispose()
        $Server.Runspace.Dispose()
        $Server.StopEvent.Dispose()
    }
}

function Stop-TestProcessTree {
    param($Process)
    if (-not $Process) { return }
    try {
        $taskkill = Join-Path $env:SystemRoot 'System32\taskkill.exe'
        if (Test-Path -LiteralPath $taskkill) {
            & $taskkill /PID $Process.Id /T /F 2>&1 | Out-Null
        } else {
            Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
        }
    } catch { }
}

function Invoke-Rollback {
    if ($script:Committed) { return }
    if ($script:Journal.TestProcess) {
        Stop-TestProcessTree $script:Journal.TestProcess
        $script:Journal.TestProcess = $null
    }
    if ($script:Journal.TestServer) {
        Stop-TestWebServer $script:Journal.TestServer
        $script:Journal.TestServer = $null
    }
    if ($script:AccessToken) {
        if ($script:Journal.DnsRecordId -and $script:Journal.ZoneId) {
            try { Invoke-CfApi DELETE "/zones/$($script:Journal.ZoneId)/dns_records/$($script:Journal.DnsRecordId)" | Out-Null } catch { }
        }
        if ($script:Journal.TunnelId -and $script:Journal.AccountId) {
            try { Invoke-CfApi DELETE "/accounts/$($script:Journal.AccountId)/cfd_tunnel/$($script:Journal.TunnelId)" | Out-Null } catch { }
        }
        if ($script:Journal.WorkerDomainId -and $script:Journal.AccountId) {
            try { Invoke-CfApi DELETE "/accounts/$($script:Journal.AccountId)/workers/domains/$($script:Journal.WorkerDomainId)" | Out-Null } catch { }
        }
        if ($script:Journal.WorkerCreated -and $script:Journal.WorkerName -and $script:Journal.AccountId) {
            try { Invoke-CfApi DELETE "/accounts/$($script:Journal.AccountId)/workers/scripts/$($script:Journal.WorkerName)" | Out-Null } catch { }
        }
    }
    foreach ($path in @($script:Journal.TemporaryFiles)) {
        try { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue } catch { }
    }
    foreach ($path in @($script:Journal.CreatedFiles)) {
        try { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue } catch { }
    }
}

function Initialize-CloudflaredArtifact {
    # PROCESSOR_ARCHITEW6432 reports the native OS architecture when this script
    # runs in a 32-bit PowerShell process on 64-bit Windows.
    $architecture = if (-not [string]::IsNullOrWhiteSpace($env:PROCESSOR_ARCHITEW6432)) {
        [string]$env:PROCESSOR_ARCHITEW6432
    } else {
        [string]$env:PROCESSOR_ARCHITECTURE
    }

    $fileName = switch ($architecture.ToUpperInvariant()) {
        { $_ -in @('X86', 'I386', '386') } {
            $script:CloudflaredSha256 = '11b6e4b2d306950bd87e7caa4deee8e80a32d71ffee555a96237a76651eeae4c'
            'cloudflared-windows-386.exe'
            break
        }
        'AMD64' {
            $script:CloudflaredSha256 = '2837888cc0f5d58f15b6dc478376de90b4d3ba5241c7947455d1e0a0df429712'
            'cloudflared-windows-amd64.exe'
            break
        }
        'ARM64' {
            throw 'FW setup does not currently support Windows on ARM64 due to cloudflared missing support on this version.'
        }
        default {
            throw "FW setup does not support the detected Windows architecture: $architecture."
        }
    }

    $script:CloudflaredUrl = "https://github.com/cloudflare/cloudflared/releases/download/$($script:CloudflaredVersion)/$fileName"
}

function Assert-Configuration {
    foreach ($value in @($OAuthClientId) + @($BaseScopes) + @($WorkersScope)) {
        if ([string]::IsNullOrWhiteSpace($value) -or $value -like 'REPLACE_*') {
            throw 'This setup release is not configured: its Cloudflare OAuth client ID or scope values have not been replaced.'
        }
    }
    Initialize-CloudflaredArtifact
}

function Invoke-Setup {
    Clear-Host
    Write-Host 'Welcome to ' -NoNewline
    Write-Bold 'FW Automated Setup' -ForegroundColor Blue -NoNewline
    Write-Host '. This wizard will help you to:'
    Write-Host ''
    Write-Host ' •' -ForegroundColor DarkGray -NoNewline
    Write-Host ' Connect to your ' -NoNewline
    Write-Bold 'Cloudflare account' -ForegroundColor Blue
    Write-Host ' •' -ForegroundColor DarkGray -NoNewline
    Write-Host ' Configure a wildcard domain'
    Write-Host ' •' -ForegroundColor DarkGray -NoNewline
    Write-Host ' Create a ' -NoNewline
    Write-Bold 'Cloudflare Tunnel' -ForegroundColor Green
    Write-Host ' •' -ForegroundColor DarkGray -NoNewline
    Write-Host ' Download the official ' -NoNewline
    Write-Bold 'cloudflared' -ForegroundColor Yellow -NoNewline
    Write-Host ' binary'
    Write-Host ' •' -ForegroundColor DarkGray -NoNewline
    Write-Host ' Verify that your tunnel is working'
    Write-Host ''
    Write-Host "You'll be asked to authorize with Cloudflare and configure your domain during setup."
    Write-Host 'Additional configuration prompts may appear depending on your selections.'
    Write-Host ''
    Write-Host 'To start the setup process, press enter.'
    [void](Read-Host)
    Assert-Configuration
    $setupHex = New-RandomHex

    if ($FWPath -notmatch '^(?:[A-Za-z]:[\\/]|\\\\)') { throw 'FWPath must be an absolute path.' }
    $FWPath = [IO.Path]::GetFullPath($FWPath)
    if (-not (Test-Path -LiteralPath $FWPath -PathType Leaf)) { throw "FW executable was not found: $FWPath" }
    $root = [IO.Path]::GetDirectoryName($FWPath)
    if ([string]::IsNullOrWhiteSpace($root)) { throw "FW executable directory could not be determined: $FWPath" }
    $cfDirectory = Join-Path $root 'cf'
    New-Item -ItemType Directory -Path $cfDirectory -Force | Out-Null
    $probe = Join-Path $cfDirectory ('.write-' + [Guid]::NewGuid().ToString('N'))
    try { [IO.File]::WriteAllText($probe, 'test'); Remove-Item -LiteralPath $probe -Force } catch { throw "FW cannot write to $cfDirectory." }
    $configPath = Join-Path $cfDirectory 'config.yml'
    if (Test-Path -LiteralPath $configPath) { throw "FW will not overwrite the existing configuration: $configPath" }

    Write-Host ''
    [void](Invoke-CloudflareLogin $BaseScopes)
    Start-Status 'Checking scopes...'
    $accounts = @(Get-AllPages '/accounts')
    Complete-Status

    $account = Select-Account $accounts
    $script:Journal.AccountId = [string]$account.id

    Write-Host ''
    Write-Host "Cloudflare account: $($account.name)"
    $wildcardHostname = Read-WildcardHostname

    Start-Status 'Checking domain availability...'
    $zone = Find-ZoneForHostname $wildcardHostname $account.id
    if (-not $zone) { Fail-Status; throw 'No active Cloudflare zone in the selected account matches that domain.' }
    $script:Journal.ZoneId = [string]$zone.id
    $relativeBase = $wildcardHostname.Substring(2, $wildcardHostname.Length - 2 - $zone.name.Length).TrimEnd('.')
    $relativeLabels = if ($relativeBase) { @($relativeBase.Split('.')).Count } else { 0 }
    if ($relativeLabels -gt 2) { Fail-Status; throw 'The wildcard domain may be at most three levels deep relative to its Cloudflare zone.' }
    if (@(Get-DnsRecordsByName $zone.id $wildcardHostname).Count -gt 0) { Fail-Status; throw "A DNS record already exists for $wildcardHostname. FW will not overwrite it." }
    Complete-Status

    Start-Status 'Checking domain ACM...'
    $certificate = Find-WildcardCertificate (Get-CertificatePacks $zone.id) $wildcardHostname
    if ($certificate -and (Test-CertificateFailed $certificate)) {
        Fail-Status
        throw "The existing certificate for $wildcardHostname is in a failed state: $($certificate.status)."
    }
    Complete-Status $(if ($certificate) { ([string]$certificate.status).ToUpperInvariant() } else { 'NOT FOUND' })

    # Universal SSL covers only one level below the zone. Two- and three-level
    # wildcard hostnames require an exact certificate pack host match.
    $requiresWorkerCertificate = $relativeLabels -in @(1, 2)
    $cloudflaredPath = Join-Path $cfDirectory 'cloudflared.exe'
    if ($requiresWorkerCertificate -and -not $certificate) {
        $certificate = Invoke-WorkerCertificateWorkaround $account.id $zone.id $zone.name $wildcardHostname $setupHex
    }
    Install-Cloudflared $cloudflaredPath

    $existingTunnels = @(Get-AllPages "/accounts/$($account.id)/cfd_tunnel?is_deleted=false")
    $tunnelName = "tunnel-fw-$setupHex"
    if ($existingTunnels | Where-Object { $_.name -eq $tunnelName } | Select-Object -First 1) {
        throw "The generated tunnel name already exists: $tunnelName. Run setup again to generate a new identifier."
    }
    $tunnelSecret = New-TunnelSecret
    Start-Status 'Creating tunnel...'
    try {
        $tunnelResponse = Invoke-CfApi -Method POST -Path "/accounts/$($account.id)/cfd_tunnel" -Body @{
            name = $tunnelName
            config_src = 'local'
            tunnel_secret = $tunnelSecret
        } -ReturnEnvelope
        $tunnel = $tunnelResponse.result
        if ($null -eq $tunnel -or -not (Test-ObjectProperty $tunnel 'id') -or -not $tunnel.id) {
            throw 'Cloudflare reported tunnel creation success but returned no tunnel ID.'
        }
        $tunnelId = [string]$tunnel.id
        $script:Journal.TunnelId = $tunnelId
    } catch {
        Fail-Status
        throw
    }
    Complete-Status

    $credentialsPath = Join-Path $cfDirectory "credentials-$tunnelId.json"
    $credentials = [ordered]@{
        AccountTag = [string]$account.id
        TunnelSecret = $tunnelSecret
        TunnelID = $tunnelId
    } | ConvertTo-Json -Depth 5
    Write-Utf8FileAtomic $credentialsPath $credentials
    $script:Journal.CreatedFiles.Add($credentialsPath)
    Protect-CredentialsFile $credentialsPath

    Start-Status 'Creating tunnel DNS record...'
    try {
        $dnsResponse = Invoke-CfApi -Method POST -Path "/zones/$($zone.id)/dns_records" -Body @{
            type = 'CNAME'
            name = $wildcardHostname
            content = "$tunnelId.cfargotunnel.com"
            proxied = $true
            ttl = 1
        } -ReturnEnvelope
        $dnsRecord = $dnsResponse.result
        if ($null -eq $dnsRecord -or -not (Test-ObjectProperty $dnsRecord 'id') -or -not $dnsRecord.id) {
            throw 'Cloudflare reported DNS record creation success but returned no record ID.'
        }
        $script:Journal.DnsRecordId = [string]$dnsRecord.id
    } catch {
        Fail-Status
        throw
    }
    Complete-Status

    $testPort = Get-FreeTestPort
    $testSlug = "setup-$setupHex"
    $testHostname = "$testSlug.$($wildcardHostname.Substring(2))"
    $yamlCredentials = $credentialsPath.Replace('\','/').Replace('"','\"')
    $config = @"
tunnel: "$tunnelId"
credentials-file: "$yamlCredentials"

ingress:
  - hostname: "$wildcardHostname"
    service: "http://127.0.0.1:$testPort"
  - service: "http_status:404"
"@
    Write-Utf8FileAtomic $configPath $config
    $script:Journal.CreatedFiles.Add($configPath)

    Start-Status 'Validating tunnel configuration...'
    & $cloudflaredPath tunnel --config $configPath ingress validate 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail-Status; throw 'cloudflared rejected the generated config.yml.' }
    Complete-Status

    Write-Host ''
    Write-Host 'Tunnel & DNS created successfully.' -ForegroundColor Green
    Write-Host ''

    Start-Status "Starting local test server on port $testPort..."
    try {
        $testServer = Start-TestWebServer $testPort
        $script:Journal.TestServer = $testServer
        $testResponse = Invoke-WebRequest -Uri "http://127.0.0.1:$testPort/" -UseBasicParsing -TimeoutSec 5
        if ([int]$testResponse.StatusCode -ne 200 -or ([string]$testResponse.Content).Trim() -ne 'ok') {
            throw 'The local test server returned an unexpected response.'
        }
    } catch {
        Fail-Status
        throw
    }
    Complete-Status

    Start-Status "Starting FW on port $testPort with slug $testSlug..."
    $process = Start-Process -FilePath $FWPath -ArgumentList @('start', [string]$testPort, '--slug', $testSlug) -WorkingDirectory $root -PassThru -WindowStyle Hidden
    $script:Journal.TestProcess = $process
    Start-Sleep -Seconds 3
    if ($process.HasExited) { Fail-Status; throw "FW start exited with code $($process.ExitCode)." }
    Complete-Status

    Start-Status 'Waiting for tunnel connection...'
    [void](Wait-TunnelConnected $account.id $tunnelId)
    Complete-Status

    [void](Wait-CertificateActive $zone.id $wildcardHostname)
    Test-TunnelHttps $testHostname

    Start-Status 'Finishing setup...'
    Stop-TestProcessTree $process
    $script:Journal.TestProcess = $null
    Stop-TestWebServer $testServer
    $script:Journal.TestServer = $null
    $script:Committed = $true
    Revoke-OAuthTokensSilently
    Complete-Status

    Write-Host ''
    Write-Bold 'FW setup completed successfully.'
    Write-Host "Tunnel: $tunnelName"
    Write-Host "Domain: $wildcardHostname"
    Write-Host "Configuration: $configPath"
    Write-Host ''
    Write-Host 'Press Enter to close.'
    [void](Read-Host)
}

try {
    Invoke-Setup
    exit 0
} catch {
    Stop-StatusAnimation
    if (-not $script:Committed) {
        try { Invoke-Rollback } catch { }
    }
    Revoke-OAuthTokensSilently
    Write-Host ''
    Write-ErrorLine $_.Exception.Message
    exit 1
}
