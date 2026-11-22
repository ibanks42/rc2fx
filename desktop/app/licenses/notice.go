package licenses

import (
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/common"
)

// NoticeMarkdown converts a plain-text license notice into safe Markdown for
// the rich-text dialog. ASCII punctuation and leading whitespace are escaped so
// notices can't accidentally render as headings, links, or HTML, and lines are
// chunked into 32-line paragraphs so the text view can virtualize long notices.
func NoticeMarkdown(text string) string {
	var result strings.Builder
	result.Grow(len(text) * 2)
	for index, line := range common.Lines(text) {
		if index > 0 {
			if index%32 == 0 {
				result.WriteString("\n\n")
			} else {
				result.WriteString("\\\n")
			}
		}
		if line == "" {
			result.WriteString("&#32;")
		}
		leading := true
		for _, ch := range line {
			switch {
			case ch == ' ' && leading:
				result.WriteString("&#32;")
			case ch == '\t' && leading:
				result.WriteString("&#9;")
			default:
				leading = false
				if ch < 0x80 && strings.ContainsRune("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~", ch) {
					result.WriteByte('\\')
				}
				result.WriteRune(ch)
			}
		}
	}
	return result.String()
}
