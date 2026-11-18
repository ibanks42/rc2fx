package app

// interchange.rs: importing an EQ from APO text or EasyEffects JSON (a file
// or the clipboard), reviewing it, and exporting the channel's EQ. The file
// logic is in edit/files.go.

import (
	"errors"
	"fmt"
	"path/filepath"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// ImportDraft is an EQ import under review: the channel it was started
// for, the file's text and name, the choices made so far and where they
// leave it.
type ImportDraft struct {
	target  edit.ImportTarget
	text    string
	name    string
	options core.ImportOptions
	stage   core.ImportResult
}

// writeExport is interchange.rs write_export (edit.WriteExport); the view
// preferences (prefs.go) use it too.
func writeExport(path, text string) error { return edit.WriteExport(path, text) }

func (a *App) stageImport(target edit.ImportTarget, name, text string) {
	options := core.ImportOptions{Stereo: target.Channels == 2}
	stage, err := edit.ParseImport(text, options)
	if err != nil {
		a.importDraft = nil
		a.message = "Could not import EQ: " + err.Error()
		message := a.message
		a.importError = &message
		return
	}
	a.importError = nil
	a.message = fmt.Sprintf("Review %s below Import EQ, then replace this target’s EQ.", name)
	a.importDraft = &ImportDraft{target: target, text: text, name: name, options: options, stage: stage}
}

// pasteEQ stages the clipboard's text as an EQ import.
func (a *App) pasteEQ(cx *gk.Cx) {
	if a.fileBusy {
		return
	}
	a.importDraft = nil
	a.importError = nil
	a.draggingBand = false
	if text, ok := gk.ClipboardRead(cx); ok {
		a.stageImport(edit.CaptureImportTarget(a.target(), a.generation), "clipboard", text)
	} else {
		message := "Copy APO text or EasyEffects JSON, then click Paste."
		a.importError = &message
	}
	a.notify(cx)
}

// importEQ asks for an EQ file and stages it, unless the channel or its EQ
// changed while the dialog was open.
func (a *App) importEQ(cx *gk.Cx) {
	if a.fileBusy {
		return
	}
	a.importDraft = nil
	a.importError = nil
	a.draggingBand = false
	a.fileBusy = true
	target := edit.CaptureImportTarget(a.target(), a.generation)
	type opened struct {
		name, text string
		err        error
	}
	finish := func(cx *gk.Cx, result *opened, err error) {
		a.fileBusy = false
		switch {
		case err != nil:
			message := "Could not import EQ: " + err.Error()
			a.importError = &message
		case result == nil:
		case !target.Matches(a.target(), a.generation):
			message := "The target or EQ changed while the file was opening. Import it again for the current EQ."
			a.importError = &message
		default:
			a.stageImport(target, result.name, result.text)
		}
		a.notify(cx)
	}
	gk.PromptForPaths(cx, gk.PathPrompt{Files: true, Prompt: "Import EQ: APO text or EasyEffects JSON"},
		func(cx *gk.Cx, d gk.PathsDone) {
			path, err := chosenPath(d)
			if err != nil || path == "" {
				finish(cx, nil, err)
				return
			}
			gk.Go(cx, func() opened {
				text, err := edit.ReadPreset(path)
				return opened{name: fileName(path), text: text, err: err}
			}, func(cx *gk.Cx, o opened) {
				if o.err != nil {
					finish(cx, nil, o.err)
				} else {
					finish(cx, &o, nil)
				}
			})
		})
	a.notify(cx)
}

// chosenPath is a file dialog's answer: the last path chosen, "" when it
// was cancelled, or why it could not be shown.
func chosenPath(d gk.PathsDone) (string, error) {
	switch d.Status {
	case gk.DoneFailed:
		return "", errors.New(d.Error)
	case gk.DoneAnswered:
		if len(d.Paths) > 0 {
			return d.Paths[len(d.Paths)-1], nil
		}
	}
	return "", nil
}

// fileName is Path::file_name, lossy: the last part of path, "" when it
// has none.
func fileName(path string) string {
	base := filepath.Base(path)
	if base == "." || base == ".." || base == string(filepath.Separator) {
		return ""
	}
	return base
}

// chooseImport answers the import's question: which equalizer (selected,
// "" to keep the current one) and which side for a mono input (nil for
// none).
func (a *App) chooseImport(cx *gk.Cx, selected string, channel *core.Channel) {
	if imp := a.importDraft; imp != nil {
		if selected != "" {
			imp.options.Selected = &selected
		}
		imp.options.Channel = channel
		if stage, err := edit.ParseImport(imp.text, imp.options); err != nil {
			message := err.Error()
			a.importError = &message
		} else {
			imp.stage = stage
		}
	}
	a.notify(cx)
}

// finishImport replaces the channel's EQ with the reviewed import.
func (a *App) finishImport(cx *gk.Cx) {
	imp := a.importDraft
	if imp == nil {
		return
	}
	a.importDraft = nil
	if !imp.target.Matches(a.target(), a.generation) {
		message := "This EQ changed after the import started. Import again to replace its current settings."
		a.importError = &message
	} else if imp.stage.Kind == core.ImportResultReady {
		a.targetMut().EQ = imp.stage.EQ
		a.channel = core.ChannelLeft
		a.band = 0
		a.message = fmt.Sprintf("Imported %s into %s. It is saved to the mixer once edits stop.",
			imp.name, a.targetName(a.target()))
		a.rebuildEditors(cx)
	}
	a.notify(cx)
}

// importPanel is the import under review: the question it asks, or what it
// will bring and the button that replaces the EQ.
func (a *App) importPanel(imp *ImportDraft, cx *gk.Cx) gk.Element {
	panel := gk.VFlex(s.Gap3, s.P3, s.RoundedMd).Bg(rgb(surface().Raised)).
		Child(gk.Div(s.FontSemibold).Child(txt("Import " + imp.name)))
	switch imp.stage.Kind {
	case core.ImportResultChooseEqualizer:
		panel = panel.Child(txt("Choose the equalizer to import:"))
		for index, candidate := range imp.stage.Candidates {
			key := candidate.Key
			panel = panel.Child(gk.Button(gk.Key("import-candidate", index)).
				Label(fmt.Sprintf("%s · %d bands", candidate.Key, candidate.BandCount)).
				Disabled(candidate.Error != nil).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.chooseImport(cx, key, nil) }))
			if candidate.Error != nil {
				panel = panel.Child(gk.Div(s.TextSm).Child(txt(*candidate.Error)))
			}
		}
	case core.ImportResultChooseChannel:
		panel = panel.Child(txt("This preset has different left and right settings. Choose one for this mono input:"))
		for index, side := range []struct {
			channel core.Channel
			label   string
		}{{core.ChannelLeft, "Left"}, {core.ChannelRight, "Right"}} {
			key, channel := imp.stage.Source.Key, side.channel
			panel = panel.Child(gk.Button(gk.Key("import-channel", index)).Label(side.label).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.chooseImport(cx, key, &channel) }))
		}
	case core.ImportResultReady:
		eq := imp.stage.EQ
		panel = panel.Child(txt(fmt.Sprintf("%s · %d left / %d right bands · input %+.2f dB · output %+.2f dB",
			imp.stage.Source.Key, len(eq.Bands), len(eq.Channel(core.ChannelRight)), eq.InputGainDB, eq.OutputGainDB)))
		if imp.stage.OtherEffects > 0 {
			panel = panel.Child(txt("Only the equalizer is imported; the preset’s other effects are excluded."))
		}
		panel = panel.Child(gk.Button("import-confirm").Primary().
			Label(fmt.Sprintf("Replace %s EQ", a.targetName(a.target()))).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.finishImport(cx) }))
	}
	return panel.Child(gk.Button("import-cancel").Ghost().Label("Cancel import").
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.importDraft = nil
			a.importError = nil
			a.notify(cx)
		}))
}

// exportEQ writes the channel's EQ as EasyEffects JSON to a file the user
// names.
func (a *App) exportEQ(cx *gk.Cx) {
	if a.fileBusy {
		return
	}
	target := a.target()
	pipeline := core.PipelineOutput
	if target.Kind == core.TargetKindMic {
		pipeline = core.PipelineInput
	}
	text, err := core.ExportEasyEffects(target.EQ, pipeline)
	if err != nil {
		a.message = err.Error()
		a.notify(cx)
		return
	}
	name := target.ID + "-eq.json"
	a.fileBusy = true
	a.draggingBand = false
	finish := func(cx *gk.Cx, path string, err error) {
		a.fileBusy = false
		switch {
		case err != nil:
			a.message = "Could not export EQ: " + err.Error()
		case path != "":
			a.message = "Exported EQ to " + path
		}
		a.notify(cx)
	}
	gk.PromptForNewPath(cx, edit.HomeDirectory(), name, func(cx *gk.Cx, d gk.PathsDone) {
		path, err := chosenPath(d)
		if err != nil || path == "" {
			finish(cx, "", err)
			return
		}
		gk.Go(cx, func() error { return writeExport(path, text) }, func(cx *gk.Cx, err error) {
			if err != nil {
				finish(cx, "", err)
			} else {
				finish(cx, path, nil)
			}
		})
	})
	a.notify(cx)
}
