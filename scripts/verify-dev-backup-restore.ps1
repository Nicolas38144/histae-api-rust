[CmdletBinding()]
param(
    [string]$Container = 'histae-rust-dev-postgres-1',
    [string]$SourceDatabase = 'histae-dev',
    [string]$DatabaseUser = 'postgres'
)

$ErrorActionPreference = 'Stop'

if ($SourceDatabase -ne 'histae-dev') {
    throw 'This verification is restricted to the local histae-dev database.'
}
if ($Container -notmatch '^histae-rust-dev-postgres-[0-9]+$') {
    throw 'This verification is restricted to the Rust development PostgreSQL container.'
}

$suffix = [guid]::NewGuid().ToString('N')
$restoreDatabase = "histae_s29_restore_$suffix"
$archive = "/tmp/histae_s29_$suffix.dump"
$created = $false

function Invoke-ContainerCommand {
    param([Parameter(Mandatory)][string[]]$CommandArguments)
    & wsl.exe docker exec $Container @CommandArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Docker command failed with exit code $LASTEXITCODE"
    }
}

try {
    Invoke-ContainerCommand -CommandArguments @('pg_dump', '-U', $DatabaseUser, '-d', $SourceDatabase, '-Fc', '-f', $archive)
    Invoke-ContainerCommand -CommandArguments @('createdb', '-U', $DatabaseUser, $restoreDatabase)
    $created = $true
    Invoke-ContainerCommand -CommandArguments @('pg_restore', '-U', $DatabaseUser, '-d', $restoreDatabase, '--exit-on-error', $archive)

    $query = "SELECT version || ':' || checksum FROM schema_migrations ORDER BY version"
    $sourceHistory = (& wsl.exe docker exec $Container psql -U $DatabaseUser -d $SourceDatabase -Atc $query) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Could not read source migration history' }
    $restoredHistory = (& wsl.exe docker exec $Container psql -U $DatabaseUser -d $restoreDatabase -Atc $query) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Could not read restored migration history' }
    if ($sourceHistory -ne $restoredHistory -or [string]::IsNullOrWhiteSpace($sourceHistory)) {
        throw 'The restored migration history differs from the source database.'
    }

    $tableQuery = "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'"
    $sourceTables = (& wsl.exe docker exec $Container psql -U $DatabaseUser -d $SourceDatabase -Atc $tableQuery).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not count source tables' }
    $restoredTables = (& wsl.exe docker exec $Container psql -U $DatabaseUser -d $restoreDatabase -Atc $tableQuery).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not count restored tables' }
    if ($sourceTables -ne $restoredTables) {
        throw "Restored table count differs: source=$sourceTables restored=$restoredTables"
    }

    Write-Output "S29 backup/restore verification passed ($sourceTables public tables)."
}
finally {
    if ($created) {
        & wsl.exe docker exec $Container dropdb -U $DatabaseUser --if-exists $restoreDatabase | Out-Null
    }
    & wsl.exe docker exec $Container rm -f $archive | Out-Null
}
