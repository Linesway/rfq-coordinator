param(
    [ValidateRange(1, 20)]
    [int]$Requirement
)

$ErrorActionPreference = 'Stop'
$arguments = @{}
if ($Requirement) {
    $arguments.Requirement = $Requirement
}
& (Join-Path $PSScriptRoot '../server/tests/run-tests.ps1') @arguments
