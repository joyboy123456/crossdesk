<#
.SYNOPSIS
    Start CrossDesk when the current user signs in.

.DESCRIPTION
    Writes a per-user Run entry, the same one the installer offers. Useful for
    a build you run straight out of target\release without installing it.

    Per-user on purpose: the entry must live in the hive of the account that
    actually uses the machine, and input capture needs a desktop session
    anyway, so a machine-wide or service-based autostart would not help.

.PARAMETER Path
    Executable to launch. Defaults to the release build in this checkout.

.PARAMETER Remove
    Remove the entry instead of adding it.

.EXAMPLE
    .\scripts\windows-autostart.ps1
    .\scripts\windows-autostart.ps1 -Path "C:\Apps\CrossDesk.exe"
    .\scripts\windows-autostart.ps1 -Remove
#>
[CmdletBinding()]
param(
    [string]$Path,
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'

$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$name = 'CrossDesk'

if ($Remove) {
    if (Get-ItemProperty -Path $runKey -Name $name -ErrorAction SilentlyContinue) {
        Remove-ItemProperty -Path $runKey -Name $name
        Write-Output "removed autostart entry '$name'"
    } else {
        Write-Output "no autostart entry '$name' to remove"
    }
    return
}

if (-not $Path) {
    $repoRoot = Split-Path -Parent $PSScriptRoot
    $Path = Join-Path $repoRoot 'target\release\crossdesk.exe'
}

$Path = (Resolve-Path -LiteralPath $Path).Path
if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "not a file: $Path"
}

Set-ItemProperty -Path $runKey -Name $name -Value "`"$Path`""
Write-Output "CrossDesk will start at sign-in: $Path"
