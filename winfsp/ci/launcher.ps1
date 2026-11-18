# SPDX-License-Identifier: GPL-3.0-or-later
#
# sacdfs-win started by WinFsp.Launcher with a SOURCE that ends in a backslash (#466), run by
# the windows workflow (as administrator) and by hand on a Windows test machine:
#
#   pwsh -File winfsp\ci\launcher.ps1 -Exe PATH\sacdfs-win.exe -Source tests\corpus\images -Files 914
#
# Registers a launcher entry as docs/install/windows.md does, with `--log-file` after %1 %2,
# then starts two mounts: SOURCE with a backslash at the end on Y:, and a drive root (`T:\`, a
# subst of SOURCE) on `Z:\`. Each must show Files files, stop cleanly and leave the repair in
# the log. Removes the entry and the subst at the end. Exits 1 on any failure.
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Source,
    [Parameter(Mandatory)] [int] $Files,
    [string] $Work = $env:TEMP
)
$ErrorActionPreference = "Stop"
$failed = New-Object System.Collections.Generic.List[string]
function Check([string] $name, [bool] $ok, [string] $detail = "") {
    $line = "{0} {1}{2}" -f ($(if ($ok) { "PASS" } else { "FAIL" })), $name, $(if ($detail) { ": $detail" } else { "" })
    Write-Host $line
    if (-not $ok) { $failed.Add($name) }
}

$exe = (Resolve-Path $Exe).Path
$src = (Resolve-Path $Source).Path.TrimEnd('\')
$launchctl = Join-Path ${env:ProgramFiles(x86)} "WinFsp\bin\launchctl-x64.exe"
$log = Join-Path $Work "sacdfs-launcher-$PID.log"
$key = "HKLM:\SOFTWARE\WOW6432Node\WinFsp\Services\sacdfs-ci"
New-Item $key -Force | Out-Null
Set-ItemProperty $key Executable $exe
Set-ItemProperty $key CommandLine "%1 %2 --log-file $log --no-index-cache"
Set-ItemProperty $key Security "D:P(A;;RPWPLC;;;BU)(A;;RPWPLC;;;BA)"
Set-ItemProperty $key RunAs "."
Set-ItemProperty $key JobControl 1 -Type DWord
subst.exe T: $src

try {
    foreach ($case in @(
            @{ Name = "trailing-backslash"; Src = "$src\"; Mount = "Y:"; Root = "Y:\" },
            @{ Name = "drive-root"; Src = "T:\"; Mount = "Z:\"; Root = "Z:\" })) {
        $n = $case.Name
        $out = & $launchctl start sacdfs-ci $n $case.Src $case.Mount 2>&1 | Out-String
        Check "$n start" ($out -match "OK") $out.Trim()
        $count = 0
        foreach ($i in 1..60) {
            if (Test-Path $case.Root) {
                $count = @(Get-ChildItem -LiteralPath $case.Root -Recurse -File -Force -ErrorAction SilentlyContinue).Count
                if ($count -ge $Files) { break }
            }
            Start-Sleep -Seconds 1
        }
        Check "$n files" ($count -eq $Files) "$count of $Files"
        $out = & $launchctl stop sacdfs-ci $n 2>&1 | Out-String
        Check "$n stop" ($out -match "OK") $out.Trim()
        $gone = $false
        foreach ($i in 1..30) {
            if (-not (Test-Path $case.Root)) { $gone = $true; break }
            Start-Sleep -Seconds 1
        }
        Check "$n unmounted" $gone
    }
    $text = if (Test-Path $log) { Get-Content -Raw $log } else { "" }
    $repairs = ([regex]::Matches($text, "#466")).Count
    Check "repair logged" ($repairs -eq 2) "$repairs of 2"
    if ($failed.Count) { Write-Host "--- $log"; Write-Host $text }
}
finally {
    subst.exe T: /d
    Remove-Item $key -ErrorAction SilentlyContinue
    Remove-Item $log -ErrorAction SilentlyContinue
}

if ($failed.Count) {
    Write-Host "FAILED: $($failed -join ', ')"
    exit 1
}
Write-Host "All launcher checks passed."
