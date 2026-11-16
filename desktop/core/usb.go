package core

// Observed USB link state, independent of saved effects and DSP progress.

// USBLinkState is serde `rename_all = "camelCase"`.
type USBLinkState string

const (
	USBLinkStateUnavailable     USBLinkState = "unavailable"
	USBLinkStateNotAttached     USBLinkState = "notAttached"
	USBLinkStateAttached        USBLinkState = "attached"
	USBLinkStatePowered         USBLinkState = "powered"
	USBLinkStateReconnecting    USBLinkState = "reconnecting"
	USBLinkStateUnauthenticated USBLinkState = "unauthenticated"
	USBLinkStateDefault         USBLinkState = "default"
	USBLinkStateAddressed       USBLinkState = "addressed"
	USBLinkStateConfigured      USBLinkState = "configured"
	USBLinkStateSuspended       USBLinkState = "suspended"
)

var usbLinkStates = []USBLinkState{
	USBLinkStateUnavailable, USBLinkStateNotAttached, USBLinkStateAttached, USBLinkStatePowered,
	USBLinkStateReconnecting, USBLinkStateUnauthenticated, USBLinkStateDefault,
	USBLinkStateAddressed, USBLinkStateConfigured, USBLinkStateSuspended,
}

func (s *USBLinkState) UnmarshalText(text []byte) error { return enumUnmarshal(s, text, usbLinkStates) }

type USBStatus struct {
	// Pro II USB 1 only. This does not describe USB 2, network connectivity,
	// host playback activity, or whether a configured link carries audio.
	USB1 USBLinkState `json:"usb1"`
}
