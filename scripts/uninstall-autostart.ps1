# LetShare autostart uninstaller. Run in an ELEVATED PowerShell.
#
# Removes the "LetShare" and "LetShare-Watchdog" scheduled tasks and stops the
# tier-2 supervisor if it is running. The watchdog goes first so it cannot
# start the supervisor again while it is being torn down. Safe to re-run.

#Requires -RunAsAdministrator
$ErrorActionPreference = 'Continue'

$taskNames = @('LetShare-Watchdog', 'LetShare')
$installDir = Join-Path $env:ProgramFiles 'LetShare'
$changed = 0
$failed = $false

Write-Host '== remove the scheduled tasks ==' -ForegroundColor Cyan

foreach ($name in $taskNames) {
    $task = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    if (-not $task) {
        Write-Host ('   no task named ' + $name)
        continue
    }
    Write-Host ('   found task ' + $name + ' (state: ' + $task.State + ')')
    if ($task.State -eq 'Running') {
        Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
        Write-Host ('   stopped the running task ' + $name)
        Start-Sleep -Seconds 1
    }
    Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
    $after = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    if ($after) {
        Write-Host ('   FAILED to remove task ' + $name) -ForegroundColor Red
        Write-Host '   The task is still registered; check Task Scheduler.' -ForegroundColor Red
        $failed = $true
        continue
    }
    Write-Host ('   unregistered task ' + $name)
    $changed++
}

Write-Host '== stop the supervisor and its children ==' -ForegroundColor Cyan

# Killing the daemon does not kill its children on Windows; they hold the AP and
# ports 67/53. Uninstall has to clear them or they outlive the app.
$procNames = @('letsshare-watchdog', 'letsshare-daemon', 'letsshare-tier2', 'wfd', 'dhcpd', 'dnsrescue')
foreach ($procName in $procNames) {
    $procs = @(Get-Process -Name $procName -ErrorAction SilentlyContinue)
    if ($procs.Count -gt 0) {
        $ids = ($procs | ForEach-Object { $_.Id }) -join ', '
        $procs | Stop-Process -Force -ErrorAction SilentlyContinue
        Write-Host ('   stopped ' + $procName + ' (pid ' + $ids + ')')
        $changed++
    }
}

Write-Host '== remove the network state ==' -ForegroundColor Cyan

if (Get-NetNat -Name 'LetShare' -ErrorAction SilentlyContinue) {
    Remove-NetNat -Name 'LetShare' -Confirm:$false -ErrorAction SilentlyContinue
    Write-Host '   removed WinNat LetShare'
    $changed++
} else {
    Write-Host '   no WinNat named LetShare'
}

# The AP address would otherwise stay on a stale virtual adapter.
Get-NetAdapter -ErrorAction SilentlyContinue |
    Where-Object { $_.InterfaceDescription -like '*Wi-Fi Direct*' } |
    ForEach-Object {
        Get-NetIPAddress -InterfaceIndex $_.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue |
            Where-Object { $_.IPAddress -eq '192.168.50.1' } |
            ForEach-Object {
                Remove-NetIPAddress -InterfaceIndex $_.InterfaceIndex -IPAddress $_.IPAddress -Confirm:$false -ErrorAction SilentlyContinue
                Write-Host ('   removed ' + $_.IPAddress + ' from ' + $_.InterfaceAlias)
                $changed++
            }
    }

Write-Host '== remove the installed binaries ==' -ForegroundColor Cyan

if (Test-Path -LiteralPath $installDir) {
    Start-Sleep -Seconds 1
    Remove-Item -LiteralPath $installDir -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $installDir) {
        Write-Host ('   could not remove ' + $installDir + ' (files still in use?)') -ForegroundColor Yellow
    } else {
        Write-Host ('   removed ' + $installDir)
        $changed++
    }
} else {
    Write-Host ('   no ' + $installDir)
}

if ($failed) {
    exit 1
}

Write-Host ''
if ($changed -gt 0) {
    Write-Host 'LetShare autostart removed.' -ForegroundColor Green
} else {
    Write-Host 'Nothing to do; autostart was not installed.' -ForegroundColor Yellow
}
Write-Host '  Neither the supervisor nor the watchdog will start at logon.'
