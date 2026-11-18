//go:build rc2fxdev

package app

// The guided flows' states from made-up data, development builds only
// (packaging refuses rc2fxdev): RC2FX_DEV_PAGE=flow:<state> opens one, with
// no mixer, no helper and no network, so each can be screenshotted and
// compared with the approved mock-up. The states are the mock-up's:
//
//	A1 nothing connected      A2 found, no RC2FX      A3 finishing an update
//	A3s RC2FX starting        A4 waiting in update mode
//	A5 storage mode           A6 two mixers           A7 RØDECaster Duo
//	Aoff disconnected by the person
//	B1 before you start       B1v versions open       B1L Linux (password)
//	B2 updating the mixer     B3 waiting for it       B3L taking longer
//	B3Lh taking longer, "What can I do?" open
//	B4 failed while sending   B4b failed downloading  B4c access ended
//	B5 power cycle            B6 done
//	D1 remove: before         D2 remove: updating     D3 remove: done
//	B2F install, first of the fix's two updates       B6F done, storage fixed
//	G1 fix only: before       G2 fix only: running    G3 fixed
//	G4 already fixed          Gfail fix didn't finish Gunrec layout unrecognized
//	C1 update in place: before                        C2 sending
//	C3 done                   C4 went back
//	E1 repair: waiting for storage mode               E2 findings
//	E3 repaired
//	F1 first channel, from Done's "Open Mic 1"
//
// Nothing here runs a request: the fixture installer's runner is closed and
// its send does nothing (Installer.fixture).

import (
	"fmt"
	"os"
	"strings"
	"sync/atomic"
	"time"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

// devFlows: this build opens flow:<state> pages.
const devFlows = true

func fixtureMixer(model firmware.Model, serial string, updateMode bool) installer.USBMixer {
	m := installer.USBMixer{Model: model, Serial: serial, UpdateMode: updateMode}
	if !updateMode {
		v := "1.7.7"
		m.Firmware = &v
	}
	return m
}

var firstFixtures = map[string]installflow.Sighting{
	"A1":  {},
	"A2":  {USB: []installer.USBMixer{fixtureMixer(firmware.ModelProIi, "FX1", false)}},
	"A3":  {Flashed: true},
	"A3s": {USB: []installer.USBMixer{fixtureMixer(firmware.ModelProIi, "FX1", false)}, RC2FX: true},
	"A4":  {USB: []installer.USBMixer{fixtureMixer(firmware.ModelProIi, "FX1", true)}},
	"A5":  {Storage: true},
	"A6": {USB: []installer.USBMixer{fixtureMixer(firmware.ModelProIi, "FX1", false),
		fixtureMixer(firmware.ModelProIi, "FX2", false)}},
	"A7": {USB: []installer.USBMixer{fixtureMixer(firmware.ModelDuo, "FXD", false)}},
}

// fixtureProfile is the made-up RØDE firmware profile the fixtures' RC2FX
// releases support.
const fixtureProfile = "rodecaster-pro-ii-1.7.7"

func fixtureOffer(version string) installer.MixerOffer {
	return installer.MixerOffer{
		Target: "mixer-" + version + ".payload",
		SHA256: strings.Repeat("00", 32),
		Length: 1,
		Metadata: installer.MixerTarget{
			Version:           version,
			PayloadFormat:     1,
			ProtocolVersion:   uint32(core.MixerProtocolVersion),
			MinimumAppVersion: "0.0.1",
			FirmwareProfiles:  []string{fixtureProfile},
		},
	}
}

// fixtureModel is a model that has looked at one Pro II on RØDE 1.7.7 and
// found everything it needs to start.
func fixtureModel(updateMode bool) *installflow.Model {
	m := installflow.NewModel()
	m.Status = &installer.Status{USB: []installer.USBMixer{fixtureMixer(firmware.ModelProIi, "FX1", updateMode)}}
	m.Releases = []installer.Release{
		{Channel: "stable", Version: "1.7.7", Profile: fixtureProfile, Supported: true},
		{Channel: "stable", Version: "1.7.5", Profile: "rodecaster-pro-ii-1.7.5", Supported: true},
	}
	m.Choice = &installflow.Choice{Channel: "stable", Version: "1.7.7"}
	m.FirmwareCheck = installflow.FirmwareCheckDone
	m.Offers = installflow.Offers{Kind: installflow.OffersReady,
		List: []installer.MixerOffer{fixtureOffer("0.2.1"), fixtureOffer("0.2.0")}}
	source := installflow.Signed(fixtureOffer("0.2.1"))
	m.Mixer = &source
	m.StockReleases = []installer.Release{{Channel: "stable", Version: "1.7.7"}}
	m.StockChoice = &installflow.Choice{Channel: "stable", Version: "1.7.7"}
	m.StockCheck = installflow.StockCheck{Kind: installflow.StockCheckDone}
	fw := "1.7.7"
	m.InstalledFirmware = &fw
	return m
}

func (a *App) fixtureInstaller(cx *gk.Cx, kind taskKind, m *installflow.Model) *Installer {
	runner := &installerRunner{
		wake:       make(chan struct{}, 1),
		cancel:     new(atomic.Bool),
		helper:     &cancelerSlot{},
		privileged: &helperSlot{},
		closed:     true,
	}
	return &Installer{
		model:          m,
		runner:         runner,
		kind:           kind,
		controlsScroll: gk.NewScrollHandle(cx),
		access:         installflow.Access{Kind: installflow.AccessNotNeeded},
		stage:          m.Stage(),
		stageSince:     time.Now(),
		fixture:        true,
	}
}

// fixtureRun is a run of the reviewed release that got as far as index.
func fixtureRun(m *installflow.Model, index int) *installflow.Run {
	run := &installflow.Run{
		Started:  time.Now().Add(-5 * time.Minute),
		Index:    index,
		Serial:   "FX1",
		Device:   firmware.ModelProIi,
		BackedUp: index > 0,
		Selected: index > 1,
		Prepared: index > 2,
		Flashed:  index > 4,
		Stock:    m.Stock,
	}
	if c := m.Chosen(); c != nil {
		choice := *c
		run.Choice = &choice
	}
	if !m.Stock && m.Mixer != nil {
		source := *m.Mixer
		run.Mixer = &source
	}
	run.Committed = index > 3
	return run
}

func fixtureAgo(d time.Duration) time.Time { return time.Now().Add(-d) }

func fixtureSince(i *Installer, d time.Duration) {
	i.stage, i.stageSince = i.model.Stage(), fixtureAgo(d)
}

var taskFixtures = map[string]func(a *App, cx *gk.Cx) *Installer{
	"B1": func(a *App, cx *gk.Cx) *Installer { return a.fixtureInstaller(cx, taskInstall, fixtureModel(false)) },
	"B1v": func(a *App, cx *gk.Cx) *Installer {
		i := a.fixtureInstaller(cx, taskInstall, fixtureModel(false))
		i.versionsOpen = true
		return i
	},
	"B1L": func(a *App, cx *gk.Cx) *Installer {
		m := fixtureModel(false)
		m.Privileged = false
		i := a.fixtureInstaller(cx, taskInstall, m)
		i.access = installflow.Access{Kind: installflow.AccessNeeded}
		return i
	},
	"B2": func(a *App, cx *gk.Cx) *Installer { return updating(a, cx, false, 58) },
	"B3": func(a *App, cx *gk.Cx) *Installer { return waiting(a, cx, 112*time.Second, false) },
	"B3L": func(a *App, cx *gk.Cx) *Installer {
		return waiting(a, cx, 8*time.Minute+4*time.Second, false)
	},
	"B3Lh": func(a *App, cx *gk.Cx) *Installer {
		return waiting(a, cx, 8*time.Minute+4*time.Second, true)
	},
	"B4": func(a *App, cx *gk.Cx) *Installer {
		return failed(a, cx, 4, "The mixer stopped answering while the firmware was being copied to it.")
	},
	"B4b": func(a *App, cx *gk.Cx) *Installer {
		return failed(a, cx, 2, "Couldn't download RØDE's firmware: no answer from the download server.")
	},
	"B4c": func(a *App, cx *gk.Cx) *Installer {
		i := failed(a, cx, 3, "Administrator access ended. Grant it again to continue.")
		i.access = installflow.Access{Kind: installflow.AccessRefused, Why: "Administrator access ended. Grant it again to continue."}
		i.model.Privileged = false
		return i
	},
	"B5": func(a *App, cx *gk.Cx) *Installer {
		m := fixtureModel(false)
		m.Run = fixtureRun(m, 5)
		m.Run.ShowStatus(installflow.RunStatus{Kind: installflow.RunWaiting, Why: "Turn the mixer off and on again.", Since: fixtureAgo(30 * time.Second), PowerCycle: true})
		m.SetService(&installer.Service{Kind: installer.ServiceReady})
		m.ShowImageCheck(core.ImageCheck{Status: core.ImageStatusRepaired, Repaired: []string{"a", "b"}})
		i := a.fixtureInstaller(cx, taskInstall, m)
		fixtureSince(i, 4*time.Minute+42*time.Second)
		i.pausedAt = 4*time.Minute + 12*time.Second
		return i
	},
	"B6": func(a *App, cx *gk.Cx) *Installer { return finished(a, cx, false) },
	"D1": func(a *App, cx *gk.Cx) *Installer {
		m := fixtureModel(false)
		m.SetStock(true)
		return a.fixtureInstaller(cx, taskRemove, m)
	},
	"D2": func(a *App, cx *gk.Cx) *Installer { return updating(a, cx, true, 34) },
	"D3": func(a *App, cx *gk.Cx) *Installer { return finished(a, cx, true) },
	"B2F": func(a *App, cx *gk.Cx) *Installer {
		m := fixtureModel(false)
		m.Run = fixtureRun(m, 3)
		m.Run.Committed = true
		m.Run.Fix = &installflow.RunFix{}
		event := fixtureFixEvent(autofix.StageCopy, 1, true, 58)
		m.Run.Fix.Event = &event
		m.ShowBusy(installer.Request{Kind: installer.RequestStorageFix, Final: true})
		i := a.fixtureInstaller(cx, taskInstall, m)
		fixtureSince(i, 2*time.Minute+10*time.Second)
		return i
	},
	"B3F": func(a *App, cx *gk.Cx) *Installer {
		m := fixtureModel(false)
		m.Run = fixtureRun(m, 3)
		m.Run.Committed = true
		m.Run.Fix = &installflow.RunFix{}
		event := installer.Progress{Phase: installer.StorageFixPhase + autofix.StageWait, Flash: 1, Carrier: true,
			Completed: 34, Total: 288, Detail: "The mixer is running again; waiting for it to confirm the fix."}
		m.Run.Fix.Event = &event
		m.ShowBusy(installer.Request{Kind: installer.RequestStorageFix, Final: true})
		i := a.fixtureInstaller(cx, taskInstall, m)
		fixtureSince(i, 3*time.Minute+5*time.Second)
		i.waitLine, i.waitSince = fixWaitLine(m), fixtureAgo(34*time.Second)
		return i
	},
	"B6F": func(a *App, cx *gk.Cx) *Installer {
		i := finished(a, cx, false)
		i.model.LastRun().Fix = &installflow.RunFix{Reply: &installer.StorageFix{
			Outcome: string(autofix.OutcomeFixed), Backup: "rcfix-sector0", Touched: true, FinalSent: true}}
		return i
	},
	"G1": func(a *App, cx *gk.Cx) *Installer { return fixTask(a, cx, nil) },
	"G2": func(a *App, cx *gk.Cx) *Installer {
		i := fixTask(a, cx, nil)
		i.fix.Kind = installflow.FixRunning
		i.fix.Progress(fixtureFixEvent(autofix.StageCopy, 1, true, 41))
		i.fixStep = i.fix.Step()
		fixtureSince(i, 2*time.Minute+10*time.Second)
		return i
	},
	"G3": func(a *App, cx *gk.Cx) *Installer {
		return fixTask(a, cx, &installer.StorageFix{Outcome: string(autofix.OutcomeFixed), Backup: "rcfix-sector0"})
	},
	"G4": func(a *App, cx *gk.Cx) *Installer {
		return fixTask(a, cx, &installer.StorageFix{Outcome: string(autofix.OutcomeAlreadyFixed)})
	},
	"Gfail": func(a *App, cx *gk.Cx) *Installer {
		return fixTask(a, cx, &installer.StorageFix{Outcome: string(autofix.OutcomeFailed), Reason: "slot 2 could not be mounted"})
	},
	"Gunrec": func(a *App, cx *gk.Cx) *Installer {
		return fixTask(a, cx, &installer.StorageFix{Outcome: string(autofix.OutcomeUnrecognized), Reason: "partition 3 does not start where expected"})
	},
	"C1": func(a *App, cx *gk.Cx) *Installer {
		return inPlace(a, cx, installflow.InPlace{Kind: installflow.InPlaceIdle})
	},
	"C2": func(a *App, cx *gk.Cx) *Installer {
		i := inPlace(a, cx, installflow.InPlace{Kind: installflow.InPlaceUploading, Version: "0.2.2", Sent: 72, Total: 100})
		i.stageSince = fixtureAgo(41 * time.Second)
		return i
	},
	"C3": func(a *App, cx *gk.Cx) *Installer {
		i := inPlace(a, cx, installflow.InPlace{Kind: installflow.InPlaceDone, Version: "0.2.2", Committed: true})
		v := "0.2.2"
		i.model.Installed = &v
		return i
	},
	"C4": func(a *App, cx *gk.Cx) *Installer {
		return inPlace(a, cx, installflow.InPlace{Kind: installflow.InPlaceDone, Version: "0.2.2",
			Detail: "The previous version was put back: the new version did not start."})
	},
	"E1": func(a *App, cx *gk.Cx) *Installer {
		return repairTask(a, cx, installflow.RepairStage{Kind: installflow.RepairWaiting})
	},
	"E2": func(a *App, cx *gk.Cx) *Installer {
		return repairTask(a, cx, installflow.RepairStage{Kind: installflow.RepairFindings, Findings: &installer.RepairFindings{
			SettingsOK: true,
			Roots: []installer.RootFindings{
				{Index: 1, Damaged: []installer.DamagedFile{{Path: "a"}, {Path: "b"}}},
				{Index: 2},
			}}})
	},
	"E3": func(a *App, cx *gk.Cx) *Installer {
		return repairTask(a, cx, installflow.RepairStage{Kind: installflow.RepairDone,
			Result: &installer.RepairResult{Backup: "settings.img", Repaired: []string{"a", "b"}}})
	},
}

// fixtureFixEvent is the storage fix's progress at stage, in its flash-th
// flash, percent of the way through copying.
func fixtureFixEvent(stage string, flash int, carrier bool, percent uint64) installer.Progress {
	return installer.Progress{Phase: installer.StorageFixPhase + stage, Flash: flash, Carrier: carrier,
		Completed: percent, Total: 100, Detail: "Copying archive.tar.gz to the mixer"}
}

// fixTask is the storage fix on its own, before it starts (reply nil) or
// ended with reply.
func fixTask(a *App, cx *gk.Cx, reply *installer.StorageFix) *Installer {
	i := a.fixtureInstaller(cx, taskFix, fixtureModel(false))
	i.fix = installflow.NewFix()
	if reply != nil {
		reply.Firmware, reply.Touched, reply.FinalSent, reply.Flashes = "1.7.7", true, true, 2
		i.fix.Kind = installflow.FixEnded
		i.fix.Reply, i.fix.Touched = reply, true
	}
	i.fixStep = i.fix.Step()
	return i
}

// inPlace is an RC2FX update in place at p, on a mixer running RC2FX 0.2.1
// that installs signed releases itself and is offered 0.2.2.
func inPlace(a *App, cx *gk.Cx, p installflow.InPlace) *Installer {
	m := fixtureModel(false)
	newer := fixtureOffer("0.2.2")
	m.Offers.List = append([]installer.MixerOffer{newer}, m.Offers.List...)
	source := installflow.Signed(newer)
	m.Mixer = &source
	installed := "0.2.1"
	m.Installed = &installed
	m.SetService(&installer.Service{Kind: installer.ServiceReady})
	m.ShowInPlace(p)
	serial, profile, updates := "FX1", fixtureProfile, uint32(2)
	a.connectedMixer = &serial
	a.deviceVersions = &core.DeviceVersions{Runtime: &installed, FirmwareProfile: &profile, InPlaceUpdates: &updates}
	i := a.fixtureInstaller(cx, taskUpdate, m)
	i.inPlaceFrom, i.inPlaceKind = installed, p.Kind
	return i
}

// repairTask is Repair mixer at stage, with the mixer's storage seen.
func repairTask(a *App, cx *gk.Cx, stage installflow.RepairStage) *Installer {
	i := a.fixtureInstaller(cx, taskInstall, fixtureModel(false))
	i.repair = installflow.NewRepair()
	i.repair.Stage = stage
	i.repair.Wrote = stage.Kind == installflow.RepairDone
	if stage.Kind != installflow.RepairWaiting {
		size := uint64(7 << 30)
		i.repair.Disk = &size
	}
	return i
}

// openFirstChannel is F1: the channel Done's "Open Mic 1" opens, with its
// one-line hint, from the default catalog and no mixer. It pretends to be
// connected only so the page draws; nothing connects.
func (a *App) openFirstChannel(cx *gk.Cx) {
	a.link = linkIntent{}
	a.session = core.DefaultSession()
	a.session.Connected = true
	for index := range a.session.Draft {
		if a.session.Draft[index].ID == "analog-1" {
			a.effect = core.EffectSuppression
			a.openChannel(cx, index)
			a.firstHintDone, a.firstHintSaved = false, true
			a.firstHintTarget = a.session.Draft[index].ID
			return
		}
	}
}

func updating(a *App, cx *gk.Cx, stock bool, percent uint64) *Installer {
	m := fixtureModel(true)
	m.Status.UpdateDrive = true
	kind := taskInstall
	if stock {
		m.SetStock(true)
		kind = taskRemove
	}
	m.Run = fixtureRun(m, 4)
	m.ShowBusy(installer.Request{Kind: installer.RequestFlash, Serial: "FX1"})
	i := a.fixtureInstaller(cx, kind, m)
	i.progress = &installer.Progress{Phase: "stage", Completed: percent, Total: 100}
	fixtureSince(i, 3*time.Minute+48*time.Second)
	return i
}

func waiting(a *App, cx *gk.Cx, d time.Duration, help bool) *Installer {
	m := fixtureModel(false)
	m.Status.USB = nil
	m.Run = fixtureRun(m, 5)
	m.Run.ShowStatus(installflow.RunStatus{Kind: installflow.RunWaiting, Why: "The mixer is updating.", Since: fixtureAgo(d)})
	i := a.fixtureInstaller(cx, taskInstall, m)
	fixtureSince(i, d)
	i.waitHelp = help
	return i
}

func failed(a *App, cx *gk.Cx, index int, why string) *Installer {
	m := fixtureModel(false)
	m.Run = fixtureRun(m, index)
	m.Run.ShowStatus(installflow.RunStatus{Kind: installflow.RunFailed, Why: why})
	return a.fixtureInstaller(cx, taskInstall, m)
}

func finished(a *App, cx *gk.Cx, stock bool) *Installer {
	m := fixtureModel(false)
	kind := taskInstall
	if stock {
		m.SetStock(true)
		kind = taskRemove
	}
	m.ShowFinished(fixtureRun(m, len(installflow.Steps)))
	m.Status.PreparedPresent = true
	m.Status.Saved.Prepared = &installer.Prepared{Channel: "stable", Version: "1.7.7", Stock: stock, Settings: "{}"}
	return a.fixtureInstaller(cx, kind, m)
}

// openFlowFixture opens the flow state id from made-up data.
func (a *App) openFlowFixture(cx *gk.Cx, id string) {
	a.flowFixture = true
	a.link = linkIntent{phase: linkOn}
	if id == "Aoff" {
		a.link = linkIntent{}
		a.notify(cx)
		return
	}
	if id == "F1" {
		a.openFirstChannel(cx)
		a.notify(cx)
		return
	}
	if seen, ok := firstFixtures[id]; ok {
		a.look = &firstLook{seen: seen, usbOK: true}
		if id == "A7" {
			a.allowDuo = false
		}
		a.notify(cx)
		return
	}
	build, ok := taskFixtures[id]
	if !ok {
		fmt.Fprintf(os.Stderr, "RC2FX_DEV_PAGE: no flow state `%s`\n", id)
		return
	}
	a.installer = build(a, cx)
	a.notify(cx)
}
