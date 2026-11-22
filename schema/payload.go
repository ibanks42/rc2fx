package schema

// PayloadNames lists all payload files in canonical envelope order (checksummed
// files first, then SHA256SUMS, then scripts).
var PayloadNames = [14]string{
	"librc2fx_native.so",
	"rc2fxd",
	"rc2fx-supervisor",
	"rc2fx-firmware",
	"firmware-profiles.json",
	"librc2fx_gui.so",
	"librc2fx_jackerr.so",
	"rc2fx_jackerr_snapshot",
	"mixer-release.json",
	"SHA256SUMS",
	"rc2fx.sh",
	"rc2fx-shim.sh",
	"rc2fx-gui-shim.sh",
	"install.sh",
}

// Pinned lists the files in SHA256SUMS order.
var Pinned = [9]string{
	"librc2fx_native.so",
	"rc2fxd",
	"rc2fx-supervisor",
	"rc2fx-firmware",
	"firmware-profiles.json",
	"librc2fx_gui.so",
	"librc2fx_jackerr.so",
	"rc2fx_jackerr_snapshot",
	"mixer-release.json",
}

// Binaries lists payload files that must be ARM64 ELF binaries.
var Binaries = [7]string{
	"librc2fx_native.so",
	"rc2fxd",
	"rc2fx-supervisor",
	"rc2fx-firmware",
	"librc2fx_gui.so",
	"librc2fx_jackerr.so",
	"rc2fx_jackerr_snapshot",
}

// Scripts lists shell scripts sourced from mixer/scripts.
var Scripts = [4]string{
	"rc2fx.sh",
	"rc2fx-shim.sh",
	"rc2fx-gui-shim.sh",
	"install.sh",
}
