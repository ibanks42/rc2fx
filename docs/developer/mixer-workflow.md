# Working on a mixer

Reflashing firmware for every code change is slow, so day-to-day mixer
development uses SSH instead. `just mixer-run` deploys a build into the mixer's
RAM, and `just mixer-install` installs it persistently. The desktop app toggles
SSH access over USB 1, though the app itself never uses SSH.

## 1. Enable SSH access

RØDE's firmware already runs an SSH server that only permits key-based root
logins, and ships with no authorized keys so nobody can log in by default.
RC2FX can install a managed key without reflashing:

1. With RC2FX installed and the mixer connected on USB 1, enable
   **Settings › Developer › Developer mode** in the app.
2. Under **SSH access**, click **Enable SSH** and confirm.

The app adds this computer's public key to root's `authorized_keys` on the
mixer (tagged as RC2FX-managed) without interrupting audio. It uses
`~/.ssh/rc2fx_rodecaster_pro_ii`, generating the key pair if needed; the
private key never leaves your computer. The app also fetches the mixer's SSH
host key over USB and pins it in the installer cache's `known_hosts` under an
alias like `rc2fx-0123456789ab` (derived from the mixer's serial number), so
you don't have to blindly trust it on first login. The app then displays the
exact `ssh` command with a **Copy** button. Enter the IP from the mixer's
**Settings › Network** screen into **Mixer address** to fill it in (remembered
until the app closes).

The status row reports which key is active, with its `SHA256:` fingerprint on
the **Key** line underneath:

- **Logins use this computer's key:** the managed key matches this machine.
  Only this state shows the copyable `ssh` command.
- **Logins use a key from another computer:** the managed key belongs to a
  different machine.
- **Logins use an RC2FX-managed key:** this computer's local key couldn't be
  read.

This status is refreshed whenever you open Settings.

If other keys are also present in `authorized_keys` (such as keys added
manually or by an older installer), the app reports how many there are. RC2FX
never modifies or removes keys it didn't add.

Clicking **Disable SSH** and confirming removes only RC2FX's managed key. New
logins with that key are blocked immediately, while any open SSH sessions stay
connected until closed. Turning **Developer mode** off only hides the developer
UI; it does not revoke SSH access. Reflashing firmware also clears the key
because an update replaces the root filesystem.

Enabling and disabling SSH are refused during the first boot after a firmware
update, and whenever `/home/root/.ssh` or `authorized_keys` looks unsafe to
modify (for instance if it's a symlink, owned by another user, writable by
others, or contains NUL bytes).

## 2. Configure the mixer address

Connect the mixer to your local network, copy `.rc2fx-mixer.conf.example` to
`.rc2fx-mixer.conf` (gitignored), and fill in its IP address:

```ini
ip = 192.168.0.100
# key = ~/.ssh/rc2fx_rodecaster_pro_ii
# known_hosts = ~/.local/share/RC2FX/installer/known_hosts
# host_key_alias = rc2fx-0123456789ab
# pre_command = ~/.local/bin/rc2fx-pre-mixer
```

Point `known_hosts` at the file inside your installer cache directory, and set
`host_key_alias` to the alias shown in the app's `ssh` command:

| Platform | Cache folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%\RC2FX\installer` |
| macOS | `~/Library/Application Support/RC2FX/installer` |
| Linux | `~/.local/share/RC2FX/installer` |

You can override any of these with environment variables: `RC2FX_MIXER_IP`,
`RC2FX_MIXER_USER`, `RC2FX_MIXER_KEY`, `RC2FX_MIXER_KNOWN_HOSTS`,
`RC2FX_MIXER_HOST_KEY_ALIAS`, and `RC2FX_MIXER_PRE_COMMAND`.

If `pre_command` is set (a leading `~/` expands to your home directory),
mutating mixer commands run that executable with the action name as `$1` and
`RC2FX_MIXER_*` environment variables after the first-boot safety check,
aborting if it exits non-zero. Read-only commands (`status`, `logs`) skip it.

Verify the connection:

```sh
just mixer-status
```

## 3. Run or install a build

```sh
just mixer-run       # run this build from RAM until the next reboot
just mixer-install   # install this build to /opt/rc2fx and start it at every boot
just mixer-uninstall # remove RC2FX from the mixer (--purge also deletes settings)
```

Both `mixer-run` and `mixer-install` rebuild the mixer runtime first.

`mixer-run` copies the build into RAM and restarts the audio engine through
RØDE's launcher. Because this restarts audio, only run it when you expect a
brief interruption. It refuses to run until the active engine has been up for
at least 60 seconds, while USB setup is still settling. After the restart, give
the screen and USB audio roughly a minute to settle. The USB link itself stays
up across the restart, so audio apps on your computer keep their device handle.

Run `just mixer-stop` or reboot the mixer to exit a RAM run. After rebooting,
the mixer boots whatever is installed in `/opt/rc2fx` (or stock if nothing is
installed).

Settings live in `/opt/rc2fx/data` and persist across reinstalls and switches
between `mixer-run` and `mixer-install`.

## Logs and status

```sh
just mixer-status    # what's running
just mixer-logs      # recent RC2FX, controller and mixer logs
just mixer-xruns     # JACK errors recorded over the next 60 s
just mixer-xruns 0   # all JACK errors since JACK started
```

The JACK error recorder loads into RØDE's JACK server whenever RC2FX starts,
capturing driver and ALSA xruns and identifying clients that missed their
deadline. Records stay in RAM and clear on reboot. See the
[recorder's README](../../mixer/jackerr/README.md).

## Recovering

- If the mixer becomes unresponsive, power-cycle it. A software reboot doesn't
  always clear a wedged state.
- If the audio engine exits early twice in a single boot, or fails to become
  ready, the mixer automatically falls back to stock until the next reboot.
- Use `just mixer-disable` to prevent an installed RC2FX from starting at boot,
  and `just mixer-enable` to re-enable it.
- Don't touch the mixer during its first boot after a firmware flash; it
  updates its internal controllers and reboots on its own.

### Repairing a mixer that forgets its setup

If a mixer prompts for initial setup on every boot, its settings partition
failed to mount. RØDE's startup script (`/usr/bin/rode-init-update-env.sh`)
mounts that partition (and reformats it if mounting fails). Without it, setup
writes to an empty directory on the root filesystem that gets wiped on the next
boot, the USB serial falls back to `RD0000000` (or the model's letters plus
zeros), and RØDE's engine fills up the root partition with its 286 MB
update-drive image. The installer's
[image check](../reference/installer.md#image-check) automatically repairs the
known cause when RC2FX is running. If RC2FX isn't running, you can recover via
the bootloader:

1. Connect USB 1 to a Linux machine. Starting with the mixer powered off, hold
   the power button for about 15 seconds. The screen stays on the splash image
   and U-Boot exposes the entire eMMC as a USB mass-storage device
   (`1b8e:fada`, "UMS disk 0", 7.3 GB). Unplugging power exits this mode.
2. Mark every partition read-only before inspecting anything:
   `sudo blockdev --setro /dev/sdX /dev/sdX[1-4]`. The DOS partition table has
   p1 (ext4, 507 MB) and p2 (ext4, 583 MB) as the two root slots, p3 (FAT,
   835 MB) for `/Application/update-staging` (settings, `unit-serial.txt`, and
   the update-drive image), and p4 (FAT, 4.9 GB) for `/Application/emmc-data`.
   Check `root=` in `/boot/extlinux/extlinux.conf` to see which root slot is
   active.
3. Run `fsck.vfat -n` on p3 and mount it read-only to confirm your setup is
   intact, then back it up. Mount the root partitions with `-o ro,noload`,
   compare the active root against the flashed archive
   (`~/.local/share/RC2FX/installer/archive.tar.gz` on Linux), and check free
   space with `df`.
4. To repair, switch only the affected partition to read-write
   (`blockdev --setrw`), restore any damaged file from the archive or the other
   root slot, delete `usb-update.img` and any empty settings files from the
   root's `/Application/update-staging` if the root filled up, and optionally
   add a developer key to `/home/root/.ssh/authorized_keys` (mode 600) if you
   need SSH. Unmount it, flip it back to read-only, and run `fsck -n`.
5. Unmount everything, **disconnect USB 1** and power, wait 30 seconds, and
   power the mixer back on normally. (In the recovery observed so far, the
   mixer wouldn't boot until USB 1 was unplugged.)

Note that **Reset Device** in the RØDECaster App zeroes the first 25 MB of p4
and reformats it, wiping SMART pad sounds. Their audio data often remains on p4
past that boundary and can be carved from a read-only image of p4.

## Linking `target/mixer` to another bundle

`target/mixer` is normally a build output directory. If you replace it with a
symlink pointing to another bundle, the build refuses to overwrite it, and any
recipe that rebuilds first stops at that guard. Run `./xtask mixer install` to
deploy the symlinked bundle as-is, or remove the symlink to go back to normal
builds.
