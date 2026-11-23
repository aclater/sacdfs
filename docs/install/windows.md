# Windows: install sacdfs

This shows your SACD images as a drive letter of DSF and WAV files on a Windows PC, which any
player on that PC can open. About 10 minutes. To play the discs from a NAS instead, install
sacdfs on the NAS ([install guide](README.md)) and open its share from Windows.

Checked with the 0.7.0 release candidates' zips, following these steps as a standard
(non-administrator) user, on Windows 11 25H2 in a VM (x64), not on physical hardware, with WinFsp
2.1; Windows 10 22H2 and the service, logon task and NAS steps were last checked with 0.6.5. See
[Details](#details).

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

   The first time, winget asks you to accept the Microsoft Store source terms: type `Y`. Windows
   then asks to allow the installer to make changes. On an account that isn't an administrator,
   it asks for an administrator's name and password instead.

   Windows 10 doesn't include winget. There, and on any PC without winget, download
   `winfsp-2.1.25156.msi` from [WinFsp's releases](https://github.com/winfsp/winfsp/releases)
   and open it.

2. **Download sacdfs-win and its checksum.**

   ```powershell
   cd $HOME\Downloads
   curl.exe -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-x86_64-windows.zip
   curl.exe -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-x86_64-windows.zip.sha256
   ```

3. **Check the download.**

   ```powershell
   (Get-FileHash sacdfs-0.7.2-x86_64-windows.zip).Hash -eq (Get-Content sacdfs-0.7.2-x86_64-windows.zip.sha256).Split(" ")[0]
   ```

   *You should see* `True`. If you see `False`, delete both files and download them again.

4. **Unpack it into your programs folder.**

   ```powershell
   Expand-Archive sacdfs-0.7.2-x86_64-windows.zip $env:LOCALAPPDATA\Programs\sacdfs
   Unblock-File $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe
   ```

   `Unblock-File` tells Windows the file is the one you checked, so it doesn't warn about a
   downloaded program (see [Windows's warning](#windowss-warning)). After these steps it changes
   nothing: `curl.exe` doesn't mark its downloads, and `Expand-Archive` doesn't pass a browser
   download's mark on to the program. It matters if you unpacked a browser-downloaded zip with
   Explorer (**Extract all**), which does pass the mark on.

5. **Mount your images.** Pick a free drive letter; this uses `S:`.

   ```powershell
   & $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe D:\Music\SACD S:
   ```

   *You should see* a line ending `mounted D:\Music\SACD on S:`, then
   `The service sacdfs has been started.` Leave the window open: the drive
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

   If you use a standard (non-administrator) account, the administrator window runs as the
   administrator, so the first line can't find the program and fails with "Cannot find path".
   Copy `sacdfs-win.exe` from your own account's `AppData\Local\Programs\sacdfs` folder (under
   `C:\Users`) to `C:\Program Files` instead, then run the other lines. The first line works as
   written only when the administrator window runs as the account that did step 4.

   `RunAs "."` runs sacdfs-win as the user who starts it; `Security` lets every user of the PC
   (`BU`) start and stop their own. The program goes under
   `C:\Program Files`, where only administrators can change it, because the launcher may run it
   as SYSTEM.

2. **Start it** (a normal PowerShell window from now on), with your folder and drive letter.
   `music` is a name for this mount, used to stop it:

   ```powershell
   & "C:\Program Files (x86)\WinFsp\bin\launchctl-x64.exe" start sacdfs music D:\Music\SACD S:
   ```

   *You should see* `OK`, and the drive `S:` in Explorer. No window stays open.

   A folder with a backslash at the end (`D:\Music\SACD\`) and a whole drive (`E:\`) work
   from the version after 0.6.5. In 0.6.5 and earlier the launcher passes them on in a way that
   sacdfs-win can't read, and you see `OK` but no drive
   ([#466](https://github.com/aclater/sacdfs/issues/466)): there, leave the backslash out, and
   for a whole drive write `E:\.` (with the dot). The same applies to the logon task below.

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

*You should see* a table with the task `sacdfs` and `Ready` under State. At your next logon the
drive comes back by itself. Stop it as in step 3 above; remove the task with
`Unregister-ScheduledTask -TaskName sacdfs -Confirm:$false`.

## For every user of the PC

The launcher can also run sacdfs-win as the SYSTEM account, so that one drive is seen in every
user's session. SOURCE must then be a folder SYSTEM can read: a local disk, not a mapped network
drive (`The system cannot find the path specified`) nor a network share that needs a user name
and password (`Access is denied`), and every user of the PC can read the files
(`--no-allow-other` limits them to administrators). In PowerShell **as administrator**, register
it under a second name, without `RunAs`:

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

## Images on a NAS

SOURCE can be a folder on a network share, so the images stay on a NAS and the PC shows them as
a drive. The examples use a NAS called `nas` with a share `isos` and the user `music`; use yours.

Mount it with the share's path, or with a drive letter mapped to it (`secret` stands for the
share's password):

```powershell
& $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe \\nas\isos S:
```

```powershell
net use N: \\nas\isos /user:music secret
& $env:LOCALAPPDATA\Programs\sacdfs\sacdfs-win.exe N:\ S:
```

Both work with the launcher too (`launchctl-x64.exe start sacdfs music \\nas\isos S:`, or `N:`
without the backslash), except as SYSTEM (see [For every user of the PC](#for-every-user-of-the-pc)). For the logon task, save
the share's password for your account once, so the task can reach the share at the next logon,
then use the share's path in the task:

```powershell
cmdkey /add:nas /user:music /pass:secret
```

If the NAS restarts while you play, playback can stop (foobar2000 did), as it does with any file
on a share. The drive works again once the NAS is back.

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
- `--log-file FILE`: append the log there instead of the window. Each start writes a `---- sacdfs-win VERSION started TIME, pid N ----` line first.

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

The program isn't code-signed yet; it will be signed (Authenticode, through SSL.com eSigner,
with a timestamp) once the certificate is in place, and the release notes say so for each
release. `Get-AuthenticodeSignature sacdfs-win.exe` shows `Valid` for a signed one. Windows warns only about a program that carries a browser
download's mark, which the steps above don't leave on it (see step 4):

- Opened from Explorer, SmartScreen shows "Windows protected your PC". Run `Unblock-File` on the
  program, after the checksum matched; if it still warns, sign out and back in.
- Started from PowerShell, it runs without that warning. Windows 10 may show "Security scan
  required" for a few seconds the first time.
- Started through the shell (`Start-Process`), Windows may show "Open File – Security Warning"
  instead.

After `Unblock-File`, none of these show.

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
- **The launcher says `OK` but no drive appears:** sacdfs-win stopped at once, and the
  launcher keeps no output. To see why, add `--log-file` with a file you can write to the
  registration's `CommandLine` (as for `sacdfs-all` below, which logs to
  `C:\ProgramData\sacdfs.log`), start it again and read that file. With 0.6.5 and earlier, a
  folder ending with a backslash does this: leave it out, and for a whole drive write `E:\.`
  ([#466](https://github.com/aclater/sacdfs/issues/466)).
- **Something else:** start it with `-v` for more detail, and see
  [reporting a problem](../../CONTRIBUTING.md).

## Remove sacdfs

1. Stop the drive: `& "C:\Program Files (x86)\WinFsp\bin\launchctl-x64.exe" stop sacdfs music`,
   and, in PowerShell as administrator, `stop sacdfs-all music` the same way if you set that up.
2. In a normal PowerShell window, remove the logon task, the program and its seek index:

   ```powershell
   Unregister-ScheduledTask -TaskName sacdfs -Confirm:$false
   Remove-Item -Recurse $env:LOCALAPPDATA\Programs\sacdfs, $env:LOCALAPPDATA\sacdfs
   ```

3. In PowerShell **as administrator**, remove the launcher entries, the copied program, the
   SYSTEM log and the SYSTEM account's seek index:

   ```powershell
   Remove-Item "HKLM:\SOFTWARE\WOW6432Node\WinFsp\Services\sacdfs", "HKLM:\SOFTWARE\WOW6432Node\WinFsp\Services\sacdfs-all"
   Remove-Item "C:\Program Files\sacdfs-win.exe", C:\ProgramData\sacdfs.log
   Remove-Item -Recurse C:\Windows\System32\config\systemprofile\AppData\Local\sacdfs
   ```

   Leave out what you didn't set up. Remove WinFsp like any other app if nothing else uses it.

## Details

- **What was checked with 0.7.0-rc.1:** steps 1 to 7 as a standard user on Windows 11 in a VM
  (checksum, unpack, drive letter, Explorer, Ctrl+C): all 186 files of five real discs, and two
  RF64 files over 4 GiB, hash like the Linux build's read. Explorer's Title and Album columns
  show accented titles. With 0.7.0-rc.2: checksum, unpack, mount and the same hashes. A SOURCE ending in a backslash, a drive root and a `subst` drive
  mount through WinFsp.Launcher.
- **What was checked with 0.6.5:** the steps above as a standard user, with the 0.6.5 release zip, on
  Windows 11 25H2 (build 26200) and Windows 10 22H2 (build 19045) in VMs, not on physical
  hardware: the launcher (as a user and as SYSTEM), the logon task across a restart, a Windows
  service (Windows 11), a NAS share as SOURCE (path and mapped drive), hibernating with the drive
  mounted, the removal steps (Windows 10), Explorer as a normal user (listing, thumbnails of
  covers, the details pane, large icons), and audio played to the VM's sound device, recorded and
  compared with the same file played from a local copy (bit-identical in foobar2000). Not run:
  sleep (S3), because the VMs don't wake from it, with or without sacdfs. Plus the checks the
  `windows` workflow runs on every change: every file of the test corpus byte-identical to the
  Linux mount, end of file, names in any case, refused writes, the WinFsp notice, and a clean stop
  on Ctrl+C and Ctrl+Break; and, through the launcher with a `--log-file` after `%1 %2`, a
  folder ending with a backslash and a drive root (`subst`) as SOURCE, each mounted with every
  file and stopped (#466). That workflow runs on GitHub's Windows Server runner; the
  backslash fix has not been run on Windows 10 or 11.
- **WinFsp:** sacdfs-win uses WinFsp - Windows File System Proxy, Copyright (C) Bill
  Zissimopoulos, <https://github.com/winfsp/winfsp>, under WinFsp's FLOSS exception.
  `sacdfs-win --version` shows the same notice.
- **Licence:** sacdfs is GPL-3.0-or-later; the Windows program links the winfsp crate, which is
  GPL-3.0-only, so the Windows binary is under GPL-3.0.
