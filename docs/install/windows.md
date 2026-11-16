# Windows: install sacdfs

This shows your SACD images as a drive letter of DSF and WAV files on a Windows PC, which any
player on that PC can open. About 10 minutes. To play the discs from a NAS instead, install
sacdfs on the NAS ([install guide](README.md)) and open its share from Windows.

Checked before 0.6.5 with the zip the release workflow builds, on Windows 11 in a VM (x64),
with WinFsp 2.1. See [Details](#details).

## What you need

- Windows 10 or 11, x64.
- [WinFsp](https://winfsp.dev) 2.1 or later: the file system driver sacdfs runs on. The steps
  install it.
- A folder with your SACD images (`.iso` files). The examples use `D:\Music\SACD`; change it to
  yours.

## Steps

Open **PowerShell** (Start menu, type `powershell`). Use a normal window, not "Run as
administrator": a drive mounted from an administrator window is hidden from Explorer.

1. **Install WinFsp.** Type:

   ```powershell
   winget install WinFsp.WinFsp
   ```

   Windows asks to allow the installer to make changes: allow it. Without winget, download
   `winfsp-2.1.25156.msi` from [WinFsp's releases](https://github.com/winfsp/winfsp/releases)
   and open it.

2. **Download sacdfs-win and its checksum.**

   ```powershell
   cd $HOME\Downloads
   curl.exe -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.5/sacdfs-0.6.5-x86_64-windows.zip
   curl.exe -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.5/sacdfs-0.6.5-x86_64-windows.zip.sha256
   ```

3. **Check the download.**

   ```powershell
   (Get-FileHash sacdfs-0.6.5-x86_64-windows.zip).Hash -eq (Get-Content sacdfs-0.6.5-x86_64-windows.zip.sha256).Split(" ")[0]
   ```

   *You should see* `True`. If you see `False`, delete both files and download them again.

4. **Unpack it into your programs folder.**

   ```powershell
   Expand-Archive sacdfs-0.6.5-x86_64-windows.zip $env:LOCALAPPDATA\Programs\sacdfs
   Unblock-File $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe
   ```

   `Unblock-File` tells Windows the file is the one you checked, so it doesn't warn about a
   downloaded program each time (see [Windows's warning](#windowss-warning)).

5. **Mount your images.** Pick a free drive letter; this uses `S:`.

   ```powershell
   & $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe D:\Music\SACD S:
   ```

   *You should see* a line ending `mounted D:\Music\SACD on S:`. Leave the window open: the drive
   stays while sacdfs-win runs.

6. **Open the drive.** In Explorer, open **This PC**, then the drive **S:**. *You should see* one
   folder per disc, each with `Stereo` and, for multichannel discs, `Multichannel` folders of
   `.dsf` and `.wav` files. Point your player's library at `S:\`.

7. **Stop it** with **Ctrl+C** in its window. *You should see* `unmounted`, and the drive goes
   away.

## Keep it running without a window

WinFsp comes with a service, WinFsp.Launcher, that starts and stops file systems on request.
Registered once by an administrator, it lets each user start sacdfs-win in the background,
under their own account, and stop it cleanly.

1. **Register sacdfs-win with the launcher** (once, in PowerShell **as administrator**):

   ```powershell
   Copy-Item $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe "C:\Program Files\sacdfs-win.exe"
   $key = "HKLM:\SOFTWARE\WOW6432Node\WinFsp\Services\sacdfs"
   New-Item $key -Force | Out-Null
   Set-ItemProperty $key Executable "C:\Program Files\sacdfs-win.exe"
   Set-ItemProperty $key CommandLine "%1 %2"
   Set-ItemProperty $key Security "D:P(A;;RPWPLC;;;BU)"
   Set-ItemProperty $key RunAs "."
   Set-ItemProperty $key JobControl 1 -Type DWord
   ```

   `RunAs "."` runs sacdfs-win as the user who starts it; `Security` lets every user of the PC
   (`BU`) start and stop their own. The first line copies the program from the folder of the
   account the administrator window runs as; if that's another account than the one you
   unpacked it with in step 4, copy `sacdfs-win.exe` from that account's
   `AppData\Local\Programs\sacdfs` folder instead. The program goes under
   `C:\Program Files`, where only administrators can change it, because the launcher may run it
   as SYSTEM.

2. **Start it** (a normal PowerShell window from now on), with your folder and drive letter.
   `music` is a name for this mount, used to stop it:

   ```powershell
   & "C:\Program Files (x86)\WinFsp\bin\launchctl-x64.exe" start sacdfs music D:\Music\SACD S:
   ```

   *You should see* `OK`, and the drive `S:` in Explorer. No window stays open.

3. **Stop it:**

   ```powershell
   & "C:\Program Files (x86)\WinFsp\bin\launchctl-x64.exe" stop sacdfs music
   ```

   *You should see* `OK`, and the drive goes away.

## Start it when you log on

With the launcher registered (above), a scheduled task can start it at each logon. In a normal
PowerShell window:

```powershell
$launchctl = "C:\Program Files (x86)\WinFsp\bin\launchctl-x64.exe"
$action = New-ScheduledTaskAction -Execute $launchctl -Argument "start sacdfs music D:\Music\SACD S:"
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
Register-ScheduledTask -TaskName sacdfs -Action $action -Trigger $trigger
```

*You should see* the task listed with `State: Ready`. At your next logon the drive comes back by
itself. Stop it as in step 3 above; remove the task with
`Unregister-ScheduledTask -TaskName sacdfs -Confirm:$false`.

## For every user of the PC

The launcher can also run sacdfs-win as the SYSTEM account, so that one drive is seen in every
user's session. SOURCE must then be a folder SYSTEM can read: a local disk, not a mapped network
drive, and every user of the PC can read the files (`--no-allow-other` limits them to
administrators). In PowerShell **as administrator**, register it under a second name, without
`RunAs`:

```powershell
$key = "HKLM:\SOFTWARE\WOW6432Node\WinFsp\Services\sacdfs-all"
New-Item $key -Force | Out-Null
Set-ItemProperty $key Executable "C:\Program Files\sacdfs-win.exe"
Set-ItemProperty $key CommandLine "%1 %2 --log-file C:\ProgramData\sacdfs.log"
Set-ItemProperty $key Security "D:P(A;;RPWPLC;;;BA)"
Set-ItemProperty $key JobControl 1 -Type DWord
& "C:\Program Files (x86)\WinFsp\bin\launchctl-x64.exe" start sacdfs-all music D:\Music\SACD S:
```

*You should see* `OK`. Only administrators may start and stop this one (`BA`); stop it with
`launchctl-x64.exe stop sacdfs-all music`. sacdfs-win also runs as an ordinary Windows service
(`sc.exe create`, with SOURCE and the drive letter in its command line).

## Options

sacdfs-win takes the options of the Linux mount that mean the same on Windows, with the same
defaults: `--format`, `--pauses`, `--pcm-rate`, `--pcm-cutoff`, `--pcm-gain`, `--flatten`,
`--sidecars`, `--tags`, `--musicbrainz`, `--cache-mb`, `--threads`, `--index-cache` and the
others in [options](../options.md). `sacdfs-win --help` lists them. Windows only:

- `--no-kernel-cache`: by default Windows keeps file contents and folder listings in memory
  until sacdfs-win says they changed. This turns that off; every read reaches sacdfs-win.
- `--allow-other`: let the other users of the PC read the drive (needed to share it over SMB).
  By default only the user who started sacdfs-win can, besides administrators; run as SYSTEM,
  everyone can, unless `--no-allow-other`.
- `--log-file FILE`: write the log there instead of the window.

The seek index is kept in `%LOCALAPPDATA%\sacdfs` unless `--index-cache` names another folder.

## Names on Windows

Windows refuses some characters in names that discs and folders can have. sacdfs-win shows them
as the same look-alike characters that a NAS's SMB share shows (Samba's `catia` mapping, as
macOS and Windows SMB clients expect):

| In the name | On Windows |
|---|---|
| `"` `*` `:` `<` `>` `?` `\` `\|` | private-use characters U+F020 to U+F027, which look alike in most fonts |
| a trailing space or dot | U+F028 or U+F029 |
| a bare device name (`CON`, `NUL`, `COM1`...) | the name with `_` after it |

Lookups ignore case, and Windows is told the name as stored. Of two names that differ only in
case, and of names longer than 255 UTF-16 units, the first is shown and the others are left out
with a line in the log.

## Windows's warning

The program isn't signed yet. The first time it runs from a download, Windows's SmartScreen may
show "Windows protected your PC" and Microsoft Defender may scan it. `Unblock-File` in step 4,
after the checksum matched, stops the warning. If SmartScreen shows anyway, choose **More info**,
then **Run anyway**.

## Microsoft Defender and the first open

Microsoft Defender scans each file of the drive the first time a program opens it, as it does on
any local disk. sacdfs makes the scanned part on the fly, so the first open of each file costs
about 5 ms more (Windows 11 in a VM, 4 CPUs); later opens in the same session don't. A player
that reads whole albums doesn't notice it; a library scan of thousands of files does, the first
time.

## If something goes wrong

- **"WinFsp is not installed":** install WinFsp (step 1), then open a new PowerShell window.
- **The drive isn't in Explorer:** sacdfs-win was started from an administrator window. Stop it
  and start it from a normal one.
- **"cannot mount on S:":** the letter is taken. Pick another.
- **A folder in your images can't be renamed while mounted:** close the player that reads from
  it; sacdfs-win closes images 10 seconds after their last read.
- **"Windows protected your PC" from the launcher or a task:** unblock the program
  (`Unblock-File`, step 4) before copying it.
- **Something else:** start it with `-v` for more detail, and see
  [reporting a problem](../../CONTRIBUTING.md).

## Details

- **What was checked:** the steps above, the launcher (as a user and as SYSTEM), the logon task
  and a Windows service on Windows 11 in a VM, Explorer as a normal user (listing, thumbnails of
  covers, the details pane), plus the checks the `windows` workflow runs on every change: every file of the test
  corpus byte-identical to the Linux mount, end of file, names in any case, refused writes, the
  WinFsp notice, and a clean stop on Ctrl+C and Ctrl+Break.
- **WinFsp:** sacdfs-win uses WinFsp - Windows File System Proxy, Copyright (C) Bill
  Zissimopoulos, <https://github.com/winfsp/winfsp>, under WinFsp's FLOSS exception.
  `sacdfs-win --version` shows the same notice.
- **Licence:** sacdfs is GPL-3.0-or-later; the Windows program links the winfsp crate, which is
  GPL-3.0-only, so the Windows binary is under GPL-3.0.
