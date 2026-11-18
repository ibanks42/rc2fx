package licenses

import "testing"

// The license dialog's rendering checks run offscreen (desktop/app/uitest);
// these check the Markdown they render.

func TestNoticesEscapeMarkupAndKeepLeadingSpace(t *testing.T) {
	for in, want := range map[string]string{
		"# License <AUTHORS>":    `\# License \<AUTHORS\>`,
		"    * not emphasis":     "&#32;&#32;&#32;&#32;\\* not emphasis",
		"\tIndented notice":      "&#9;Indented notice",
		"a\n\nb":                 "a\\\n&#32;\\\nb",
		"©":                      "©",
		"line\r\nnext\n":         "line\\\nnext",
		"![image](x.png)":        `\!\[image\]\(x\.png\)`,
		"`code` | ~~strike~~ \\": "\\`code\\` \\| \\~\\~strike\\~\\~ \\\\",
	} {
		if got := NoticeMarkdown(in); got != want {
			t.Fatalf("%q: %q", in, got)
		}
	}
}

func TestLongNoticesBreakIntoSmallBlocks(t *testing.T) {
	text := ""
	for i := 0; i < 65; i++ {
		text += "x\n"
	}
	got := NoticeMarkdown(text)
	blocks := 0
	for i := 0; i+1 < len(got); i++ {
		if got[i] == '\n' && got[i+1] == '\n' {
			blocks++
		}
	}
	if blocks != 2 {
		t.Fatalf("%d paragraph breaks", blocks)
	}
}
