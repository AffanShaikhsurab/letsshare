# LetShare clean restart. Run in an ELEVATED PowerShell.
#
# Stops the task, kills every LetShare process (they run elevated, so an
# unelevated shell cannot touch them), kills the lingering elevated shells that
# earlier redeploys left behind, clears stale neighbour entries, rotates the
# logs so the next run is unambiguous, rebuilds, and starts the task again.

#Requires -RunAsAdministrator
param(
    [string]$Root = (Split-Path -Parent $PSScriptRoot)
)
$ErrorActionPreference = 'Continue'

$root = $Root
Set-Location -LiteralPath $root

Write-Host '== 1. stop the scheduled task ==' -ForegroundColor Cyan
Stop-ScheduledTask -TaskName LetShare -ErrorAction SilentlyContinue

Write-Host '== 2. kill every letsshare process ==' -ForegroundColor Cyan
$names = @('letsshare-watchdog', 'letsshare-daemon', 'letsshare-tier2', 'wfd', 'dhcpd', 'dnsrescue', 'dhcpprobe', 'adapters', 'soak')
foreach ($n in $names) {
    Get-Process -Name $n -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host ('   kill ' + $_.ProcessName + ' pid=' + $_.Id)
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
}

Write-Host '== 3. kill lingering elevated shells from earlier runs ==' -ForegroundColor Cyan
Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" -ErrorAction SilentlyContinue | Where-Object {
    $_.ProcessId -ne $PID -and $_.CommandLine -and (
        $_.CommandLine -like '*letsshare-redeploy*' -or
        $_.CommandLine -like '*letsshare-takeover*'
    )
} | ForEach-Object {
    Write-Host ('   kill shell pid=' + $_.ProcessId)
    Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
}

Start-Sleep -Seconds 3

Write-Host '== 4. ports after the kill (67 and 53) ==' -ForegroundColor Cyan
netstat -ano -p UDP | Select-String -Pattern ':67\s|:53\s' | ForEach-Object { Write-Host ('   ' + $_.Line.Trim()) }

Write-Host '== 5. clear stale neighbour entries ==' -ForegroundColor Cyan
netsh interface ip delete arpcache | Out-Null

Write-Host '== 6. rotate logs for a clean run ==' -ForegroundColor Cyan
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
Get-ChildItem $env:TEMP -Filter 'letsshare-*.log' -ErrorAction SilentlyContinue | ForEach-Object {
    Move-Item -LiteralPath $_.FullName -Destination ($_.FullName + '.' + $stamp) -Force -ErrorAction SilentlyContinue
}
$leases = Join-Path $env:TEMP 'letsshare-leases.txt'
if (Test-Path -LiteralPath $leases) {
    Move-Item -LiteralPath $leases -Destination ($leases + '.' + $stamp) -Force -ErrorAction SilentlyContinue
}

Write-Host '== 7. build release ==' -ForegroundColor Cyan
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
& $cargo build --release -p letsshare-probe 2>&1 | Select-Object -Last 3

if (-not (Test-Path -LiteralPath (Join-Path $root 'target\release\letsshare-daemon.exe'))) {
    Write-Host '   BUILD FAILED: daemon missing' -ForegroundColor Red
    exit 1
}

Write-Host '== 8. reinstall and start the task ==' -ForegroundColor Cyan
& (Join-Path $root 'scripts\install-autostart.ps1') | Out-Null
Start-ScheduledTask -TaskName LetShare -ErrorAction SilentlyContinue
Start-Sleep -Seconds 18

Write-Host '== 9. status ==' -ForegroundColor Cyan
& (Join-Path $root 'target\release\letsshare-tier2.exe') status

Write-Host '== 10. binds ==' -ForegroundColor Cyan
netstat -ano -p UDP | Select-String -Pattern ':67\s|:53\s' | ForEach-Object { Write-Host ('   ' + $_.Line.Trim()) }

Write-Host '== 11. processes ==' -ForegroundColor Cyan
Get-Process letsshare-daemon, wfd, dhcpd, dnsrescue -ErrorAction SilentlyContinue |
    Select-Object ProcessName, Id, StartTime | Format-Table -AutoSize

Write-Host '== clean restart done ==' -ForegroundColor Green
