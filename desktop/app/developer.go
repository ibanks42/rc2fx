package app

// Developer mode UI for managing SSH access on the connected mixer over USB 1.
// Toggling developer mode off only hides the controls; only Disable removes the key.

import (
	"path/filepath"
	"strings"

	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/editor"
	"github.com/ibanks42/goui-kit/gk/kit"
	"github.com/ibanks42/goui-kit/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/developer"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

type SSH struct{ developer.SSH }

func (s *SSH) clear() { s.Clear() }

func addressInput(cx *gk.Cx) editor.InputState {
	input := editor.NewInputState(cx).Placeholder("192.168.1.20").
		Validate(func(_ *gk.Cx, text string) bool { return developer.ValidAddress(text) }).
		Build()
	input.OnInputEvent(cx, func(cx *gk.Cx, event editor.InputEvent) {
		if _, change := event.(editor.InputEventChange); !change {
			gk.Quiet(cx)
		}
	})
	return input
}

func (a *App) developerVisible() bool {
	return a.developerMode && a.session.Connected && a.streamConnected
}

func (a *App) readSSH() {
	if a.developerVisible() && !a.ssh.Change.Sent() {
		id := a.ssh.Track(false)
		if !a.command(transport.CommandSSH{Generation: a.generation, ID: id}) {
			a.ssh.Untrack(id)
		}
	}
}

func (a *App) setDeveloperMode(cx *gk.Cx, on bool) {
	if err := rememberDeveloper(on); err != nil {
		// Leave the toggle unchanged if persisting it failed.
		a.message = "Could not save Developer mode: " + err.Error()
	} else {
		a.developerMode = on
		a.ssh.clear()
		a.readSSH()
	}
	a.notify(cx)
}

type pinOutcome struct {
	pinned *developer.Pinned
	local  string
}

func (a *App) receiveSSH(cx *gk.Cx, id uint64, status *core.SSHStatus, err error) {
	if !a.ssh.AcceptReply(id) {
		return
	}
	// Clear any previous host pin before processing the new status.
	a.ssh.Pinned = nil
	if err == nil && status != nil && a.connectedMixer != nil {
		serial := *a.connectedMixer
		keys := status.HostKeys
		epoch := a.ssh.Epoch
		gk.Go(cx, func() pinOutcome {
			var pinned *developer.Pinned
			switch {
			case mockMixer:
				// Build a synthetic host entry for the mock mixer without touching known_hosts.
				pinned = mockPinned(serial)
			case len(keys) > 0:
				host, err := installer.PinMixerHostKeys(serial, keys)
				pinned = &developer.Pinned{Serial: serial, Host: host, Err: err}
			}
			return pinOutcome{pinned, developer.ReadLocalFingerprint()}
		}, func(cx *gk.Cx, r pinOutcome) {
			if a.ssh.Epoch == epoch && a.connectedMixer != nil && *a.connectedMixer == serial {
				a.ssh.LocalFingerprint = r.local
				a.ssh.Pinned = r.pinned
				a.notify(cx)
			}
		})
	}
	a.ssh.Status = &developer.Reading{Status: status, Err: err}
}

func (a *App) sendSSH(request core.SSHRequest) {
	id := a.ssh.Track(true)
	if a.command(transport.CommandSetSSH{Generation: a.generation, ID: id, Request: request}) {
		a.ssh.Change = developer.ChangeDisabling
		if request.Enabled {
			a.ssh.Change = developer.ChangeEnabling
		}
	} else {
		a.ssh.Untrack(id)
	}
}

// sameConnection checks that the mixer connection hasn't changed since the user confirmed.
func (a *App) sameConnection(epoch uint64, serial *string) bool {
	return a.ssh.Epoch == epoch && opt.Equal(a.connectedMixer, serial) && a.developerVisible()
}

func (a *App) enableSSH(cx *gk.Cx, epoch uint64, serial *string) {
	if !a.sameConnection(epoch, serial) {
		a.message = "The mixer connection changed; SSH was not enabled."
		return
	}
	if a.ssh.Change.Busy() {
		a.message = "SSH access is already changing; try again when it is done."
		return
	}
	a.ssh.Change = developer.ChangePreparingKey
	type prepared struct {
		key string
		err error
	}
	gk.Go(cx, func() prepared {
		key, err := installer.SSHPublicKey()
		return prepared{key, err}
	}, func(cx *gk.Cx, p prepared) {
		if a.ssh.Epoch == epoch && a.ssh.Change == developer.ChangePreparingKey {
			a.ssh.Change = developer.ChangeNone
		}
		switch {
		case !a.sameConnection(epoch, serial):
			a.message = "The mixer connection changed; SSH was not enabled."
		case p.err != nil:
			a.message = "Could not prepare the SSH key: " + p.err.Error()
		default:
			key := p.key
			a.sendSSH(core.SSHRequest{Enabled: true, PublicKey: &key})
		}
		a.notify(cx)
	})
}

func (a *App) confirmEnableSSH(cx *gk.Cx) {
	epoch, serial := a.ssh.Epoch, opt.Clone(a.connectedMixer)
	a.confirm(cx, confirmKindOther, Confirm{
		Eyebrow: "Developer mode",
		Title:   "Enable SSH access?",
		Detail: "This adds this computer's RC2FX key to the mixer, so anyone with that key " +
			"can log in as root over the network. Audio keeps running and nothing is " +
			"flashed. You can disable it here at any time.",
		Cancel: "Cancel",
		Action: "Enable SSH",
	}, func(cx *gk.Cx) {
		a.enableSSH(cx, epoch, serial)
		a.notify(cx)
	}, nil)
}

func (a *App) disableSSH(epoch uint64, serial *string) {
	if !a.sameConnection(epoch, serial) {
		a.message = "The mixer connection changed; SSH was not disabled."
		return
	}
	if a.ssh.Change.Busy() {
		a.message = "SSH access is already changing; try again when it is done."
		return
	}
	a.sendSSH(core.SSHRequest{Enabled: false})
}

func (a *App) confirmDisableSSH(cx *gk.Cx) {
	epoch, serial := a.ssh.Epoch, opt.Clone(a.connectedMixer)
	a.confirm(cx, confirmKindOther, Confirm{
		Eyebrow: "Developer mode",
		Title:   "Disable SSH access?",
		Detail: "This removes the RC2FX-managed key, so new logins with it stop working. " +
			"Sessions already open stay open until they end.",
		Cancel: "Cancel",
		Action: "Disable SSH",
	}, func(cx *gk.Cx) {
		a.disableSSH(epoch, serial)
		a.notify(cx)
	}, nil)
}

// TODO(B1 styled text): highlight the address placeholder in amber once gk
// exposes gpui's StyledText::with_highlights.
func highlightedCommand(command string) gk.Element {
	return txt(command)
}

func (a *App) sshLogin(cx *gk.Cx, command string) [2]gk.DivEl {
	mono := gk.KitTheme(cx).MonoFontFamily(cx)
	return [2]gk.DivEl{
		detailRow("Mixer address", gk.VFlex().Gap(px(6)).
			Child(gk.Div().MaxW(px(240)).Child(editor.Input(a.sshAddress))).
			Child(gk.Div().TextSize(px(12.5)).TextColor(rgb(Faint)).
				Child(txt("On the mixer, under Settings › Network.")))).
			Style(s.ItemsStart),
		detailRow("Login command", codeBox(mono).Px(px(12)).Py(px(9)).Pr(px(76)).TextColor(rgb(Text)).
			Child(highlightedCommand(command)).
			Child(gk.Div(s.Absolute).Top(px(6)).Right(px(6)).
				Child(a.copyButton("copy-ssh-login", command, cx)))).
			Style(s.ItemsStart),
	}
}

func (a *App) sshRows(cx *gk.Cx) gk.DivEl {
	change := a.ssh.Change
	local := a.ssh.LocalFingerprint
	summary := developer.Describe(a.ssh.Status, change, local)
	var reported *core.SSHStatus
	if st := a.ssh.Status; st != nil && st.Err == nil && st.Status != nil && !change.Busy() && st.Status.Managed {
		reported = st.Status
	}
	var pinned *developer.Pinned
	if p := a.ssh.Pinned; p != nil && a.connectedMixer != nil && p.Serial == *a.connectedMixer {
		pinned = p
	}
	caution := summary.Caution
	var details []gk.DivEl
	if reported != nil {
		running := reported.ServerRunning == nil || *reported.ServerRunning
		var login *developer.Pinned
		if running && developer.UsesLocalKey(reported, local) {
			login = pinned
		}
		key, keyErr := installer.SSHKeyPath()
		switch {
		case login != nil && login.Err == nil && keyErr == nil:
			address := strings.TrimSpace(a.sshAddress.Value(cx))
			command := developer.LoginCommand(key, login.Host.KnownHosts, login.Host.Alias, address)
			rows := a.sshLogin(cx, command)
			details = append(details, rows[:]...)
		case login != nil && login.Err != nil:
			warning := "Could not save the mixer's host identity, so no login command is shown: " + login.Err.Error()
			if caution != "" {
				caution = caution + " " + warning
			} else {
				caution = warning
			}
		}
		if reported.ManagedFingerprint != nil {
			mono := gk.KitTheme(cx).MonoFontFamily(cx)
			details = append(details, detailRow("Key", gk.Div().FontFamily(mono).TextSize(px(12.5)).
				Child(txt(*reported.ManagedFingerprint))))
		}
	}
	var button gk.Element
	enable, offer := developer.Action(a.ssh.Status, change)
	switch {
	case offer && enable:
		button = ghost("enable-ssh", "Enable SSH").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.confirmEnableSSH(cx) })
	case offer:
		button = ghost("disable-ssh", "Disable SSH").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.confirmDisableSSH(cx) })
	case a.ssh.Status != nil && a.ssh.Status.Err != nil && !change.Busy():
		button = ghost("retry-ssh", "Try again").OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.ssh.Status = nil
			a.ssh.Pinned = nil
			a.readSSH()
			a.notify(cx)
		})
	}
	open := caution != "" || len(details) > 0
	controls := gk.HFlex(s.ItemsCenter).Gap(px(16))
	if summary.Flag != nil {
		controls = controls.Child(flag(summary.Flag.Text, summary.Flag.Tone))
	}
	if button != nil {
		controls = controls.Child(button)
	}
	r := row(labeled("SSH access", summary.Line), controls)
	if open {
		r = r.Style(s.BorderB0).Pb(px(12))
	}
	rows := gk.Div().Child(r)
	if !open {
		return rows
	}
	block := gk.VFlex(s.BorderB1).Gap(px(14)).Pb(px(18)).BorderColor(rgb(surface().Line))
	if caution != "" {
		block = block.Child(gk.Div().TextSize(px(13.5)).TextColor(rgb(Warn)).Child(txt(caution)))
	}
	for _, d := range details {
		block = block.Child(d)
	}
	return rows.Child(block)
}

func (a *App) developerSection(cx *gk.Cx) gk.DivEl {
	rows := gk.Div().Child(row(
		labeled("Developer mode",
			"Shows developer controls for the connected mixer. Turning it off only hides them: it does not remove access."),
		kit.Switch("developer-mode").Checked(a.developerMode).
			OnClick(func(cx *gk.Cx, on bool) { a.setDeveloperMode(cx, on) })))
	if a.developerMode {
		if a.developerVisible() {
			rows = rows.Child(a.sshRows(cx))
		} else {
			rows = rows.Child(row(labeled("SSH access", "Connect the mixer over USB 1 to manage SSH."), gk.Div()))
		}
	}
	return settingsSection("Developer", gk.Div(), rows)
}

func detailRow(label string, value gk.Element) gk.DivEl {
	return gk.HFlex(s.ItemsCenter).Gap(px(12)).TextSize(px(13.5)).TextColor(rgb(Muted)).
		Child(gk.Div(s.FlexShrink0).W(px(112)).Child(txt(label))).
		Child(gk.Div(s.Flex1, s.MinW0).Child(value))
}

func mockPinned(serial string) *developer.Pinned {
	work, ok := installer.DefaultWork()
	alias, err := installer.MixerHostAlias(serial)
	if !ok || err != nil {
		return nil
	}
	return &developer.Pinned{Serial: serial, Host: &installer.PinnedHost{
		Alias:      alias,
		KnownHosts: filepath.Join(work, "known_hosts"),
	}}
}
