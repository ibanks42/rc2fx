package theme

// Default and minimum window dimensions (sized to fit a 1440x900 screen with
// menu bar and dock).
var (
	Window    = [2]float32{1280, 760}
	WindowMin = [2]float32{900, 580}
)

const (
	// Base rem size for spacing and typography utilities.
	RootText         float32 = 15
	DisplayText      float32 = 50
	DisplayTextSmall float32 = 44
	TitleText        float32 = 30
	HeadingText      float32 = 26
	SubheadingText   float32 = 20
	LeadText         float32 = 15

	PageX        float32 = 32
	PageTop      float32 = 20
	PageBottom   float32 = 96
	SectionGap   float32 = 20
	SidebarWidth float32 = 224
	RailWidth    float32 = 212
	EditorBottom float32 = 20

	// Width threshold below which the channel editor stacks its columns vertically.
	NarrowEditor      float32 = 760
	EditorMinHeight   float32 = 420
	MeterHeight       float32 = 150
	LargeButtonHeight float32 = 46
	InfoWidth         float32 = 260
)

// AppID matches StartupWMClass and Icon in desktop/packaging/rc2fx.desktop.
const AppID = "rc2fx"
