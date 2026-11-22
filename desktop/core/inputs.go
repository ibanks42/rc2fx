package core

import (
	"strconv"
	"strings"
	"unicode"
)

// Stock RØDE show inventory (wire types generated from schema/core.go).

func (s StockInventory) Validate() error {
	text := func(value string, maximum int) bool {
		if len(value) > maximum {
			return false
		}
		return strings.IndexFunc(value, unicode.IsControl) < 0
	}
	hex := func(value string, length int) bool {
		if len(value) != length {
			return false
		}
		for i := 0; i < len(value); i++ {
			c := value[i]
			if !(c >= '0' && c <= '9') && !(c >= 'a' && c <= 'f') && !(c >= 'A' && c <= 'F') {
				return false
			}
		}
		return true
	}
	if !text(s.ShowName, 128) || !text(s.Message, 256) || len(s.Inputs) > 32 {
		return CoreError("Invalid show metadata")
	}
	if !s.Available {
		if len(s.Inputs) == 0 && s.Revision == "" {
			return nil
		}
		return CoreError("Unavailable show contains input assignments")
	}
	sources := map[uint16]struct{}{}
	faders := map[uint8]struct{}{}
	targets := map[string]struct{}{}
	invalid := func(source StockSource) bool {
		if source.SourceID > 255 || source.ID != "input-"+strconv.Itoa(int(source.SourceID)) {
			return true
		}
		if _, duplicate := sources[source.SourceID]; duplicate {
			return true
		}
		sources[source.SourceID] = struct{}{}
		if source.Fader != nil {
			fader := *source.Fader
			if fader < 1 || fader > 9 {
				return true
			}
			if _, duplicate := faders[fader]; duplicate {
				return true
			}
			faders[fader] = struct{}{}
		}
		if !text(source.Name, 128) || strings.TrimSpace(source.Name) == "" || !hex(source.Color, 8) {
			return true
		}
		if source.TargetID != "" {
			if !ValidID(source.TargetID) {
				return true
			}
			if _, duplicate := targets[source.TargetID]; duplicate {
				return true
			}
			targets[source.TargetID] = struct{}{}
		}
		return false
	}
	if !hex(s.Revision, 64) {
		return CoreError("Invalid or duplicate show input")
	}
	for _, source := range s.Inputs {
		if invalid(source) {
			return CoreError("Invalid or duplicate show input")
		}
	}
	return nil
}
