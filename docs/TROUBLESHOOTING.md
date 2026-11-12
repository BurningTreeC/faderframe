# Troubleshooting

What to do when FaderFrame crashes, freezes or something does not work, and where its files are.

## After a crash: the report and the log

When FaderFrame crashes, it writes a **crash report** and keeps a **log** of
the run. Those two files usually say what went wrong.

**The quick way.** Start FaderFrame again. If the last run didn't close
properly, a window appears: *FaderFrame closed unexpectedly last time
(started … UTC)*. **Show Report** opens the crash report: the file to send.
**Show Log** appears instead when there is no report (see
[No crash report?](#no-crash-report)). **Open Folder** opens the folder with
all reports and logs. The window appears only once, at the next start; if
you closed it, find the files by hand as described below.

**What to send.** All files are in a folder named **`Crash Reports`**. The
names hold the start time (UTC) and the process number, so files from the
same run match:

| File | What it is |
|---|---|
| `crash-20261021-095712-46264.log` | **The crash report.** FaderFrame version, system, GTK version, where it crashed (backtrace) and the last 150 lines of the log. |
| `session-20261021-095712-46264.log` | **The log of that run**, from start to crash. Same time and number as the report. |
| `crash-…-plugin-sandbox-….log` | A plugin crashed in its sandbox. FaderFrame itself kept running. |
| `crash-…-check-plugin-….log` | A plugin crashed during a plugin check. |
| `running-<number>` | A marker FaderFrame writes while it runs. You don't need to send it. |

Please send:

1. The `crash-….log` **and** the `session-….log` with the same time and number.
2. What you were doing just before. For example: "pressed Play", "changed
   the Audio System from PipeWire to JACK in Preferences → Audio", "opened
   plugin X".
3. Whether it happens every time or only sometimes.

Report problems at <https://github.com/BurningTreeC/faderframe/issues>.

> **Privacy:** logs and crash reports contain file paths, including your
> user name, project names and plugin names (a Windows crash report lists
> every library and plugin that was loaded). Look through them before
> posting them publicly.

> **Copy the files soon.** FaderFrame keeps the newest 20 crash reports and
> only the newest **8 session logs**. Older ones are deleted at start-up,
> so after eight more starts the log of a crashed run is gone.

## Where the files are

| How FaderFrame is installed | `Crash Reports` folder |
|---|---|
| **Portable** (Linux tarball or Windows zip with a `FaderFrame Data` folder next to it) | `FaderFrame Data/Data/Crash Reports` inside the FaderFrame folder |
| **Linux** (installed with `install.sh`, a distribution package, or built from source) | `~/.local/share/faderframe/Crash Reports` (or `$XDG_DATA_HOME/faderframe/Crash Reports`) |
| **Linux Flatpak** | `~/.var/app/io.github.BurningTreeC.FaderFrame/data/faderframe/Crash Reports` |
| **macOS** | `~/Library/Application Support/faderframe/Crash Reports` (portable: `FaderFrame Data/Data/Crash Reports` next to `FaderFrame.app`) |
| **Windows** (installer) | `%LOCALAPPDATA%\faderframe\Crash Reports` |
| `FADERFRAME_DATA_DIR` set | `<that folder>/Data/Crash Reports` |

A crash report's first lines also name the log file of its run, with its
full path. To open the folder from a terminal:

```sh
# Linux, newest files first
ls -lt ~/.local/share/faderframe/"Crash Reports" | head
# Linux portable: in the FaderFrame folder
ls -lt "FaderFrame Data/Data/Crash Reports" | head
# macOS
open ~/Library/Application\ Support/faderframe/Crash\ Reports
```

```bat
:: Windows (Win+R, or a command prompt)
explorer "%LOCALAPPDATA%\faderframe\Crash Reports"
```

## No crash report?

If there is a `session-….log` from the time of the crash but no
`crash-….log`, the system ended FaderFrame, so it had no chance to write a
report. Common causes:

- **Memory ran out** (Linux's out-of-memory killer).
- **A realtime time limit was reached.** On Linux, audio threads that get
  realtime priority from RealtimeKit (rtkit) have a CPU-time limit, and the
  system kills a program whose realtime thread goes past it. FaderFrame
  normally catches this first: it puts the thread back to normal priority
  and the log says *"… ran realtime past the time limit without a break"*.
  Giving your user realtime rights removes the limit (see
  [Realtime rights on Linux](#realtime-rights-on-linux)).
- **The computer froze, restarted or lost power.**

Please send the `session-….log` of that run. On Linux, also send what the
system logged around that time:

```sh
journalctl -b --since "15 minutes ago" | grep -i -E 'faderframe|out of memory|oom|killed process|rtkit' > faderframe-system.txt
```

Change `"15 minutes ago"` to cover the time of the crash. If the computer
restarted since then, use `journalctl -b -1` (the previous boot) instead of
`-b`.

## FaderFrame stops responding (a freeze, not a crash)

If the window stops responding and the system offers to force-quit it, don't
force-quit yet. A freeze writes no report by itself. While it hangs, save
where every thread is.

On Linux, with `gdb` installed (`sudo apt install gdb`, `sudo pacman -S gdb`,
…):

```sh
gdb -p "$(pgrep -o -x faderframe)" -batch -ex 'thread apply all bt' > faderframe-freeze.txt 2>&1
```

(`pgrep -o` picks the oldest process named `faderframe`: the program
itself, not its plugin helpers.) If gdb says `ptrace: Operation not
permitted`, run the same command with `sudo` in front. Then close FaderFrame and send `faderframe-freeze.txt`
together with the `session-….log` of that run.

On macOS, open Activity Monitor, select FaderFrame, and choose **⋯ → Sample
Process**. Then save the sample and send it.

## A complete native backtrace (Linux, optional)

When FaderFrame crashes on a fatal signal (a segmentation fault or an abort),
it writes its report and then lets the system handle the crash. Where the
system keeps core dumps, this adds a complete backtrace of every thread.

Arch, Fedora and openSUSE have `systemd-coredump` set up already. On Debian,
Ubuntu, Linux Mint and LMDE, install it once:

```sh
sudo apt install systemd-coredump
```

After the next crash:

```sh
coredumpctl list faderframe              # the crashes the system kept
coredumpctl info faderframe > faderframe-coredump.txt   # the newest one
```

Send `faderframe-coredump.txt` together with the crash report.

## More detail in the log

If the crash is hard to pin down, start FaderFrame from a terminal with a
more detailed log. Then make the crash happen again. The `session-….log` of
that run will hold the extra detail, which also appears in the terminal.

```sh
# Linux portable: in the FaderFrame folder
RUST_LOG=debug ./faderframe
# Linux installed
RUST_LOG=debug faderframe
# macOS
RUST_LOG=debug /Applications/FaderFrame.app/Contents/MacOS/FaderFrame
```

```powershell
# Windows (PowerShell)
$env:RUST_LOG = "debug"; & "$env:ProgramFiles\FaderFrame\bin\faderframe.exe"
```

A debug log grows quickly, so stop as soon as the crash has happened.
Afterwards, start FaderFrame normally again, without `RUST_LOG`.

## Realtime rights on Linux

Audio runs best when your user may give threads realtime priority and lock
memory. FaderFrame's log says at start-up when it can't lock memory:

```text
memory not locked: the memory-lock limit is 8192 KiB (unlimited for members of the realtime or audio group …)
```

Without these rights, FaderFrame asks RealtimeKit for realtime priority.
RealtimeKit grants it with limits: a low priority, a limited number of
threads per user, and a CPU-time limit per thread. Also, without memory
locking, audio memory can be swapped out. Expect more dropouts under load.

To check your limits, run this in a terminal (`ulimit -r` is the realtime
priority, `ulimit -l` the memory you may lock, in KiB):

```sh
ulimit -r -l
```

A realtime priority of `0` means no realtime rights. To grant them:

**Debian, Ubuntu, Linux Mint, LMDE** (and most other distributions):

```sh
sudo tee /etc/security/limits.d/95-audio.conf <<'LIMITS'
@audio   -  rtprio     95
@audio   -  memlock    unlimited
LIMITS
sudo usermod -aG audio "$USER"
```

**Arch Linux:**

```sh
sudo pacman -S realtime-privileges
sudo usermod -aG realtime "$USER"
```

Then log out and back in (a restart is surest). `ulimit -r -l` should now
show a realtime priority above 0 (95 with the file above, 98 with Arch's
package) and `unlimited`, and the "memory not locked" line is gone from
FaderFrame's log. While FaderFrame runs, this shows the limits it really
got:

```sh
grep -E 'realtime|locked' /proc/"$(pgrep -o -x faderframe)"/limits
```

Realtime rights let your programs run ahead of everything else on the
system, so a program that hangs at realtime priority can make the desktop
sluggish. That is the usual trade-off on audio workstations.

## A phone or tablet cannot open the remote

The page keeps loading and then gives up: most often the computer's
firewall drops the connection. On the computer itself the remote still
works — its own connections pass the firewall — so the address answering
there proves nothing. When ufw or firewalld blocks the port, Preferences →
Remote shows a **Firewall** row with the command that lets in your
network only. With ufw (on by default on Omarchy) it looks like this —
your network interface, your network and the remote's port (8642 unless
you changed it):

```sh
sudo ufw allow in on enp4s0 from 192.168.178.0/24 to any port 8642 proto tcp comment 'FaderFrame remote'
sudo ufw status numbered    # the rule is there
```

That lets in only devices on your network, on that port, over TCP, on
the interface the computer is on that network with (not a VPN's). The
remote answers IPv4 only, so no IPv6 rule is needed; and when the remote
is off, nothing listens on the port. To close it again: `sudo ufw delete
allow in on enp4s0 from 192.168.178.0/24 to any port 8642 proto tcp`.
Don't forward the port on your router: nothing needs to reach it from the
internet.

If the firewall lets it in and the page still doesn't load:

* The phone must be on the same network, not a guest Wi-Fi (a FRITZ!Box's
  guest network, for example, is kept apart from the computers).
* A VPN app on the phone can keep it from local addresses.
* Preferences → Remote's Status names the address to open; it changes
  when the computer gets another address on the network.

## Options to narrow a problem down

Each of these turns off one part, so you can see whether the problem goes
away with it. The audio system, buffer size and processing threads take
effect with Preferences → Audio's Apply button; the other Preferences
options take effect at once. Command-line options and environment
variables apply to one start. For example:
`FADERFRAME_NO_MIDI=1 ./faderframe`.

**Audio**

- **Silent dummy device:** `faderframe --backend dummy`. If the problem is
  gone, it lies in the audio path (device, audio system or plugins).
- **Another audio system:** Preferences → Audio → Audio system (PipeWire,
  JACK or ALSA on Linux), or `--backend pipewire|jack|system`.
- **A larger buffer:** Preferences → Audio → Buffer size, or
  `--buffer-size 1024`. This helps with dropouts under load.
- **Fewer processing threads:** Preferences → Audio → Processing threads →
  `1 (audio thread only)`, or `--threads 1`.
- **Render ahead off:** Preferences → Audio → Render ahead → Off.
- **Stuck notes or a burst of noise:** Transport → Panic resets every
  device.

**Plugins**

- **Plugins in their own processes:** Preferences → General → Plugins,
  "Run each plugin in its own process" (on by default; CLAP and VST3, plus
  Audio Units on macOS). A crashing plugin then stops only itself, and its
  track plays on dry.
- **Check a plugin:** the plugin browser's Check button plays a plugin
  under measurement and lists what it does wrong on the audio thread.
- **An empty project:** `faderframe --empty` shows whether the problem
  comes with the project's plugins or content.

**Graphics, MIDI and video**

- **Drawing on the GPU off:** Preferences → General → Drawing, or
  `FADERFRAME_GPU_PAINTER=0`. Try this for crashes or wrong pictures that
  could come from the graphics driver.
- **Software rendering:** Preferences → General → Renderer → Software
  (from the next start), or `GSK_RENDERER=cairo` for one start. GTK then
  draws without the graphics driver. When the last run crashed inside the
  driver (for example Mesa's `libgallium`), FaderFrame switches to
  Software by itself and says so; choose Automatic there to go back.
  Automatic is Software on Intel graphics up to Haswell (GMA, Ironlake,
  Sandy Bridge, Ivy Bridge, Bay Trail, Haswell, Cherry View): Mesa's
  crocus driver for them crashes under GTK's renderer. Choose OpenGL to
  use the chip anyway.
- **No GPU frame sharing:** `FADERFRAME_GPU_DMABUF=0` (Linux; Windows:
  `FADERFRAME_GPU_SHARE=0`).
- **No zero-copy video:** `FADERFRAME_VIDEO_DMABUF=0` (Linux).
- **MIDI hardware closed:** `FADERFRAME_NO_MIDI=1`.

**Realtime (Linux)**

- **Don't ask RealtimeKit:** `FADERFRAME_RTKIT=0`. FaderFrame then doesn't
  ask RealtimeKit for realtime priority for its own threads. PipeWire still
  asks for its own.
- **Let the CPU sleep deeply:** `FADERFRAME_CPU_LATENCY=0`. Where the
  system lets it (`/dev/cpu_dma_latency` writable, e.g. Arch's `realtime`
  group), FaderFrame keeps the CPUs out of deep sleep states while it runs.
  This turns that off.

If one of these makes the problem go away, please say which in your report.
