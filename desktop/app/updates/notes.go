package updates

// The Versions section's changelog: notes come from the network, so they are
// treated as untrusted: raw HTML and image syntax are escaped, images never
// load, and links open only when SafeURL allows them.

import (
	"fmt"
	"strings"
	"unicode"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// EntryHeading is one version's heading, marking notes this app cannot
// install: entries newer than available (nil for none) need an app update
// first.
func EntryHeading(prefix string, entry installer.ReleaseNotes, available *string) string {
	if available != nil {
		if order, ok := model.Compare(entry.Version, *available); ok && order > 0 {
			return fmt.Sprintf("%s %s · needs an app update", prefix, entry.Version)
		}
	}
	return fmt.Sprintf("%s %s", prefix, entry.Version)
}

// SafeMarkdown escapes what could reach the renderer as raw HTML or as an
// image. Text in fenced blocks and code spans is literal already, so it
// stays as written. A misjudged fence only leaves a tag to the renderer,
// which still loads no images and opens no unapproved links.
func SafeMarkdown(notes string) string {
	var out strings.Builder
	out.Grow(len(notes))
	fenced := false
	var open rune
	var length int
	for _, line := range splitInclusive(notes) {
		text := strings.TrimLeftFunc(line, unicode.IsSpace)
		var marker rune
		run := 0
		if text != "" && (text[0] == '`' || text[0] == '~') {
			marker = rune(text[0])
			for _, c := range text {
				if c != marker {
					break
				}
				run++
			}
		}
		switch {
		case fenced:
			out.WriteString(line)
			if marker != 0 && marker == open && run >= length {
				fenced = false
			}
		case run >= 3:
			out.WriteString(line)
			fenced, open, length = true, marker, run
		default:
			escapeLine(line, &out)
		}
	}
	return out.String()
}

// splitInclusive splits s after each '\n', keeping it; no empty part at
// the end.
func splitInclusive(s string) []string {
	var lines []string
	for s != "" {
		i := strings.IndexByte(s, '\n')
		if i < 0 {
			lines = append(lines, s)
			break
		}
		lines = append(lines, s[:i+1])
		s = s[i+1:]
	}
	return lines
}

func escapeLine(line string, out *strings.Builder) {
	chars := []rune(line)
	backticks := func(from int) int {
		n := 0
		for from+n < len(chars) && chars[from+n] == '`' {
			n++
		}
		return n
	}
	for i := 0; i < len(chars); {
		switch c := chars[i]; {
		case c == '\\':
			out.WriteString(string(chars[i:min(i+2, len(chars))]))
			i += 2
		case c == '`':
			run := backticks(i)
			end := i + run
			for j := i + run; j < len(chars); j++ {
				if backticks(j) == run && chars[j-1] != '`' {
					end = j + run
					break
				}
			}
			out.WriteString(string(chars[i:end]))
			i = end
		case c == '<':
			out.WriteString("\\<")
			i++
		case c == '!' && i+1 < len(chars) && chars[i+1] == '[':
			out.WriteString("\\!")
			i++
		default:
			out.WriteRune(c)
			i++
		}
	}
}
