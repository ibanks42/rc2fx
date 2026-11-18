package installer

// Tests never use the helper's real folder for anchor records: only the ones
// a test sets (Engine.anchorDir). Without this, tests run as root read and
// create /var/lib/rc2fx/prepared.
func init() {
	anchorDirectory = func(string) (string, error) { return "", nil }
}
