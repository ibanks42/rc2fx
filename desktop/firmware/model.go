package firmware

import (
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
)

// Model is a mixer model. Board identity is independent of the mixer
// executable: Pro II and Duo 1.7.6 ship the same executable but different
// board/controller firmware. The zero value is ModelProIi, as the Rust
// enum's default.
type Model uint8

const (
	ModelProIi Model = iota
	ModelDuo
)

// ModelAll is every model, in the Rust order.
var ModelAll = [2]Model{ModelProIi, ModelDuo}

// VendorID is RØDE's USB vendor ID.
const VendorID uint16 = 0x19f7

// ParseModel finds the model with the given ID.
func ParseModel(id string) (Model, error) {
	for _, m := range ModelAll {
		if m.ID() == id {
			return m, nil
		}
	}
	return 0, fmt.Errorf("unknown mixer model %s", id)
}

// ID is the model's catalog and serialized name.
func (m Model) ID() string {
	if m == ModelDuo {
		return "rodecaster-duo"
	}
	return "rodecaster-pro-ii"
}

func (m Model) String() string { return m.ID() }

func (m Model) Hostname() string {
	if m == ModelDuo {
		return "rodecaster-pro-ii-mini"
	}
	return "rodecaster-pro-ii"
}

func (m Model) BoardDTB() string {
	if m == ModelDuo {
		return "/boot/meson-axg-rodecaster-pro-ii-mini.dtb"
	}
	return "/boot/meson-axg-rodecaster-pro-ii.dtb"
}

func (m Model) FirmwareDirectory() string {
	if m == ModelDuo {
		return "rcp2-duo"
	}
	return "rcp2"
}

// RuntimeEnabled reports whether the model is admitted without an explicit
// experimental opt-in.
func (m Model) RuntimeEnabled() bool { return m == ModelProIi }

func (m Model) RequireRuntime() error { return m.RequireRuntimeWith(false) }

func (m Model) RequireRuntimeWith(experimentalDuo bool) error {
	if m.RuntimeEnabled() || (m == ModelDuo && experimentalDuo) {
		return nil
	}
	return errors.New("RØDECaster Duo support is experimental and untested on hardware. Enable experimental Duo flashing at your own risk.")
}

func (m Model) Label() string {
	if m == ModelDuo {
		return "RØDECaster Duo"
	}
	return "RØDECaster Pro II"
}

func (m Model) RouteLayout() string {
	if m == ModelDuo {
		return "duo-23-v1"
	}
	return "pro-ii-25-v1"
}

func (m Model) SourceEnum() string {
	if m == ModelDuo {
		return "duo-sources-v1"
	}
	return "pro-ii-sources-v1"
}

func (m Model) NormalPIDs() [5]uint16 {
	if m == ModelDuo {
		return [5]uint16{0x0050, 0x0079, 0x008e, 0x0093, 0x0095}
	}
	return [5]uint16{0x0037, 0x0078, 0x008d, 0x0092, 0x0094}
}

func (m Model) UpdatePID() uint16 {
	if m == ModelDuo {
		return 0x004f
	}
	return 0x0030
}

// ModelFromUSB identifies a model by its USB IDs, in normal or update mode.
func ModelFromUSB(vendor, product uint16) (Model, bool) {
	if vendor != VendorID {
		return 0, false
	}
	for _, m := range ModelAll {
		pids := m.NormalPIDs()
		if slices.Contains(pids[:], product) || m.UpdatePID() == product {
			return m, true
		}
	}
	return 0, false
}

// ModelFromBoard requires consistent rootfs and boot-board evidence. No
// environment override or executable hash may impersonate a different
// device model.
func ModelFromBoard(hostname, extlinux string) (Model, error) {
	var boards [][]string
	for _, line := range rustLines(extlinux) {
		words := strings.FieldsFunc(line, isRustWhitespace)
		if len(words) > 0 && strings.EqualFold(words[0], "FDT") && isASCII(words[0]) {
			boards = append(boards, words[1:])
		}
	}
	if len(boards) == 0 {
		return 0, errors.New("missing boot board identity")
	}
	if len(boards) > 1 || len(boards[0]) != 1 {
		return 0, errors.New("ambiguous boot board identity")
	}
	host := strings.TrimFunc(hostname, isRustWhitespace)
	for _, m := range ModelAll {
		if m.Hostname() == host && m.BoardDTB() == boards[0][0] {
			return m, nil
		}
	}
	return 0, errors.New("unknown or conflicting hostname and boot board identity")
}

// MarshalJSON writes the model's ID, as serde does.
func (m Model) MarshalJSON() ([]byte, error) {
	return json.Marshal(m.ID())
}

// UnmarshalJSON reads a model ID; anything else is refused.
func (m *Model) UnmarshalJSON(data []byte) error {
	id, err := stringValue(data)
	if err != nil {
		return err
	}
	for _, model := range ModelAll {
		if model.ID() == id {
			*m = model
			return nil
		}
	}
	return fmt.Errorf("unknown variant `%s`, expected `rodecaster-pro-ii` or `rodecaster-duo`", id)
}

// rustLines splits as Rust's str::lines: on \n, dropping one \r before it,
// with no final empty line.
func rustLines(text string) []string {
	if text == "" {
		return nil
	}
	ended := strings.HasSuffix(text, "\n")
	lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")
	for i, line := range lines {
		// Only a \r that ends a line with its \n is part of the line ending.
		if i < len(lines)-1 || ended {
			lines[i] = strings.TrimSuffix(line, "\r")
		}
	}
	return lines
}

// isRustWhitespace is Rust's char::is_whitespace: Unicode White_Space.
func isRustWhitespace(r rune) bool {
	switch r {
	case '\t', '\n', '\v', '\f', '\r', ' ', 0x85, 0xa0, 0x1680, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000:
		return true
	}
	return r >= 0x2000 && r <= 0x200a
}

func isASCII(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] >= 0x80 {
			return false
		}
	}
	return true
}
