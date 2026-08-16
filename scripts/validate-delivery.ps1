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

Invoke-Checked docker buildx bake verify --progress=plain

$composeFile = Join-Path $PSScriptRoot '..\docker\compose.validation.yml'
if (Test-Path -LiteralPath $composeFile) {
    try {
        Invoke-Checked docker compose --file $composeFile up --build --abort-on-container-exit --exit-code-from integration
    }
    finally {
        & docker compose --file $composeFile down --volumes --remove-orphans
    }
}
