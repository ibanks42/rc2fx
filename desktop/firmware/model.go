package firmware

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

// Model identifies a mixer hardware model. Board identity is distinct from the
// mixer executable (Pro II and Duo 1.7.6 share the same binary but run
// different board firmware). Zero value is ModelProIi to match Rust's Default.
type Model uint8

const (
	ModelProIi Model = iota
	ModelDuo
)

// ModelAll lists all models in the same order as the Rust crate.
var ModelAll = [2]Model{ModelProIi, ModelDuo}

// VendorID is RØDE's USB vendor ID.
const VendorID = model.VendorID

// Hardware converts m to the flashing library's model type.
func (m Model) Hardware() model.Model {
	if m == ModelDuo {
		return model.Duo
	}
	return model.ProIi
}

// FromHardware converts a flashing-library model back to a firmware.Model.
func FromHardware(h model.Model) Model {
	if h == model.Duo {
		return ModelDuo
	}
	return ModelProIi
}

// ParseModel parses a model ID string.
func ParseModel(id string) (Model, error) {
	h, err := model.Parse(id)
	if err != nil {
		return 0, err
	}
	return FromHardware(h), nil
}

// ID returns the model's catalog and JSON identifier.
func (m Model) ID() string { return m.Hardware().ID() }

func (m Model) String() string { return m.ID() }

func (m Model) Hostname() string { return m.Hardware().Hostname() }

func (m Model) BoardDTB() string { return m.Hardware().BoardDTB() }

func (m Model) FirmwareDirectory() string { return m.Hardware().FirmwareDirectory() }

// RuntimeEnabled reports whether the model is supported without an explicit
// experimental opt-in.
func (m Model) RuntimeEnabled() bool { return m == ModelProIi }

// RequireRuntimeWith rejects models that aren't runtime-enabled unless
// experimentalDuo is set for ModelDuo.
func (m Model) RequireRuntimeWith(experimentalDuo bool) error {
	if m.RuntimeEnabled() || (m == ModelDuo && experimentalDuo) {
		return nil
	}
	return errors.New("RØDECaster Duo support is experimental and untested on hardware. Enable experimental Duo flashing at your own risk.")
}

func (m Model) Label() string { return m.Hardware().Label() }

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

func (m Model) NormalPIDs() [5]uint16 { return m.Hardware().NormalPIDs() }

func (m Model) UpdatePID() uint16 { return m.Hardware().UpdatePID() }

// ModelFromUSB identifies a model by its USB IDs, in normal or update mode.
func ModelFromUSB(vendor, product uint16) (Model, bool) {
	h, ok := model.FromUSB(vendor, product)
	if !ok {
		return 0, false
	}
	return FromHardware(h), true
}

// ModelFromBoard identifies the mixer from its rootfs hostname and extlinux
// FDT entry, requiring both to agree with no overrides.
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

func (m Model) MarshalJSON() ([]byte, error) {
	return json.Marshal(m.ID())
}

func (m *Model) UnmarshalJSON(data []byte) error {
	var id string
	if err := core.DecodeStrict(data, &id); err != nil {
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

// rustLines matches Rust's str::lines: splits on \n, strips a preceding \r,
// and omits a trailing empty line.
func rustLines(text string) []string {
	if text == "" {
		return nil
	}
	ended := strings.HasSuffix(text, "\n")
	lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")
	for i, line := range lines {
		if i < len(lines)-1 || ended {
			lines[i] = strings.TrimSuffix(line, "\r")
		}
	}
	return lines
}

// isRustWhitespace matches Rust's char::is_whitespace (Unicode White_Space).
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
