// Package release signs and renews TUF release metadata for both the mixer and
// desktop app, generates signing keys, and packages standalone mixer releases.
package release

import (
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// Keys implements `xtask mixer-keys DIR`, generating local signing keys without
// touching GitHub or the checkout.
func Keys(destination string) error {
	if err := createKeys(destination, task.Root()); err != nil {
		return err
	}
	fmt.Printf("Created signing material in %s\n", destination)
	fmt.Println("Read SETUP.md there. Back up offline/root.pem offline; never upload it.")
	fmt.Println("Only public/root.json belongs in the checkout. No repository files or GitHub settings were changed.")
	return nil
}

const instructions = `# RC2FX signing setup

Generation is complete only if COMPLETE exists. If it does not, keep this
folder private and retry in a different new folder; do not use a partial root.
The command never uploads keys, changes the checkout, or publishes releases.

## 1. Back up the root key

The private keys here are unencrypted PEM files protected by owner-only Unix
permissions. Use an encrypted disk and encrypted offline backup storage.
The root has one signer (you), a threshold of one, and a three-year expiry.
Each release role has a separate key and a threshold of one. Keys are RSA-3072
with RSA-PSS/SHA-256 signatures, in the PEM format accepted by the signer.

Copy offline/root.pem, public/root.json and public/root.sha256 to secure offline
backup storage. Verify the backup is readable before removing any original.
Keep a second protected backup. Keep the root private key off GitHub and out of
the checkout, logs, chat, and ordinary cloud-sync folders. Generation on this
computer is not an air-gapped ceremony; for that, run this tool on an offline
Linux/macOS machine with the executable and dependencies prepared beforehand.
After verifying backups, retain root.pem offline, not on the build machine.

## 2. Install only the public root

Copy public/root.json to config/mixer-update-root.json in the RC2FX checkout.
Only this JSON contains public keys and signatures safe to commit. Review its
role IDs and expiry and record a reminder to rotate the root well before expiry.
Keep the numbered root chain for existing clients. Do not replace a deployed
root with an unrelated root or rerun this tool to rotate deployed keys.
Rebuild and test the app after installing its initial public trust root.

## 3. Configure GitHub environments

In repository settings, create environments restricted to protected main:

- mixer-signing: require a release reviewer. Add MIXER_TARGETS_KEY from
  release/targets.pem, MIXER_SNAPSHOT_KEY from release/snapshot.pem and
  MIXER_TIMESTAMP_KEY from release/timestamp.pem.
- mixer-renewal: add only MIXER_SNAPSHOT_KEY and MIXER_TIMESTAMP_KEY, from the
  same files. It must not contain the targets key or the root key.

Use GitHub's secret-entry UI or gh secret set with file input. Never put private
key contents in shell arguments. Protect backups of release keys as well.
Do not upload the key directory as an artifact. Private keys are never copied
into release packages. No environment receives offline/root.pem.

## 4. Publish only when separately approved

Follow docs/developer/mixer-releases.md. Review both minimum app versions and
current qualification results before approving a mixer release. Do not enable
MIXER_UPDATES_ENABLED until the first signed payload and metadata are public.
Then set that repository variable to true for scheduled metadata renewal.
Short-lived metadata requires monitoring and renewal; signing setup alone does
not make updates available. This setup does not authorize publication.
`
