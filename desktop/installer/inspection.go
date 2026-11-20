package installer

// Offline firmware archive inspection. Read-only: never extracts files,
// prepares images, touches USB, or enables experimental boards.

import (
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"os"
	"slices"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/firmware"
)

type Inspection struct {
	Model              firmware.Model `json:"model"`
	Profile            string         `json:"profile"`
	ArchiveSHA256      string         `json:"archiveSha256"`
	ExecutableVerified bool           `json:"executableVerified"`
	RuntimeQualified   bool           `json:"runtimeQualified"`
	Notes              string         `json:"notes"`
}

// InspectArchive checks an official firmware archive against built-in pins,
// board identity, and executable hashes. For Duo, RuntimeQualified stays false.
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

// InspectArchiveWithCatalog runs InspectArchive against an explicit catalog.
func InspectArchiveWithCatalog(model firmware.Model, archivePath string, catalog *firmware.Catalog) (*Inspection, error) {
	info, err := os.Stat(archivePath)
	if err != nil {
		return nil, err
	}
	if info.Size() > maxDownload {
		return nil, Error("firmware exceeds size limit")
	}
	sha, md5sum, err := archive.HashBoth(archivePath, func(uint64, uint64) {})
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
	members, err := readMembers(archivePath, model)
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

// verifyArchiveModel rechecks board identity inside a prepared archive so
// cache metadata alone can't mislabel a Pro II image as Duo (or vice versa).
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

func readMembers(archivePath string, model firmware.Model) (map[string][]byte, error) {
	required := []string{
		"etc/hostname",
		"boot/extlinux/extlinux.conf",
		"usr/bin/rc_audio_mixer",
		"usr/bin/audio-start.sh",
		model.BoardDTB()[1:],
	}
	members := map[string][]byte{}
	err := archive.Walk(archivePath, 2<<30, false, func(entry *archive.Member) error {
		name := archive.Normalized(entry.Path)
		if !slices.Contains(required, name) {
			return nil
		}
		limit := uint64(archive.MaxSmallMember)
		if name == "usr/bin/rc_audio_mixer" {
			limit = archive.MaxMixerMember
		}
		if _, ok := members[name]; ok || !entry.Header.Regular() || entry.Size > limit {
			return errorf("duplicate, nonregular or oversized identity member %s", name)
		}
		data, err := archive.ReadAll(entry)
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
