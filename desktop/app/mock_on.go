//go:build mockmixer

package app

// mockMixer is cfg!(feature = "mock-mixer"): the transport talks to the mock
// mixer, and the window says so.
const mockMixer = true
