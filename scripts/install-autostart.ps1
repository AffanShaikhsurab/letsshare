# LetShare autostart installer. Run in an ELEVATED PowerShell.
#
# Registers two scheduled tasks:
#   LetShare           the tier-2 supervisor at logon of the current user,
#                      elevated, hidden, restarted on failure.
#   LetShare-Watchdog  a windowless watchdog that starts the first task again
#                      when no supervisor process is running.
#
# The supervisor needs Administrator: it configures a network adapter address
# and owns a WinNat instance. The watchdog needs Administrator too, because
# starting an elevated task is not something a standard user may do.
#
# Two layers exist because Task Scheduler counts restarts against a lifetime
# budget: when it runs out, the task stays down for good. RestartCount is set
# high and the watchdog ticks every 5 minutes, so a crash-loop still heals
# without a human at the keyboard.
#
# Safe to re-run; existing tasks are replaced.

#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'

$taskName = 'LetShare'
$watchdogTaskName = 'LetShare-Watchdog'
$root = Split-Path -Parent $PSScriptRoot
$sourceDir = Join-Path $root 'target\release'

# The task runs elevated at logon. If it executed binaries from the checkout,
# any process running as this user could replace them and inherit elevation at
# the next logon. Program Files is writable only by administrators, so that is
# where the installed copy lives.
$installDir = Join-Path $env:ProgramFiles 'LetShare'
$binaries = @(
    'letsshare-daemon.exe',
    'letsshare-watchdog.exe',
    'wfd.exe',
    'dhcpd.exe',
    'dnsrescue.exe'
)

foreach ($name in $binaries) {
    $src = Join-Path $sourceDir $name
    if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
        Write-Host ('ERROR: missing ' + $src) -ForegroundColor Red
        Write-Host 'Build it first: cargo build --release -p letsshare-probe' -ForegroundColor Red
        exit 1
    }
}

$exe = Join-Path $installDir 'letsshare-daemon.exe'
$watchdogExe = Join-Path $installDir 'letsshare-watchdog.exe'

$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name

Write-Host '== remove any existing tasks ==' -ForegroundColor Cyan

foreach ($name in @($taskName, $watchdogTaskName)) {
    $existing = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    if (-not $existing) {
        continue
    }
    if ($existing.State -eq 'Running') {
        Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 1
    }
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
    Write-Host ('   replaced the existing task ' + $name)
}

Write-Host '== install the binaries ==' -ForegroundColor Cyan

# Stop the old stack first: its executables are locked while running, and its
# children outlive the daemon and would keep holding ports 67 and 53.
Get-Process letsshare-daemon, letsshare-watchdog, wfd, dhcpd, dnsrescue -ErrorAction SilentlyContinue |
    Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

New-Item -ItemType Directory -Path $installDir -Force | Out-Null
foreach ($name in $binaries) {
    Copy-Item -LiteralPath (Join-Path $sourceDir $name) -Destination (Join-Path $installDir $name) -Force
}
Write-Host ('   copied ' + $binaries.Count + ' binaries to ' + $installDir)

Write-Host '== register the scheduled task ==' -ForegroundColor Cyan

$action = New-ScheduledTaskAction -Execute $exe -Argument 'run' -WorkingDirectory $installDir
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest

# AllowStartIfOnBatteries and DontStopIfGoingOnBatteries keep a laptop running
# on battery. A zero ExecutionTimeLimit is PT0S, which means unlimited.
# Task Scheduler has no "hide the console" switch; Hidden hides the task itself,
# so the supervisor is built without a console window.
#
# RestartCount is a lifetime budget, not a rolling one: 3 attempts meant the
# fourth crash left the stack down until someone noticed. 999 attempts at one
# minute apart is far beyond any real crash-loop; the watchdog covers the rest.
$settingsArgs = @{
    AllowStartIfOnBatteries    = $true
    DontStopIfGoingOnBatteries = $true
    RestartInterval            = (New-TimeSpan -Minutes 1)
    RestartCount               = 999
    ExecutionTimeLimit         = ([TimeSpan]::Zero)
    MultipleInstances          = 'IgnoreNew'
    Hidden                     = $true
}
$settings = New-ScheduledTaskSettingsSet @settingsArgs

$registerArgs = @{
    TaskName    = $taskName
    Action      = $action
    Trigger     = $trigger
    Settings    = $settings
    Principal   = $principal
    Description = 'LetShare tier-2 supervisor (elevated, at logon).'
    Force       = $true
}
Register-ScheduledTask @registerArgs | Out-Null

$registered = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if (-not $registered) {
    Write-Host ('ERROR: task ' + $taskName + ' was not registered') -ForegroundColor Red
    exit 1
}

Write-Host '== register the watchdog task ==' -ForegroundColor Cyan

# The watchdog is a no-op unless the supervisor is missing, so it can tick next
# to a healthy stack. MultipleInstances IgnoreNew keeps ticks from stacking up
# while one is still polling, and StartWhenAvailable catches up after sleep.
$watchdogAction = New-ScheduledTaskAction -Execute $watchdogExe -WorkingDirectory $installDir
$watchdogLogonTrigger = New-ScheduledTaskTrigger -AtLogOn -User $user

# Every 5 minutes, for ten years. A MaxValue duration serializes to
# P99999999DT23H59M59S, which Task Scheduler refuses at registration even
# though PowerShell will happily construct it. Ten years of five-minute ticks
# outlives any laptop, and the logon trigger covers the start either way.
$repeatArgs = @{
    Once               = $true
    At                 = (Get-Date).AddMinutes(1)
    RepetitionInterval = (New-TimeSpan -Minutes 5)
    RepetitionDuration = (New-TimeSpan -Days 3650)
}
$watchdogRepeatTrigger = New-ScheduledTaskTrigger @repeatArgs

# A repeated trigger defaults to StopAtDurationEnd, which would terminate a
# tick that was still running when the duration expired. Clear it explicitly.
$watchdogRepeatTrigger.Repetition.StopAtDurationEnd = $false

$watchdogSettingsArgs = @{
    AllowStartIfOnBatteries    = $true
    DontStopIfGoingOnBatteries = $true
    ExecutionTimeLimit         = ([TimeSpan]::Zero)
    MultipleInstances          = 'IgnoreNew'
    StartWhenAvailable         = $true
    Hidden                     = $true
}
$watchdogSettings = New-ScheduledTaskSettingsSet @watchdogSettingsArgs

$watchdogRegisterArgs = @{
    TaskName    = $watchdogTaskName
    Action      = $watchdogAction
    Trigger     = @($watchdogLogonTrigger, $watchdogRepeatTrigger)
    Settings    = $watchdogSettings
    Principal   = $principal
    Description = 'LetShare watchdog: starts the LetShare task when no supervisor process is running.'
    Force       = $true
}
try {
    Register-ScheduledTask @watchdogRegisterArgs | Out-Null
} catch {
    Write-Host ('ERROR: failed to register ' + $watchdogTaskName + ': ' + $_.Exception.Message) -ForegroundColor Red
    Write-Host 'The supervisor task is installed; the watchdog is not. Fix and re-run.' -ForegroundColor Red
    exit 1
}

$watchdog = Get-ScheduledTask -TaskName $watchdogTaskName -ErrorAction SilentlyContinue
if (-not $watchdog) {
    Write-Host ('ERROR: task ' + $watchdogTaskName + ' was not registered') -ForegroundColor Red
    exit 1
}

# Prove the repetition survived serialization instead of trusting the object
# we just built in memory.
$repeating = @($watchdog.Triggers | Where-Object { $_.Repetition -and $_.Repetition.Interval })
if ($repeating.Count -eq 0) {
    Write-Host ('ERROR: ' + $watchdogTaskName + ' has no repetition trigger') -ForegroundColor Red
    exit 1
}
$repetition = $repeating[0].Repetition

Write-Host ''
Write-Host '== start the supervisor ==' -ForegroundColor Cyan
# Installing means running; do not leave the stack down until the next logon.
Start-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
Start-Sleep -Seconds 3
$running = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
Write-Host ('   ' + $taskName + ' state: ' + $running.State)

Write-Host ''
Write-Host 'LetShare autostart installed.' -ForegroundColor Green
Write-Host ('  task    : ' + $taskName)
Write-Host ('  command : ' + $exe + ' run')
Write-Host ('  cwd     : ' + $installDir)
Write-Host ('  trigger : at logon of ' + $user + ', highest privileges')
Write-Host '  restart : on failure, every 1 minute, up to 999 attempts'
Write-Host '  window  : hidden task; the supervisor allocates no console'
Write-Host ''
Write-Host ('  task    : ' + $watchdogTaskName)
Write-Host ('  command : ' + $watchdogExe)
Write-Host ('  cwd     : ' + $installDir)
Write-Host ('  trigger : at logon of ' + $user + ', then every ' + $repetition.Interval + ' indefinitely')
Write-Host ('  role    : starts ' + $taskName + ' again when no letsshare-daemon process is running')
Write-Host '  window  : hidden task; the watchdog allocates no console'
Write-Host ('  log     : ' + (Join-Path $env:TEMP 'letsshare-watchdog.log') + ' (actions only)')
Write-Host ''
Write-Host '  Start it now:'
Write-Host ('    Start-ScheduledTask -TaskName ' + $taskName)
Write-Host ''
Write-Host '  Undo:'
Write-Host ('    powershell -ExecutionPolicy Bypass -File "' + (Join-Path $PSScriptRoot 'uninstall-autostart.ps1') + '"')
