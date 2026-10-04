[CmdletBinding()]
param(
    [ValidatePattern('^https?://(localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$')]
    [string]$BaseUrl = 'http://127.0.0.1:8080'
)

$ErrorActionPreference = 'Stop'

function Invoke-SmokeRequest {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('GET', 'POST')]
        [string]$Method,
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [int]$ExpectedStatus,
        [string]$Body
    )

    $parameters = @{
        Uri = "$BaseUrl$Path"
        Method = $Method
        SkipHttpErrorCheck = $true
        TimeoutSec = 10
    }
    if ($PSBoundParameters.ContainsKey('Body')) {
        $parameters.ContentType = 'application/json'
        $parameters.Body = $Body
    }
    $response = Invoke-WebRequest @parameters
    if ([int]$response.StatusCode -ne $ExpectedStatus) {
        throw "Unexpected HTTP status for $Method ${Path}: $($response.StatusCode), expected $ExpectedStatus"
    }
    $response
}

$live = Invoke-SmokeRequest -Method GET -Path '/health/live' -ExpectedStatus 200
if (($live.Content | ConvertFrom-Json).status -ne 'ok') {
    throw 'Liveness response is invalid'
}

$ready = Invoke-SmokeRequest -Method GET -Path '/health/ready' -ExpectedStatus 200
if (($ready.Content | ConvertFrom-Json).status -ne 'ready') {
    throw 'Readiness response is invalid'
}

$protected = Invoke-SmokeRequest -Method GET -Path '/api/users/me' -ExpectedStatus 401
if (($protected.Content | ConvertFrom-Json).error.code -ne 'authentication_required') {
    throw 'Mobile authentication error contract is invalid'
}

$adminId = [guid]::NewGuid().ToString()
$admin = Invoke-SmokeRequest -Method GET -Path "/api/admin/users/$adminId" -ExpectedStatus 401
if (($admin.Content | ConvertFrom-Json).error.code -ne 'admin_session_invalid') {
    throw 'Admin authentication error contract is invalid'
}

$invalid = Invoke-SmokeRequest -Method POST -Path '/api/auth/otp/send' -ExpectedStatus 400 -Body '{}'
if (($invalid.Content | ConvertFrom-Json).error.code -ne 'invalid_request_body') {
    throw 'OTP validation error contract is invalid'
}

$missing = Invoke-SmokeRequest -Method GET -Path '/api/s29-route-that-does-not-exist' -ExpectedStatus 404
if (($missing.Content | ConvertFrom-Json).error.code -ne 'route_not_found') {
    throw 'Not-found error contract is invalid'
}

Write-Output 'S29 API smoke passed.'
