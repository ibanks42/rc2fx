package installflow

import (
	"fmt"
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// advance carries out the next unsatisfied step, or waits for the mixer.
func (m *Model) advance() *installer.Request {
	for {
		run := m.Run
		if run == nil {
			return nil
		}
		if run.status.Kind == RunFailed || m.Busy() {
			return nil
		}
		if m.Status != nil && (len(m.Status.USB) > 1 ||
			slices.ContainsFunc(m.Status.USB, func(u installer.USBMixer) bool {
				return u.Model != run.Device || (run.Serial != "" && u.Serial != run.Serial)
			})) {
			return m.fail("The connected mixer changed. Reconnect the original model and serial; nothing else will be flashed.")
		}
		if run.Device == firmware.ModelDuo && !m.AllowDuo {
			return m.fail("Experimental Duo flashing is disabled.")
		}
		if run.Serial == "" && m.Status != nil && len(m.Status.USB) > 0 {
			run.Serial = m.Status.USB[0].Serial
		}
		if run.Index < 0 || run.Index >= len(Steps) {
			m.finished = m.Run
			m.Run = nil
			if m.Status != nil {
				if c := m.Status.Saved.LastCommand(); c != nil && c.Command == "flash" {
					at := c.AtUnix
					m.SettledFlash = &at
				}
			}
			return nil
		}
		step := Steps[run.Index]
		if m.satisfied(step) {
			run.Index++
			run.resume()
			continue
		}
		serial := run.Serial
		switch step {
		case StepBackup:
			if !m.Answering() {
				return m.fail("RC2FX stopped answering before its settings were saved, so they were not saved and nothing was changed. " +
					"Reconnect the mixer and choose Try again, or choose Stop and start again to continue without saving them.")
			}
			if serial == "" {
				return m.wait("Connect the mixer so its settings can be matched to it.")
			}
			if m.settingsRead == SettingsReadIdle {
				m.settingsRead = SettingsReadWanted
			}
			return m.wait("Reading RC2FX's settings over USB.")
		case StepMixer:
			profile, hasProfile := m.firmwareProfile(run.Choice)
			switch {
			case run.Mixer != nil && run.Mixer.Kind == MixerSourceSigned && hasProfile:
				offer := cloneOffer(run.Mixer.Offer)
				return m.send(installer.Request{
					Kind:            installer.RequestSelectMixer,
					Offer:           &offer,
					FirmwareProfile: profile,
				})
			case run.Mixer != nil && run.Mixer.Kind == MixerSourceBundled:
				return m.send(installer.Request{Kind: installer.RequestSelectBundledMixer})
			default:
				return m.fail("Choose the mixer software to install.")
			}
		case StepPrepare:
			chosen := run.Choice
			if chosen == nil {
				return m.fail("Choose a firmware version.")
			}
			return m.send(installer.Request{
				Kind:    installer.RequestPrepare,
				Channel: chosen.Channel,
				Version: chosen.Version,
				Serial:  serial,
				Stock:   run.Stock,
			})
		case StepUSBMode:
			usb := m.USB()
			// Run the storage fix from normal mode; it flashes the final firmware itself.
			if fix := run.Fix; fix != nil && fix.Reply == nil && fix.Error == "" &&
				len(usb) == 1 && !usb[0].UpdateMode {
				run.Committed = true
				fix.Event = nil
				return m.send(installer.Request{Kind: installer.RequestStorageFix, Final: true})
			}
			switch {
			case len(usb) == 0:
				return m.wait("Connect the mixer to this computer.")
			case len(usb) == 1 && !usb[0].UpdateMode:
				serial := usb[0].Serial
				if run.status.Kind == RunWaiting && m.usbModeSent() {
					return m.wait("Waiting for the mixer to switch to USB update mode.")
				}
				run.Committed = true
				return m.send(installer.Request{Kind: installer.RequestEnterUSBMode, Serial: serial})
			default:
				return m.fail("More than one RØDECaster is connected. Connect only one.")
			}
		case StepFlash:
			one := m.usbOne(true)
			// Do not flash normally if the storage fix is still pending; wait
			// for the mixer to leave update mode so the fix can rerun from normal mode.
			if fix := run.Fix; fix != nil && fix.Reply == nil && fix.Error == "" {
				if one != nil {
					return m.waitFor("The mixer is still in USB update mode from the storage fix. Once it is back on, the fix runs again.", true)
				}
				if m.usbOne(false) != nil {
					run.Index = slices.Index(Steps[:], StepUSBMode)
					run.resume()
					continue
				}
			}
			switch {
			case one != nil && m.Status != nil && m.Status.UpdateDrive:
				serial := one.Serial
				run.Committed = true
				return m.send(installer.Request{Kind: installer.RequestFlash, Serial: serial})
			case one != nil:
				return m.wait("Waiting for the mixer's update drive to appear on this computer.")
			default:
				return m.wait("Waiting for the mixer to enter USB update mode.")
			}
		default: // StepStart
			if run.Stock {
				if len(m.USB()) == 0 {
					return m.wait("The mixer is installing the firmware and restarting. Keep it connected to see when it is back.")
				}
				return m.wait("The mixer is installing the firmware and restarting.")
			}
			service := m.Service()
			if service != nil && service.Kind == installer.ServiceStarting {
				return m.wait(fmt.Sprintf("RC2FX is starting: %s", service.Detail))
			}
			if service != nil && service.Kind == installer.ServiceReady {
				if check := &m.imageCheck.result; m.imageCheck.kind == imageCheckAnswered {
					switch check.Status {
					case core.ImageStatusDamaged:
						return m.fail(damagedMessage(check))
					case core.ImageStatusRepaired:
						return m.waitFor(repairedMessage(check), true)
					}
				}
				return m.wait("RC2FX is running and is checking the files the flash installed. This takes a minute or two.")
			}
			return m.wait("The mixer is updating and will restart a couple of times. Leave it on; this takes a few minutes.")
		}
	}
}

func fileCount(check *core.ImageCheck, names []string) string {
	n := len(names) + int(check.Unlisted)
	if n == 1 {
		return "1 of RØDE's files"
	}
	return fmt.Sprintf("%d of RØDE's files", n)
}

func repairedMessage(check *core.ImageCheck) string {
	return fmt.Sprintf("The update damaged %s on the mixer, and RC2FX restored them from the mixer's previous system. "+
		"The mixer needs to be switched off and on once so it starts with the restored files.",
		fileCount(check, check.Repaired))
}

func damagedMessage(check *core.ImageCheck) string {
	return fmt.Sprintf("The update damaged %s on the mixer, and there was no undamaged copy on the mixer to restore them from. "+
		"Press “Try again” to send the firmware again. Until then the mixer may forget its setup at every restart; don't redo it if it asks.",
		fileCount(check, check.Damaged))
}

func (m *Model) usbModeSent() bool {
	if m.Status == nil {
		return false
	}
	c := m.Status.Saved.LastCommand()
	return c != nil && c.Command == "usb-mode" && num.SatSub(nowUnix(), c.AtUnix) < 120
}

// Tick returns a background probe request when due (every 3s while waiting or
// failed, every 10s when idle).
func (m *Model) Tick(now time.Time) *installer.Request {
	if m.Busy() || m.polling() {
		return nil
	}
	var interval time.Duration
	switch {
	case m.Run != nil && (m.Run.status.Kind == RunWaiting || m.Run.status.Kind == RunFailed):
		interval = 3 * time.Second
	case m.Run != nil:
		return nil
	default:
		interval = 10 * time.Second
	}
	if !m.lastPoll.IsZero() && now.Sub(m.lastPoll) < interval {
		return nil
	}
	return m.refresh()
}

// Finish processes a completed request and returns the next request to send, if any.
func (m *Model) Finish(request installer.Request, reply installer.Reply, err error) *installer.Request {
	background := len(m.queue) > 0 && m.queue[0].background
	next := m.finish(request, reply, err)
	// If a background check was the last queued request while a run is actively
	// advancing, kick off a refresh now since Tick only polls waiting/failed runs.
	if next == nil && background && m.Run != nil && m.Run.status.Kind == RunGoing && !m.Busy() && !m.polling() {
		return m.refresh()
	}
	return next
}

func (m *Model) finish(request installer.Request, reply installer.Reply, err error) *installer.Request {
	background := false
	if len(m.queue) > 0 {
		background = m.queue[0].background
		m.queue = m.queue[1:]
	}
	// A canceled run ends when its request returns, whatever it says.
	if !background && m.Canceling() {
		m.Run = nil
		return m.refresh()
	}
	if err != nil {
		return m.failed(request, err)
	}
	r := reply
	switch r.Kind {
	case installer.ReplyStatus:
		var status installer.Status
		if r.Status != nil {
			status = *r.Status
		}
		return m.receiveStatus(status)
	case installer.ReplyReleases:
		m.FirmwareCheck = FirmwareCheckDone
		m.Error = ""
		device := m.Device.ID()
		var items, unavailable []installer.Release
		for _, release := range r.Releases {
			if release.Model != "" && release.Model != device {
				continue
			}
			if release.Supported {
				items = append(items, release)
			} else {
				unavailable = append(unavailable, release)
			}
		}
		m.NeedsApp = nil
		for _, release := range unavailable {
			if model.AppTooOld(release.MinimumAppVersion) {
				m.NeedsApp = append(m.NeedsApp, release)
			}
		}
		if m.Choice == nil || !slices.ContainsFunc(items, func(r installer.Release) bool {
			return r.Channel == m.Choice.Channel && r.Version == m.Choice.Version
		}) {
			m.Choice = defaultChoice(items)
		}
		m.Releases = items
		m.settleMixer()
		// The catalog was read, so mixer software is looked for even when it
		// lists no firmware for this mixer.
		if next := m.checkMixers(); next != nil {
			return next
		}
		return m.nextCheck()
	case installer.ReplyMixerReleases:
		m.Offers = Offers{Kind: OffersReady, List: r.Offers}
		m.settleMixer()
		return m.nextCheck()
	case installer.ReplyStockReleases:
		device := m.Device.ID()
		var items []installer.Release
		for _, release := range r.Releases {
			if release.Model == "" || release.Model == device {
				items = append(items, release)
			}
		}
		if m.StockChoice == nil || !slices.ContainsFunc(items, func(r installer.Release) bool {
			return r.Channel == m.StockChoice.Channel && r.Version == m.StockChoice.Version
		}) {
			m.StockChoice = nil
			pick := slices.IndexFunc(items, func(r installer.Release) bool { return r.Channel == "stable" })
			if pick < 0 && len(items) > 0 {
				pick = 0
			}
			if pick >= 0 {
				m.StockChoice = &Choice{Channel: items[pick].Channel, Version: items[pick].Version}
			}
		}
		m.StockReleases = items
		m.StockCheck = StockCheck{Kind: StockCheckDone}
		return m.nextCheck()
	case installer.ReplyInPlaceBundle:
		var bundle installer.InPlaceBundle
		if r.InPlaceBundle != nil {
			bundle = *r.InPlaceBundle
		}
		m.inPlace = InPlace{Kind: InPlaceReady, Bundle: bundle}
	case installer.ReplyMixerSelected:
		release := ""
		if r.Identity != nil {
			release = r.Identity.Release
		}
		if run := m.Run; run != nil {
			if run.Mixer != nil && run.Mixer.Version() == release {
				run.Selected = true
			} else {
				wanted := ""
				if run.Mixer != nil {
					wanted = run.Mixer.Version()
				}
				m.fail(fmt.Sprintf(
					"The verified mixer software is %s rather than the %s you chose. Nothing was flashed.",
					release, wanted))
			}
		}
	case installer.ReplyStorageFix:
		if run := m.Run; run != nil && run.Fix != nil && r.StorageFix != nil {
			fix := *r.StorageFix
			run.Fix.Reply, run.Fix.Event = &fix, nil
			switch {
			case fix.FinalSent:
				// The run's firmware is going onto the fixed slot.
				run.Flashed = true
			case fix.Unsafe:
				// The slot a flash would format is still too small for 4 KB
				// blocks: no flash now, and trying again runs the fix again
				// (replan).
				m.fail(fix.Error)
			case fix.Touched:
				// Stopped part way: what is left follows the mixer's state,
				// and trying again installs with a normal flash.
				m.fail(fmt.Sprintf("The storage fix stopped before it finished. %s", fix.Error))
			default:
				// The mixer was not touched: install with a normal flash.
				run.Committed = run.Flashed || m.InUpdateMode()
			}
		}
	case installer.ReplyBackup:
		if m.Run != nil {
			m.Run.BackedUp = true
		}
	case installer.ReplyPrepared:
		if m.Run != nil {
			m.Run.Prepared = true
		}
	case installer.ReplyDone:
		switch request.Kind {
		case installer.RequestFlash:
			if m.Run != nil {
				m.Run.Flashed = true
			}
		case installer.RequestEnterUSBMode:
			if m.Running() {
				// A new wait, not the one before the request.
				m.Run.resume()
				m.wait("Waiting for the mixer to switch to USB update mode.")
			}
		}
	}
	return m.refresh()
}

// failed applies a request that failed with err.
func (m *Model) failed(request installer.Request, err error) *installer.Request {
	e := err.Error()
	switch request.Kind {
	case installer.RequestMixerReleases:
		m.Offers = Offers{Kind: OffersFailed, Error: e}
		m.settleMixer()
		return m.nextCheck()
	case installer.RequestStockReleases:
		m.StockCheck = StockCheck{Kind: StockCheckFailed, Error: e}
		return m.nextCheck()
	case installer.RequestProbe:
		// A failed poll while waiting is not the end of the run.
		if m.Run != nil {
			return m.advance()
		}
		m.Error = e
		return nil
	case installer.RequestInPlaceBundle:
		message := fmt.Sprintf("Couldn't get the update ready. Nothing changed on the mixer. %s", e)
		if canceled(err) {
			message = "The update was canceled. Nothing changed on the mixer."
		}
		m.inPlace = InPlace{Kind: InPlaceFailed, Error: message}
		return nil
	case installer.RequestStorageFix:
		if run := m.Run; run != nil && run.Fix != nil {
			run.Fix.Error, run.Fix.Event = e, nil
		}
		m.fail(e)
		return m.refresh()
	case installer.RequestReleases:
		m.FirmwareCheck = FirmwareCheckDone
		if canceled(err) {
			m.Error = "The check for supported firmware was canceled."
		} else {
			m.Error = fmt.Sprintf("Couldn't check for supported firmware. %s", e)
		}
		// Stock firmware doesn't depend on RC2FX's catalog; mixer software
		// waits for it (see nextCheck).
		return m.nextCheck()
	default:
		m.fail(e)
		// Show what the mixer reports after the failure.
		return m.refresh()
	}
}

func (m *Model) receiveStatus(status installer.Status) *installer.Request {
	first := m.Status == nil
	if m.Run == nil && len(status.USB) == 1 && status.USB[0].Model != m.Device {
		m.Device = status.USB[0].Model
		m.Choice = nil
		m.forgetStock()
		m.Releases = nil
		m.NeedsApp = nil
		m.FirmwareCheck = FirmwareCheckUnchecked
		m.Error = ""
	}
	m.Status = &status
	if p := m.Prepared(); p != nil {
		slot := &m.Choice
		if p.Stock {
			slot = &m.StockChoice
		}
		if *slot == nil {
			*slot = &Choice{Channel: p.Channel, Version: p.Version}
		}
	}
	// If opened while a recent flash is still completing, resume waiting for it
	// (only on the first status reply, and only while no normal-mode mixer is seen).
	if first && m.Run == nil && m.recentlyFlashed() {
		if p := m.Prepared(); p != nil {
			m.Stock = p.Stock
			m.finished = nil
			m.Run = &Run{
				Started:  time.Now(),
				Device:   m.Device,
				Stock:    p.Stock,
				Choice:   &Choice{Channel: p.Channel, Version: p.Version},
				BackedUp: true,
				Prepared: true,
				Flashed:  true,
			}
		}
	}
	if m.Run != nil {
		if next := m.advance(); next != nil {
			return next
		}
		if m.flashed() {
			return m.checkFirmware()
		}
		return nil
	}
	return m.nextCheck()
}

// nextCheck advances the startup catalog checks in order: RC2FX firmware
// (skipped while USB enumeration is failing), signed mixer releases, then
// RØDE stock firmware. Checks run one at a time.
func (m *Model) nextCheck() *installer.Request {
	usbFailed := m.Status != nil && m.Status.USBError != nil
	if !usbFailed {
		if next := m.checkFirmware(); next != nil {
			return next
		}
		if m.FirmwareCheck == FirmwareCheckChecking {
			return nil
		}
	}
	if len(m.Releases) > 0 || usbFailed {
		if next := m.checkMixers(); next != nil {
			return next
		}
		if m.Offers.Kind == OffersChecking {
			return nil
		}
	}
	return m.checkStock()
}

func defaultChoice(releases []installer.Release) *Choice {
	pick := slices.IndexFunc(releases, func(r installer.Release) bool {
		return r.Supported && r.Channel == "stable"
	})
	if pick < 0 {
		pick = slices.IndexFunc(releases, func(r installer.Release) bool { return r.Supported })
	}
	if pick < 0 {
		return nil
	}
	return &Choice{Channel: releases[pick].Channel, Version: releases[pick].Version}
}
