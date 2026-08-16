$ErrorActionPreference = 'Stop'

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)]
        [string] $FilePath,

        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]] $ArgumentList
    )

    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $FilePath $($ArgumentList -join ' ')"
    }
}

$operatingSystem = (& docker info --format '{{.OSType}}').Trim()
if ($LASTEXITCODE -ne 0 -or $operatingSystem -ne 'linux') {
    throw 'Docker must be running with the Linux engine.'
}

$localStateDirectory = Join-Path $PSScriptRoot '..\.local'
$localCaCertificate = Join-Path $localStateDirectory 'docker-ca.cer'
New-Item -ItemType Directory -Force -Path $localStateDirectory | Out-Null
if (-not [string]::IsNullOrWhiteSpace($env:DOCKER_BUILD_CA_CERT)) {
    Copy-Item -LiteralPath $env:DOCKER_BUILD_CA_CERT -Destination $localCaCertificate -Force
}
elseif (-not (Test-Path -LiteralPath $localCaCertificate)) {
    New-Item -ItemType File -Path $localCaCertificate | Out-Null
}
$cacheRevision = (Get-FileHash -Algorithm SHA256 -LiteralPath $localCaCertificate).Hash.ToLowerInvariant()
$previousCacheRevision = $env:CACHE_REVISION

try {
    $env:CACHE_REVISION = $cacheRevision
    Invoke-Checked docker buildx bake verify --progress=plain

    $composeFile = Join-Path $PSScriptRoot '..\docker\compose.validation.yml'
    if (Test-Path -LiteralPath $composeFile) {
        Invoke-Checked docker compose --file $composeFile up --build --abort-on-container-exit --exit-code-from integration
    }
}
finally {
    if ($null -eq $previousCacheRevision) {
        Remove-Item Env:CACHE_REVISION -ErrorAction SilentlyContinue
    }
    else {
        $env:CACHE_REVISION = $previousCacheRevision
    }

    if ($null -ne $composeFile -and (Test-Path -LiteralPath $composeFile)) {
        & docker compose --file $composeFile down --volumes --remove-orphans
    }
}
