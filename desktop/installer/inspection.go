package installer

// Offline qualification tooling. Never extracts files, prepares an image,
// opens USB, or grants runtime support to an experimental board.

import (
	"os"
	"slices"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/firmware"
)

// Inspection is what an offline check of an official archive found.
type Inspection struct {
	Model              firmware.Model `json:"model"`
	Profile            string         `json:"profile"`
	ArchiveSHA256      string         `json:"archiveSha256"`
	ExecutableVerified bool           `json:"executableVerified"`
	RuntimeQualified   bool           `json:"runtimeQualified"`
	Notes              string         `json:"notes"`
}

// InspectArchive verifies an official archive against the built-in pins,
// model/board evidence, and the already-qualified executable ABI. A
// successful result for Duo is only offline evidence: RuntimeQualified
// stays false.
func InspectArchive(model firmware.Model, archive string) (*Inspection, error) {
	data, err := catalogBytes()
	if err != nil {
		return nil, err
	}
	if !utf8.Valid(data) {
		return nil, Error("the firmware catalog is not UTF-8")
	}
	catalog, err := firmware.ParseCatalog(string(data))
	if err != nil {
		return nil, Error(err.Error())
	}
	return InspectArchiveWithCatalog(model, archive, catalog)
}

// InspectArchiveWithCatalog lets offline tooling explicitly provide a
// catalog instead of downloading it.
func InspectArchiveWithCatalog(model firmware.Model, archive string, catalog *firmware.Catalog) (*Inspection, error) {
	info, err := os.Stat(archive)
	if err != nil {
		return nil, err
	}
	if info.Size() > maxDownload {
		return nil, Error("firmware exceeds size limit")
	}
	sha, md5sum, err := hashBoth(archive, func(uint64, uint64) {})
	if err != nil {
		return nil, err
	}
	i := slices.IndexFunc(catalog.Profiles, func(p firmware.Profile) bool {
		return p.Model == model.ID() && p.ArchiveSHA256 == sha && p.ArchiveMD5 == md5sum
	})
	if i < 0 {
		return nil, Error("archive is not pinned for the requested model (wrong model or uninspected release)")
	}
	release := catalog.Profiles[i]
	members, err := readMembers(archive, model)
	if err != nil {
		return nil, err
	}
	text := func(name string) (string, error) {
		if !utf8.Valid(members[name]) {
			return "", Error("invalid utf-8 in " + name)
		}
		return string(members[name]), nil
	}
	hostname, err := text("etc/hostname")
	if err != nil {
		return nil, err
	}
	extlinux, err := text("boot/extlinux/extlinux.conf")
	if err != nil {
		return nil, err
	}
	board, err := firmware.ModelFromBoard(hostname, extlinux)
	if err != nil {
		return nil, Error(err.Error())
	}
	if board != model {
		return nil, Error("archive board identity differs from the requested model")
	}
	profile, err := catalog.InspectForModel(model, members["usr/bin/rc_audio_mixer"], members["usr/bin/audio-start.sh"])
	if err != nil {
		return nil, Error(err.Error())
	}
	if profile.ID != release.ID {
		return nil, Error("archive and executable belong to different releases")
	}
	return &Inspection{
		Model:              model,
		Profile:            profile.ID,
		ArchiveSHA256:      sha,
		ExecutableVerified: true,
		RuntimeQualified:   model.RuntimeEnabled() && profile.Runtime != nil,
		Notes:              profile.Notes,
	}, nil
}

// verifyArchiveModel rechecks board evidence in a prepared archive as well.
// Cache metadata alone must never be enough to turn a Pro II image into a
// Duo image.
func verifyArchiveModel(model firmware.Model, archive string) error {
	members, err := readMembers(archive, model)
	if err != nil {
		return err
	}
	if !utf8.Valid(members["etc/hostname"]) || !utf8.Valid(members["boot/extlinux/extlinux.conf"]) {
		return Error("prepared archive identity is not UTF-8")
	}
	board, err := firmware.ModelFromBoard(string(members["etc/hostname"]), string(members["boot/extlinux/extlinux.conf"]))
	if err != nil {
		return Error(err.Error())
	}
	if board != model {
		return Error("prepared archive belongs to a different model")
	}
	return nil
}

func readMembers(archive string, model firmware.Model) (map[string][]byte, error) {
	required := []string{
		"etc/hostname",
		"boot/extlinux/extlinux.conf",
		"usr/bin/rc_audio_mixer",
		"usr/bin/audio-start.sh",
		model.BoardDTB()[1:],
	}
	members := map[string][]byte{}
	err := walkArchive(archive, 2<<30, false, func(entry *tarEntry) error {
		name := normalized(entry.path)
		if !slices.Contains(required, name) {
			return nil
		}
		limit := uint64(maxSmallMember)
		if name == "usr/bin/rc_audio_mixer" {
			limit = maxMixerMember
		}
		if _, ok := members[name]; ok || !entry.header.regular() || entry.size > limit {
			return errorf("duplicate, nonregular or oversized identity member %s", name)
		}
		data, err := readAll(entry)
		if err != nil {
			return err
		}
		members[name] = data
		return nil
	})
	if err != nil {
		return nil, err
	}
	for _, name := range required {
		if _, ok := members[name]; !ok {
			return nil, Error("archive is missing required model identity or executable members")
		}
	}
	return members, nil
}
