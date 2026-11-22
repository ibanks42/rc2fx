package release

import (
	"os"
	"path/filepath"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

const mixerRefreshUsage = "usage: ./xtask mixer-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY"

func readAll(path string) ([]byte, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, task.Error(task.IOError(err))
	}
	return data, nil
}

func asTask(err error) error {
	if err == nil {
		return nil
	}
	return task.Error(err.Error())
}

// Refresh implements `xtask mixer-refresh` (and `app-refresh`), renewing
// snapshot and timestamp metadata without needing the targets key.
func Refresh(args []string) error {
	if len(args) != 7 {
		return task.Error(mixerRefreshUsage)
	}
	rootFile, input, output, sequenceText, expiresText, snapshotKey, timestampKey :=
		args[0], args[1], args[2], args[3], args[4], args[5], args[6]
	rootData, err := readAll(rootFile)
	if err != nil {
		return err
	}
	root, err := tufsign.ParseRoot(rootData)
	if err != nil {
		return asTask(err)
	}
	if err := root.VerifyRole(root.Document); err != nil {
		return asTask(err)
	}
	if root.ConsistentSnapshot {
		return task.Error("expected fixed GitHub metadata names")
	}
	targetsBytes, err := readAll(filepath.Join(input, "targets.json"))
	if err != nil {
		return err
	}
	targetsDoc, err := tufsign.ParseRole(targetsBytes, tufsign.RoleTargets)
	if err != nil {
		return asTask(err)
	}
	if err := root.VerifyRole(targetsDoc); err != nil {
		return asTask(err)
	}
	snapshotBytes, err := readAll(filepath.Join(input, "snapshot.json"))
	if err != nil {
		return err
	}
	snapshot, err := tufsign.ParseRole(snapshotBytes, tufsign.RoleSnapshot)
	if err != nil {
		return asTask(err)
	}
	if err := root.VerifyRole(snapshot); err != nil {
		return asTask(err)
	}
	timestampBytes, err := readAll(filepath.Join(input, "timestamp.json"))
	if err != nil {
		return err
	}
	timestamp, err := tufsign.ParseRole(timestampBytes, tufsign.RoleTimestamp)
	if err != nil {
		return asTask(err)
	}
	if err := root.VerifyRole(timestamp); err != nil {
		return asTask(err)
	}
	// Verify the existing hash chain as well as signatures so we can't splice
	// targets from another snapshot.
	snapshotMeta, _ := tufsign.Meta(snapshot)
	timestampMeta, _ := tufsign.Meta(timestamp)
	if err := checkPin(snapshotMeta, "targets.json", targetsBytes, targetsDoc.Version); err != nil {
		return err
	}
	if err := checkPin(timestampMeta, "snapshot.json", snapshotBytes, snapshot.Version); err != nil {
		return err
	}
	targets, err := tufsign.TargetsOf(targetsDoc)
	if err != nil {
		return asTask(err)
	}
	if len(snapshotMeta) != 1 || targets.DelegatedRoles > 0 {
		return task.Error("delegated metadata is not supported for renewal")
	}
	sequence, ok := common.ParseUint(sequenceText, 64)
	if !ok || sequence == 0 {
		return task.Error("invalid sequence")
	}
	expires, err := tufsign.ParseTimestamp(expiresText)
	if err != nil {
		return task.Errorf("invalid expiry: %s", err)
	}
	if sequence <= snapshot.Version || sequence <= timestamp.Version {
		return task.Error("renewal sequence must increase")
	}
	if !expires.After(time.Now()) || !expires.Before(targetsDoc.Expires) || !expires.Before(root.Expires) {
		return task.Error("renewal must expire in the future and before the existing targets and root; reauthorize targets offline if needed")
	}
	var signers []tufsign.Signer
	for _, key := range []struct{ role, path string }{
		{tufsign.RoleSnapshot, snapshotKey},
		{tufsign.RoleTimestamp, timestampKey},
	} {
		signer, err := tufsign.LoadKey(key.path)
		if err != nil {
			return asTask(err)
		}
		id, err := tufsign.SignerID(signer)
		if err != nil {
			return asTask(err)
		}
		for _, forbidden := range []string{tufsign.RoleRoot, tufsign.RoleTargets} {
			if root.RoleHas(forbidden, id) {
				return task.Error("renewal keys must not authorize root or targets")
			}
		}
		if !root.RoleHas(key.role, id) {
			return task.Errorf("key is not authorized for %s", key.role)
		}
		signers = append(signers, signer)
	}
	meta, _ := snapshot.Signed.Get("meta")
	renewedSnapshot := tufsign.Renewed(snapshot.Signed, sequence, expires, tufsign.ToughMeta(meta))
	snapshotFile, err := root.Sign(tufsign.RoleSnapshot, renewedSnapshot, signers)
	if err != nil {
		return asTask(err)
	}
	pin := tufsign.MetafileValue(uint64(len(snapshotFile)), sha256Bytes(snapshotFile), sequence)
	renewedTimestamp := tufsign.Renewed(timestamp.Signed, sequence, expires,
		core.ObjectValue(core.Member{Key: "snapshot.json", Value: pin}))
	timestampFile, err := root.Sign(tufsign.RoleTimestamp, renewedTimestamp, signers)
	if err != nil {
		return asTask(err)
	}
	if common.PathExists(output) {
		return task.Error("renewal output already exists")
	}
	parent := outputParent(output)
	if err := os.MkdirAll(parent, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	stage, err := tufsign.StageDir(parent)
	if err != nil {
		return err
	}
	moved := false
	defer func() {
		if !moved {
			_ = os.RemoveAll(stage)
		}
	}()
	for _, file := range []struct {
		name string
		data []byte
	}{
		{"targets.json", targetsBytes},
		{"snapshot.json", snapshotFile},
		{"timestamp.json", timestampFile},
	} {
		if err := os.WriteFile(filepath.Join(stage, file.name), file.data, 0o666); err != nil {
			return task.Error(task.IOError(err))
		}
	}
	if err := os.Rename(stage, output); err != nil {
		return task.Error(task.IOError(err))
	}
	moved = true
	return nil
}

func checkPin(meta map[string]tufsign.Metafile, name string, data []byte, version uint64) error {
	pin, ok := meta[name]
	if !ok {
		return task.Error("metadata chain is incomplete")
	}
	if pin.Version != version || pin.Length == nil || *pin.Length != uint64(len(data)) ||
		!pin.HasHashes || string(pin.SHA256) != string(sha256Bytes(data)) {
		return task.Error("metadata chain does not match its signed hashes")
	}
	return nil
}

func sha256Bytes(data []byte) []byte {
	sum := sha256Sum(data)
	return sum[:]
}

func outputParent(output string) string {
	return filepath.Dir(output)
}
