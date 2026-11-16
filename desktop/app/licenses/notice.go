package licenses

import "strings"

// NoticeMarkdown is a notice as Markdown for the dialog's rich text
// (licenses/mod.rs notice_markdown). License sources are plain text, not
// Markdown: punctuation and leading whitespace are escaped so legal notices
// cannot turn into headings, links, HTML or images. Small blocks let the
// text view virtualize even notices with no blank paragraphs.
func NoticeMarkdown(text string) string {
	var result strings.Builder
	result.Grow(len(text) * 2)
	for index, line := range lines(text) {
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

// lines is str::lines: split at '\n', drop a '\r' before it, and no empty
// line after a final '\n'.
func lines(text string) []string {
	var out []string
	for text != "" {
		line, rest, found := strings.Cut(text, "\n")
		if found {
			line = strings.TrimSuffix(line, "\r")
		}
		out = append(out, line)
		text = rest
		if !found {
			break
		}
	}
	return out
}
