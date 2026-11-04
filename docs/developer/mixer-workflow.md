# Working on a mixer

Flashing firmware for each change is slow, so development uses SSH instead.
`just mixer-run` runs a build from the mixer's RAM and `just mixer-install`
installs it. The desktop app turns SSH access on and off over USB 1, but never
uses SSH itself.

## 1. Enable SSH access

RØDE's firmware runs an SSH server that only accepts root logins with a key,
and ships with no keys, so nobody can log in. RC2FX can add one key of its
own without reflashing:

1. With RC2FX installed and the mixer connected over USB 1, turn on
   **Settings › Developer › Developer mode** in the app.
2. Under **SSH access**, press **Enable** and confirm.

The app adds this computer's public key to root's `authorized_keys` on the
mixer, marked as RC2FX's, and audio keeps running. The key is
`~/.ssh/rc2fx_rodecaster_pro_ii`, created if it doesn't exist; the private
key never leaves the computer. The app also reads the mixer's SSH host key
over USB and pins it in the installer cache's `known_hosts`, under an alias
of the form `rc2fx-0123456789ab` derived from the mixer's serial number, so
you don't have to trust it on first connection. The app then shows the exact
`ssh` command for this mixer.

The status line says which key is on:

- **On with this computer's key** and its `SHA256:` fingerprint, when the
  managed key is this computer's.
- **On with an RC2FX key from another computer**, when the managed key is a
  different computer's.
- **On with an RC2FX-managed key**, when this computer's key can't be read.

If other keys can also log in, for example one added by an earlier
installer's SSH option or by hand, the app says how many. RC2FX didn't add
them and never changes or removes them.

**Disable** removes only RC2FX's key. That stops new logins with it; sessions
already open stay open until they end. Turning **Developer mode** off only
hides these controls: it doesn't remove access. The key also goes away when
the mixer is flashed again, since an update replaces the root filesystem.

Enabling and disabling are refused during the first boot after a firmware
update, and if `/home/root/.ssh` or `authorized_keys` isn't safe to change: a
link, owned by someone else, writable by others, or containing NUL bytes.

## 2. Configure the mixer address

The mixer must be on your network. Copy `.rc2fx-mixer.conf.example` to
`.rc2fx-mixer.conf` (git-ignored) and set its address:

```ini
ip = 192.168.0.100
# key = ~/.ssh/rc2fx_rodecaster_pro_ii
# known_hosts = ~/.local/share/RC2FX/installer/known_hosts
# host_key_alias = rc2fx-0123456789ab
```

Set `known_hosts` to the file in the installer's cache folder, and
`host_key_alias` to the alias the app shows in its `ssh` command:

| Platform | Cache folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%\RC2FX\installer` |
| macOS | `~/Library/Application Support/RC2FX/installer` |
| Linux | `~/.local/share/RC2FX/installer` |

The environment variables `RC2FX_MIXER_IP`, `RC2FX_MIXER_USER`,
`RC2FX_MIXER_KEY`, `RC2FX_MIXER_KNOWN_HOSTS` and
`RC2FX_MIXER_HOST_KEY_ALIAS` override the file.

Test the connection:

```sh
just mixer-status
```

## 3. Run or install a build

```sh
just mixer-run       # run this build from RAM until the next reboot
just mixer-install   # install this build to /opt/rc2fx and start it at every boot
just mixer-uninstall # remove RC2FX from the mixer (--purge also deletes settings)
```

Both `mixer-run` and `mixer-install` rebuild first.

`mixer-run` copies the build into RAM and restarts the audio engine into it
through RØDE's launcher. This restarts audio, so only run it when the person
using the mixer expects that. The command is refused until the running engine
is at least 60 seconds old, because the USB connection is still being set up
before then. After the restart, allow about a minute for the screen and USB
audio to settle. The USB connection stays up during the restart, so audio apps
on the computer keep their device.

`just mixer-stop` or a reboot ends a RAM run. After a reboot the mixer runs
the installed build, or stock firmware if none is installed.

Settings are stored in `/opt/rc2fx/data`. They are kept across reinstalls and
when switching between `mixer-run` and `mixer-install`.

## Logs and status

```sh
just mixer-status    # what's running
just mixer-logs      # recent RC2FX, controller and mixer logs
just mixer-xruns     # JACK errors recorded over the next 60 s
just mixer-xruns 0   # all JACK errors since JACK started
```

The JACK error recorder is loaded into RØDE's JACK server each time RC2FX
starts. It logs driver and ALSA xruns and names clients that missed their
deadline. Records are kept in RAM and cleared on reboot. See the
[recorder's README](../../crates/rc2fx-native/jackerr/README.md).

## Recovering

- If the mixer stops responding, power-cycle it. A software reboot doesn't
  always recover it.
- If the engine exits early twice in one boot, or doesn't become ready, the
  mixer runs stock until the next boot.
- `just mixer-disable` stops an installed RC2FX from starting at boot.
  `just mixer-enable` turns it back on.
- Leave the mixer alone during its first boot after a firmware update. It
  flashes its controllers and restarts by itself.

## Linking `target/mixer` to another bundle

`target/mixer` is normally a build directory. If it is a symlink to another
bundle, the build won't overwrite it, and recipes that rebuild first stop at
that check. Use `cargo xtask mixer install` to deploy the linked bundle as it
is, or remove the link to return to normal builds.
