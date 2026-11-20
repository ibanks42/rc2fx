//go:build embedded

package installer

const payloadModeText = "RC2FX-PAYLOAD-MODE:embedded:1"

// Embedded returns an Engine backed by the payload compiled into this binary.
func Embedded(work string) *Engine {
	return withSource(Source{Kind: SourceEmbedded}, work)
}
