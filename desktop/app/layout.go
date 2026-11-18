package app

// This file does not import gk.

// The sizes pages share, so the app stays compact in one place. The default
// window has to fit a 1440×900 screen with its menu bar and dock. (ui.rs)
var (
	Window    = [2]float32{1280, 760}
	WindowMin = [2]float32{900, 580}
)

const (
	// RootText is one rem: what every `p_4`, `gap_3` and `text_sm` is measured in.
	RootText float32 = 15
	// DisplayText is the opening screens' headline, and DisplayTextSmall its
	// smaller form beside a card.
	DisplayText      float32 = 50
	DisplayTextSmall float32 = 44
	// TitleText is a page's or channel's name.
	TitleText float32 = 30
	// HeadingText is Home's heading, and a section's own title.
	HeadingText    float32 = 26
	SubheadingText float32 = 20
	// LeadText is the opening screens' sentence under the headline.
	LeadText float32 = 15
	// Around a page's content. PageBottom is the room below a page's last
	// section.
	PageX      float32 = 32
	PageTop    float32 = 20
	PageBottom float32 = 96
	// SectionGap is between a page's groups.
	SectionGap   float32 = 20
	SidebarWidth float32 = 224
	// RailWidth is the chain rail's, between the sidebar and a channel's
	// editor.
	RailWidth float32 = 212
	// EditorBottom is below a channel's editor, which fills the window.
	EditorBottom float32 = 20
	// NarrowEditor is the page width below which a channel's editor stacks
	// its columns (the EQ's table under its graph, a stage's readings under
	// its controls) instead of squeezing them side by side.
	NarrowEditor float32 = 760
	// EditorMinHeight is the least a channel's editor shrinks to before the
	// page scrolls.
	EditorMinHeight float32 = 420
	// MeterHeight is the overview's fader meters.
	MeterHeight float32 = 150
	// LargeButtonHeight is the opening screens' main button.
	LargeButtonHeight float32 = 46
	// InfoWidth is a long explanation's width in a tooltip, so it wraps inside
	// a tidy box.
	InfoWidth float32 = 260
)

// appID is the window's identity: the desktop entry's StartupWMClass and Icon
// (desktop/packaging/rc2fx.desktop). ui.rs window_options.
const appID = "rc2fx"
