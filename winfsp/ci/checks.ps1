# SPDX-License-Identifier: GPL-3.0-or-later
#
# Mount checks of sacdfs-win (#419), run by the windows workflow and by hand on a Windows test
# machine:
#
#   powershell -File winfsp\ci\checks.ps1 -Exe PATH\sacdfs-win.exe -Source tests\corpus\images `
#       -Mount X: -Reference linux.sha256
#
# Reference is `sha256sum` output of every file of a Linux mount of the same SOURCE, with paths
# relative to the mount (`./a/b`). Each Linux name is mapped as sacdfs-win maps it (SFM
# private-use characters, a trailing dot or space, bare reserved names), and the mount must hold
# the same files with the same bytes. Then: end of file, case-insensitive lookups that return
# the stored case, refused writes, `--version`'s WinFsp notice, and a clean stop on Ctrl+C and on
# Ctrl+Break. Exits 1 on any failure.
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Source,
    [string] $Mount = "X:",
    [Parameter(Mandatory)] [string] $Reference,
    [string] $Work = $env:TEMP
)
$ErrorActionPreference = "Stop"
$failed = New-Object System.Collections.Generic.List[string]
function Check([string] $name, [bool] $ok, [string] $detail = "") {
    $line = "{0} {1}{2}" -f ($(if ($ok) { "PASS" } else { "FAIL" })), $name, $(if ($detail) { ": $detail" } else { "" })
    Write-Host $line
    if (-not $ok) { $failed.Add($name) }
}

# The name sacdfs-win shows for a name of the core (winfsp/src/names.rs).
$sfm = @{ [char]'"' = [char]0xF020; [char]'*' = [char]0xF021; [char]':' = [char]0xF022; [char]'<' = [char]0xF023
          [char]'>' = [char]0xF024; [char]'?' = [char]0xF025; [char]'\' = [char]0xF026; [char]'|' = [char]0xF027 }
$reserved = @("CON", "PRN", "AUX", "NUL") + (1..9 | ForEach-Object { "COM$_"; "LPT$_" })
function WinName([string] $n) {
    $sb = New-Object System.Text.StringBuilder
    foreach ($c in $n.ToCharArray()) {
        if ([int]$c -ge 1 -and [int]$c -le 31) { [void]$sb.Append([char](0xF000 + [int]$c)) }
        elseif ($sfm.ContainsKey($c)) { [void]$sb.Append($sfm[$c]) }
        else { [void]$sb.Append($c) }
    }
    $o = $sb.ToString()
    if ($o.EndsWith(" ")) { $o = $o.Substring(0, $o.Length - 1) + [char]0xF028 }
    elseif ($o.EndsWith(".") -and $o -ne "." -and $o -ne "..") { $o = $o.Substring(0, $o.Length - 1) + [char]0xF029 }
    if ($reserved -contains $o.ToUpperInvariant()) { $o += "_" }
    $o
}

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SacdfsWinCheck {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern uint GetFinalPathNameByHandleW(IntPtr h, StringBuilder buf, uint len, uint flags);
    // The path Windows gives an open file, without the volume (VOLUME_NAME_NONE): the file
    // system's stored case.
    public static string FinalPath(System.IO.FileStream f) {
        var sb = new StringBuilder(1024);
        uint n = GetFinalPathNameByHandleW(f.SafeFileHandle.DangerousGetHandle(), sb, 1024, 4);
        return n == 0 ? "" : sb.ToString();
    }
}
"@

# A Ctrl+C (0) or Ctrl+Break (1) to the console of process $id, sent by a small helper that
# attaches to that console (so that this one keeps its own), as a user's Ctrl+C in a terminal
# would arrive. Compiled by Windows PowerShell's Add-Type: run this script with powershell.exe.
$ctrl = Join-Path $Work "sacdfs-win-ctrl.exe"
if (-not (Test-Path $ctrl)) {
    Add-Type -OutputAssembly $ctrl -OutputType ConsoleApplication -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class Ctrl {
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool AttachConsole(uint p);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool FreeConsole();
    delegate bool Handler(uint ev);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetConsoleCtrlHandler(Handler h, bool add);
    // Survives the event it sends (Ctrl+Break can't be ignored, only handled).
    static readonly Handler Keep = ev => true;
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GenerateConsoleCtrlEvent(uint ev, uint group);
    public static int Main(string[] a) {
        FreeConsole();
        if (!AttachConsole(uint.Parse(a[0]))) return 2;
        SetConsoleCtrlHandler(Keep, true);
        bool sent = GenerateConsoleCtrlEvent(uint.Parse(a[1]), 0);
        System.Threading.Thread.Sleep(300);
        return sent ? 0 : 1;
    }
}
"@
}
function Send-Ctrl([int] $id, [int] $ev) {
    (Start-Process $ctrl -ArgumentList "$id $ev" -PassThru -Wait -WindowStyle Hidden).ExitCode
}

$log = Join-Path $Work "sacdfs-win-checks.log"
function Start-Mount([string] $extra = "") {
    $a = "`"$Source`" $Mount --log-file `"$log`" $extra"
    # A console of its own (hidden), so that a Ctrl+C reaches only it.
    $p = Start-Process $Exe -ArgumentList $a -PassThru -WindowStyle Hidden
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path "$Mount\") -and $sw.Elapsed.TotalSeconds -lt 60 -and -not $p.HasExited) {
        Start-Sleep -Milliseconds 50
    }
    Check "mount $extra" (Test-Path "$Mount\") "up after $([int]$sw.Elapsed.TotalMilliseconds) ms"
    $p
}
function Stop-Mount($p, [int] $ev, [string] $what) {
    $sent = Send-Ctrl $p.Id $ev
    $exited = $p.WaitForExit(30000)
    if (-not $exited) { Stop-Process $p.Id -Force }
    Start-Sleep -Milliseconds 300
    Check "stop on $what" ($exited -and $p.ExitCode -eq 0 -and -not (Test-Path "$Mount\")) `
        "sent $sent, exited $exited, code $(if ($exited) { $p.ExitCode }), drive gone $(-not (Test-Path "$Mount\"))"
}

# --version names WinFsp, as its licence exception asks.
$version = (& $Exe --version) -join "`n"
Check "--version notice" ($version.Contains("WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos") -and $version.Contains("https://github.com/winfsp/winfsp")) ($version -replace "`n", " | ")

$p = Start-Mount
try {
    # Every file, byte for byte.
    $want = @{}
    foreach ($line in Get-Content -LiteralPath $Reference -Encoding UTF8) {
        if (-not $line) { continue }
        $hash, $path = $line -split "  ", 2
        $path = $path -replace "^\./", ""
        $want[(($path -split "/") | ForEach-Object { WinName $_ }) -join "/"] = $hash
    }
    $got = @{}
    $sha = [Security.Cryptography.SHA256]::Create()
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $root = "$Mount\"
    foreach ($f in Get-ChildItem -LiteralPath $root -Recurse -File -Force) {
        $s = [IO.File]::OpenRead($f.FullName)
        try { $h = $sha.ComputeHash($s) } finally { $s.Close() }
        $rel = $f.FullName.Substring($root.Length).Replace('\', '/')
        $got[$rel] = [BitConverter]::ToString($h).Replace("-", "").ToLowerInvariant()
    }
    $missing = @($want.Keys | Where-Object { -not $got.ContainsKey($_) })
    $extra = @($got.Keys | Where-Object { -not $want.ContainsKey($_) })
    $differ = @($want.Keys | Where-Object { $got.ContainsKey($_) -and $got[$_] -ne $want[$_] })
    Check "corpus bytes" ($want.Count -gt 0 -and $missing.Count -eq 0 -and $extra.Count -eq 0 -and $differ.Count -eq 0) `
        "Linux $($want.Count) files, Windows $($got.Count), $($want.Count - $missing.Count - $differ.Count) byte-identical, in $([int]$sw.Elapsed.TotalSeconds) s"
    foreach ($x in @($missing | Select-Object -First 5)) { Write-Host "  missing on Windows: $x" }
    foreach ($x in @($extra | Select-Object -First 5)) { Write-Host "  only on Windows: $x" }
    foreach ($x in @($differ | Select-Object -First 5)) { Write-Host "  differs: $x" }

    # End of file: a read 10 bytes before it gets 10, at it or past it 0.
    $wav = Get-ChildItem -LiteralPath $root -Recurse -File -Filter *.wav | Select-Object -First 1
    $s = [IO.File]::OpenRead($wav.FullName); $b = New-Object byte[] 100
    [void]$s.Seek($wav.Length - 10, "Begin"); $n1 = $s.Read($b, 0, 100)
    [void]$s.Seek($wav.Length, "Begin"); $n2 = $s.Read($b, 0, 100)
    [void]$s.Seek($wav.Length + 4096, "Begin"); $n3 = $s.Read($b, 0, 100)
    $s.Close()
    Check "end of file" ($n1 -eq 10 -and $n2 -eq 0 -and $n3 -eq 0) "before $n1, at $n2, past $n3"

    # Any case finds the file; Windows is given the stored case.
    $dsf = Get-ChildItem -LiteralPath $root -Recurse -File -Filter *.dsf | Select-Object -First 1
    $upper = $dsf.FullName.ToUpperInvariant()
    $s = [IO.File]::OpenRead($upper)
    $final = [SacdfsWinCheck]::FinalPath($s); $s.Close()
    Check "case-insensitive open, stored case" ($final -ceq $dsf.FullName.Substring(2)) "$upper -> $final"

    # Read-only: nothing is created, written, deleted or renamed.
    foreach ($t in @(
            @{ n = "create a file"; a = { [IO.File]::WriteAllText("$Mount\new.txt", "x") } },
            @{ n = "create a folder"; a = { [IO.Directory]::CreateDirectory("$Mount\newdir") | Out-Null } },
            @{ n = "open for writing"; a = { [IO.File]::Open($dsf.FullName, "Open", "ReadWrite").Close() } },
            @{ n = "delete"; a = { [IO.File]::Delete($dsf.FullName) } },
            @{ n = "rename"; a = { [IO.File]::Move($dsf.FullName, $dsf.FullName + ".x") } })) {
        $err = $null
        try { & $t.a } catch { $err = $_.Exception.InnerException.Message; if (-not $err) { $err = $_.Exception.Message } }
        Check "refused: $($t.n)" ($null -ne $err -and (Test-Path -LiteralPath $dsf.FullName)) "$err"
    }
} finally {
    Stop-Mount $p 0 "Ctrl+C"
}
$p = Start-Mount
Stop-Mount $p 1 "Ctrl+Break"
if ($failed.Count) {
    "FAILED: $($failed -join ', ')"
    Get-Content $log -Tail 30 -ErrorAction SilentlyContinue
    exit 1
}
"all checks passed"
