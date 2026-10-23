# LetShare watchdog verification. Run in an ELEVATED PowerShell.
#
# Proves three things the installer cannot: the installed restart budget is 999,
# the watchdog is a true no-op while the daemon is alive, and it repairs the
# stack after the daemon is killed (including orphaned children).

#Requires -RunAsAdministrator
param(
    [string]$Root = (Split-Path -Parent $PSScriptRoot)
)
$ErrorActionPreference = 'Continue'

$root = $Root
Set-Location -LiteralPath $root

$exe = Join-Path $root 'target\release\letsshare-watchdog.exe'
$cli = Join-Path $root 'target\release\letsshare-tier2.exe'
$log = Join-Path $env:TEMP 'letsshare-watchdog.log'

function Snapshot {
    Get-Process letsshare-daemon, wfd, dhcpd, dnsrescue -ErrorAction SilentlyContinue |
        Sort-Object ProcessName |
        ForEach-Object { $_.ProcessName + '=' + $_.Id }
}

Write-Host '== 1. installed settings ==' -ForegroundColor Cyan
$t = Get-ScheduledTask -TaskName LetShare
Write-Host ('   RestartCount       : ' + $t.Settings.RestartCount)
Write-Host ('   RestartInterval    : ' + $t.Settings.RestartInterval)
Write-Host ('   ExecutionTimeLimit : ' + $t.Settings.ExecutionTimeLimit)
$w = Get-ScheduledTask -TaskName LetShare-Watchdog
Write-Host ('   watchdog triggers  : ' + (@($w.Triggers).Count))
$rep = @($w.Triggers | Where-Object { $_.Repetition -and $_.Repetition.Interval } | Select-Object -First 1)
if ($rep.Count -gt 0) {
    Write-Host ('   watchdog repeats   : every ' + $rep[0].Repetition.Interval + ' for ' + $rep[0].Repetition.Duration)
} else {
    Write-Host '   watchdog repeats   : NONE (failed)' -ForegroundColor Red
}

Write-Host '== 2. healthy watchdog must change nothing ==' -ForegroundColor Cyan
Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
$before = @(Snapshot)
$p = Start-Process -FilePath $exe -Wait -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2
$after = @(Snapshot)
Write-Host ('   exit code      : ' + $p.ExitCode)
Write-Host ('   pids unchanged : ' + (($before -join ',') -eq ($after -join ',')))
Write-Host ('   log written    : ' + (Test-Path -LiteralPath $log))
Write-Host ('   before : ' + ($before -join ' '))
Write-Host ('   after  : ' + ($after -join ' '))

Write-Host '== 3. kill the daemon, let the SCHEDULED watchdog repair it ==' -ForegroundColor Cyan
Stop-ScheduledTask -TaskName LetShare -ErrorAction SilentlyContinue
Start-Sleep -Seconds 3
Write-Host ('   after stop  : ' + ((@(Snapshot)) -join ' '))
# Start the registered task, not the exe: this proves the principal, the
# repetition trigger and the elevation all work as installed.
Start-ScheduledTask -TaskName LetShare-Watchdog -ErrorAction SilentlyContinue
Start-Sleep -Seconds 16
Write-Host ('   after repair: ' + ((@(Snapshot)) -join ' '))
$info = Get-ScheduledTaskInfo -TaskName LetShare-Watchdog -ErrorAction SilentlyContinue
Write-Host ('   watchdog last result : ' + $info.LastTaskResult + '  next run: ' + $info.NextRunTime)
Write-Host '   status:'
& $cli status
Write-Host '   watchdog log:'
Get-Content -LiteralPath $log -ErrorAction SilentlyContinue

Write-Host '== 4. ports and phone ==' -ForegroundColor Cyan
netstat -ano -p UDP | Select-String -Pattern ':67\s' | ForEach-Object { Write-Host ('   ' + $_.Line.Trim()) }
$ap = Get-NetAdapter -ErrorAction SilentlyContinue |
    Where-Object { $_.InterfaceDescription -like '*Wi-Fi Direct*' -and $_.Status -eq 'Up' } |
    Select-Object -First 1
if ($ap) {
    Get-NetNeighbor -InterfaceIndex $ap.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object { $_.IPAddress -like '192.168.50.*' -and $_.IPAddress -notlike '*.255' } |
        Select-Object IPAddress, LinkLayerAddress, State | Format-Table -AutoSize
}

Write-Host '== watchdog verification done ==' -ForegroundColor Green
