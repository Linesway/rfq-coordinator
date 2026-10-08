$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot '../../run-tests.ps1') -Requirement 3
