package updates

import (
	"fmt"
	"strings"
	"unicode"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/appupdate"
)

// EntryHeading formats a changelog entry title, flagging versions newer than
// available as requiring an app update first.
func EntryHeading(prefix string, entry appupdate.ReleaseNotes, available *string) string {
	if available != nil {
		if order, ok := model.Compare(entry.Version, *available); ok && order > 0 {
			return fmt.Sprintf("%s %s · needs an app update", prefix, entry.Version)
		}
	}
	return fmt.Sprintf("%s %s", prefix, entry.Version)
}

// SafeMarkdown escapes raw HTML tags and markdown image syntax outside code
// spans and fenced code blocks so untrusted release notes cannot inject HTML
// or trigger remote image loads.
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
