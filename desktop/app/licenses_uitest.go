package app

// Headless UI tests for the licenses dialog, driven by desktop/app/uitest.

import (
	"fmt"
	"os"
	"strings"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/licenses"
)

// UITestModes maps CLI mode flags to test names for RunUITest.
var UITestModes = map[string]string{
	"licenses-markup":  "license_words_and_markup_characters_survive_rich_text_rendering",
	"licenses-long":    "long_notices_keep_a_bounded_viewport_and_copy_the_entire_text",
	"licenses-lazy":    "sections_parse_lazily_and_keep_selection_when_reopened",
	"licenses-measure": "measure_license_redraws (ignored in the Rust: a measurement)",
}

type uitest struct {
	name   string
	failed bool
	frame  int
}

func (t *uitest) fail(format string, args ...any) {
	if !t.failed {
		fmt.Printf("FAIL %s: %s\n", t.name, fmt.Sprintf(format, args...))
	}
	t.failed = true
}

func normalize(text string) string {
	lines := strings.Split(strings.ReplaceAll(text, "\r\n", "\n"), "\n")
	if len(lines) > 0 && lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	for i, line := range lines {
		lines[i] = strings.TrimRight(line, " \t\r\n")
	}
	return strings.Join(lines, "\n")
}

func (t *uitest) sameText(what, actual, expected string) {
	actual = strings.TrimRight(actual, " \t\r\n")
	expected = strings.TrimRight(expected, " \t\r\n")
	a, b := strings.Split(actual, "\n"), strings.Split(expected, "\n")
	for i := 0; i < len(a) && i < len(b); i++ {
		if a[i] != b[i] {
			t.fail("%s line %d: %q, want %q", what, i, a[i], b[i])
			return
		}
	}
	if len(actual) != len(expected) {
		t.fail("%s: %d bytes, want %d", what, len(actual), len(expected))
	}
}

// RunUITest executes a headless UI test mode. Must be called from main with the diag library.
func RunUITest(mode string) bool {
	t := &uitest{name: UITestModes[mode]}
	if t.name == "" {
		fmt.Printf("FAIL %s: unknown mode\n", mode)
		return false
	}
	var script string
	var start func(cx *gk.Cx)
	switch mode {
	case "licenses-markup":
		script, start = t.markup()
	case "licenses-long":
		script, start = t.long()
	case "licenses-lazy":
		script, start = t.lazy()
	case "licenses-measure":
		script, start = t.measure()
	}
	// Theme setup requires an active window, so each mode applies it in its root view init.
	gk.RunHeadless(script, start)
	if !t.failed {
		fmt.Printf("PASS %s\n", t.name)
	}
	return !t.failed
}

func noticeRoot(cx *gk.Cx, text string, frame func(cx *gk.Cx, state gk.TextViewState)) {
	var state gk.TextViewState
	var theme gk.BaseTheme
	root := gk.NewView(func(cx *gk.Cx) {
		gk.ThemeChange(cx, gk.ThemeModeDark)
		state = gk.NewTextViewStateMarkdown(cx, licenses.NoticeMarkdown(text)).Build()
		theme = gk.NewBaseThemeGlobal(cx)
		gk.OnFrame(cx, func(cx *gk.Cx) { frame(cx, state) })
	}, func(cx *gk.Cx) gk.Element {
		return gk.Div().W(px(640)).Child(noticeBox(state, theme))
	})
	gk.OpenWindowWith(cx, gk.WindowConfig{Width: 700, Height: 420, Title: "licenses"}, root)
}

func (t *uitest) markup() (string, func(cx *gk.Cx)) {
	text := "# License <AUTHORS> & copyright ©\n\n    * This is not emphasis or code.\n1. [text](https://example.com)\n![image](https://example.com/image.png)\n<script>hello</script>\n`code` | ~~strike~~ \\ slash\n\tIndented notice"
	return "size 700 420\nframe\nframe\nframe\n", func(cx *gk.Cx) {
		noticeRoot(cx, text, func(cx *gk.Cx, state gk.TextViewState) {
			t.frame++
			switch t.frame {
			case 1:
				state.SelectAll(cx)
			case 2:
				if got := normalize(state.RenderedText(cx).AsStr(cx)); got != text {
					t.fail("rendered %q", got)
				}
				if got := normalize(state.SelectedText(cx)); got != text {
					t.fail("selected %q", got)
				}
			}
		})
	}
}

func (t *uitest) long() (string, func(cx *gk.Cx)) {
	var b strings.Builder
	for i := range 7000 {
		fmt.Fprintf(&b, "Copyright holder %d: All rights reserved.\n", i)
	}
	text := b.String()
	shots := os.Getenv("RC2FX_UITEST_SHOTS")
	if shots == "" {
		shots = os.TempDir()
	}
	// gk doesn't expose ListState scroll metrics, so capture before/after screenshots for visual inspection.
	script := "size 700 420\nframe\nframe\n" +
		"shot " + shots + "/licenses-long-top.png\n" +
		"scroll 320 200 -10000000\nframe\nframe\n" +
		"shot " + shots + "/licenses-long-end.png\nframe\nframe\n"
	return script, func(cx *gk.Cx) {
		noticeRoot(cx, text, func(cx *gk.Cx, state gk.TextViewState) {
			t.frame++
			switch t.frame {
			case 2:
				t.sameText("rendered", state.RenderedText(cx).AsStr(cx), strings.TrimRight(text, "\n"))
			case 4:
				state.SelectAll(cx)
			case 5:
				t.sameText("selected", state.SelectedText(cx), strings.TrimRight(text, "\n"))
				fmt.Printf("SHOTS %s/licenses-long-top.png %s/licenses-long-end.png\n", shots, shots)
			}
		})
	}
}

func (t *uitest) lazy() (string, func(cx *gk.Cx)) {
	return "size 800 600\n" + strings.Repeat("frame\n", 7), func(cx *gk.Cx) {
		var l *licensesView
		var original gk.TextViewState
		root := gk.NewView(func(cx *gk.Cx) {
			gk.ThemeChange(cx, gk.ThemeModeDark)
			l = newLicensesView(cx)
			gk.OnFrame(cx, func(cx *gk.Cx) {
				t.frame++
				switch t.frame {
				case 1:
					if len(l.textViews) != 0 {
						t.fail("%d sections parsed before any was opened", len(l.textViews))
					}
					l.open(cx, 0)
					gk.Notify(cx, l.view)
				case 2:
					original = l.textViews[0]
					original.SelectAll(cx)
					clear(l.expanded)
					gk.Notify(cx, l.view)
				case 3:
					l.open(cx, 0)
					gk.Notify(cx, l.view)
				case 4:
					if len(l.textViews) != 1 {
						t.fail("%d text views, want 1", len(l.textViews))
					}
					if l.textViews[0] != original {
						t.fail("section 0 was parsed again")
					}
					t.sameText("selected", normalize(original.SelectedText(cx)), normalize(licenses.Sections()[0].Text))
				}
			})
		}, func(cx *gk.Cx) gk.Element {
			return gk.Div(s.SizeFull).ChildView(l.view)
		})
		gk.OpenWindowWith(cx, gk.WindowConfig{Width: 800, Height: 600, Title: "licenses"}, root)
	}
}

func (t *uitest) measure() (string, func(cx *gk.Cx)) {
	return "size 800 600\nframe\nstat\n" + strings.Repeat("frame\n", 12) + "stat\n", func(cx *gk.Cx) {
		var l *licensesView
		var started time.Time
		root := gk.NewView(func(cx *gk.Cx) {
			gk.ThemeChange(cx, gk.ThemeModeDark)
			l = newLicensesView(cx)
			for index := range licenses.Sections() {
				l.open(cx, index)
			}
			started = time.Now()
			gk.OnFrame(cx, func(cx *gk.Cx) {
				t.frame++
				switch {
				case t.frame == 1:
					fmt.Printf("first draw: %s\n", time.Since(started))
					for index, state := range l.textViews {
						if normalize(state.RenderedText(cx).AsStr(cx)) != normalize(licenses.Sections()[index].Text) {
							t.fail("section %d rendered differently", index)
						}
					}
					started = time.Now()
				case t.frame <= 11:
					gk.Notify(cx, l.view)
				case t.frame == 12:
					fmt.Printf("ten redraws: %s\n", time.Since(started))
				}
			})
		}, func(cx *gk.Cx) gk.Element {
			return gk.Div(s.SizeFull).ChildView(l.view)
		})
		gk.OpenWindowWith(cx, gk.WindowConfig{Width: 800, Height: 600, Title: "licenses"}, root)
	}
}
