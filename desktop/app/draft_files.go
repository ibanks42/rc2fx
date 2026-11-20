package app

// Opening a settings file into the draft and saving the
// draft as one. The file logic is in edit/files.go.

import (
	"fmt"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/core"
)

func (a *App) installDraft(cx *gk.Cx, snapshot edit.DraftSnapshot, targets []core.Target,
	selectedTargets []string, name string) {
	a.fileBusy = false
	if !snapshot.Matches(&a.session, a.generation) {
		a.message = "Settings changed while the file was opening. Open it again to replace the current draft."
	} else {
		a.session.Draft = targets
		a.session.SelectionDraft = selectedTargets
		a.selected = min(a.selected, len(a.session.Draft)-1)
		a.channel = core.ChannelLeft
		a.band = 0
		a.importDraft = nil
		a.rebuildEditors(cx)
		a.message = fmt.Sprintf("Opened %s. Apply saves effects; Change processing paths separately activates the path selection.", name)
	}
	a.notify(cx)
}

// openDraft asks for a settings file and puts it in the draft, asking first
// when that would replace unsaved edits.
func (a *App) openDraft(cx *gk.Cx) {
	if a.fileBusy || a.pending != nil || a.presets.writing() || a.link.Connecting() || a.transitionBusy() {
		return
	}
	snapshot := edit.CaptureDraft(&a.session, a.generation)
	a.fileBusy = true
	a.draggingBand = false
	type read struct {
		config core.Config
		err    error
	}
	finish := func(cx *gk.Cx, name string, config *core.Config, err error) {
		if err == nil && config != nil {
			var live *core.Config
			if saved := a.session.Saved; saved != nil && a.session.Connected && saved.Engine.Mode.HasProcessingControls() {
				live = &saved.Config
			}
			var selectedTargets []string
			if live != nil {
				selectedTargets, err = live.CanonicalSelection(config.SelectedTargets)
			} else {
				selectedTargets, err = config.CanonicalSelection(config.SelectedTargets)
			}
			var targets []core.Target
			if err == nil {
				targets, err = edit.ImportedTargets(*config, live)
			}
			if err == nil {
				if !snapshot.Matches(&a.session, a.generation) {
					a.fileBusy = false
					a.message = "The draft or connection changed. Open the file again for the current session."
				} else if a.session.AnyDirty() {
					a.confirmReplaceDraft(cx, snapshot, targets, selectedTargets, name)
				} else {
					a.installDraft(cx, snapshot, targets, selectedTargets, name)
				}
				a.notify(cx)
				return
			}
		}
		a.fileBusy = false
		if err != nil {
			a.message = "Could not open settings: " + err.Error()
		}
		a.notify(cx)
	}
	gk.PromptForPaths(cx, gk.PathPrompt{Files: true, Prompt: "Open RC2FX settings"}, func(cx *gk.Cx, d gk.PathsDone) {
		path, err := chosenPath(d)
		if err != nil || path == "" {
			finish(cx, "", nil, err)
			return
		}
		name := fileName(path)
		gk.Go(cx, func() read {
			text, err := edit.ReadPreset(path)
			if err != nil {
				return read{err: err}
			}
			config, err := core.ConfigFromJSON(text)
			return read{config: config, err: err}
		}, func(cx *gk.Cx, r read) {
			if r.err != nil {
				finish(cx, name, nil, r.err)
			} else {
				finish(cx, name, &r.config, nil)
			}
		})
	})
	a.notify(cx)
}

// confirmReplaceDraft asks before a settings file replaces unsaved edits.
func (a *App) confirmReplaceDraft(cx *gk.Cx, snapshot edit.DraftSnapshot, targets []core.Target,
	selectedTargets []string, name string) {
	detail := fmt.Sprintf("Open %s with %d chains. This doesn't change the mixer's saved settings.", name, len(targets))
	opened := false
	if !a.confirm(cx, confirmKindOther, Confirm{
		Title:  "Replace your unsaved draft?",
		Detail: detail,
		Cancel: "Keep editing",
		Action: "Discard and open",
	}, func(cx *gk.Cx) {
		opened = true
		a.installDraft(cx, snapshot, core.CloneTargets(targets), append([]string(nil), selectedTargets...), name)
	}, func(cx *gk.Cx) {
		if !opened {
			a.fileBusy = false
			a.notify(cx)
		}
	}) {
		// Another question is open: the file is not opened.
		a.fileBusy = false
		a.message = "Answer the open question first, then open the file again."
	}
}

// saveDraft writes the draft as a settings file the user names.
func (a *App) saveDraft(cx *gk.Cx) {
	if a.fileBusy {
		return
	}
	text, err := edit.SettingsFile(&a.session)
	if err != nil {
		a.message = err.Error()
		a.notify(cx)
		return
	}
	a.fileBusy = true
	a.draggingBand = false
	finish := func(cx *gk.Cx, path string, err error) {
		a.fileBusy = false
		switch {
		case err != nil:
			a.message = "Could not save settings: " + err.Error()
		case path != "":
			a.message = "Saved settings file to " + path
		}
		a.notify(cx)
	}
	gk.PromptForNewPath(cx, edit.HomeDirectory(), "rc2fx-settings.json", func(cx *gk.Cx, d gk.PathsDone) {
		path, err := chosenPath(d)
		if err != nil || path == "" {
			finish(cx, "", err)
			return
		}
		gk.Go(cx, func() error { return edit.WriteExport(path, text) }, func(cx *gk.Cx, err error) {
			if err != nil {
				finish(cx, "", err)
			} else {
				finish(cx, path, nil)
			}
		})
	})
	a.notify(cx)
}
