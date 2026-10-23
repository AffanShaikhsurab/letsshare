# Deploy the current build and prove the watchdog works. Run ELEVATED.
#
# One elevated session instead of two: clean-restart stops everything, rebuilds,
# reinstalls both tasks and starts them; verify-watchdog then checks the restart
# budget, the healthy no-op path, and the repair path.

#Requires -RunAsAdministrator
param(
    [string]$Root = (Split-Path -Parent $PSScriptRoot)
)
$ErrorActionPreference = 'Continue'

$root = $Root

& (Join-Path $root 'scripts\clean-restart.ps1') -Root $root
Write-Host ''
& (Join-Path $root 'scripts\verify-watchdog.ps1') -Root $root

Write-Host '== deploy + verify complete ==' -ForegroundColor Green
