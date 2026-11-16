//go:build embedded

package installer

const payloadModeText = "RC2FX-PAYLOAD-MODE:embedded:1"

// Embedded is an engine that installs from the payload embedded in this
// executable, caching in work. This is what the desktop app uses: nothing
// has to sit beside it.
func Embedded(work string) *Engine {
	return withSource(Source{Kind: SourceEmbedded}, work)
}
