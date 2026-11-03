"""Reproduce the offline Pro II runtime audit against two official archives.

Install requirements.txt, then run python -m tools.firmware_audit --help.
The output contains addresses, hashes and fixture results, never vendor code.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from .canonical import canonical
from .elf import Image
from .emulate import exercise


ROOT = Path(__file__).resolve().parents[2]


def checked_image(archive, profile):
    image = Image(archive)
    if image.archive_sha256 != profile["archiveSha256"] or image.sha256 != profile["mixerSha256"]:
        raise ValueError(f"archive/mixer hash mismatch for {profile['id']}")
    if image.assets["usr/bin/audio-start.sh"] != profile["launcherSha256"]:
        raise ValueError("launcher hash mismatch")
    if image.elf["e_machine"] != "EM_AARCH64" or image.elf.elfclass != 64 or not image.elf.little_endian:
        raise ValueError("expected a little-endian AArch64 ELF")
    runtime = profile["runtime"]
    if runtime is None:
        raise ValueError("profile has no mapped runtime")
    descriptors = {"callback": runtime["callback"], **{
        name: runtime["jobLifetime"][name] for name in ("enqueue", "manager", "invoker", "body")
    }}
    sites = {}
    for name, descriptor in descriptors.items():
        address = int(descriptor["offset"], 16)
        if image.function_at(address)[0] != address:
            raise ValueError(f"{name} is not an unwind function entry")
        if image.code(address, 16).hex() != descriptor["expected"]:
            raise ValueError(f"{name} instruction signature mismatch")
        sites[name] = address
    references = image.address_references(sites["invoker"])
    if len(references) != 1:
        raise ValueError("expected exactly one queued-closure constructor")
    sites["mixerProcess"] = image.function_at(references[0][0])[0]
    enqueue_calls = {relative: target for relative, _, target, _ in canonical(image, sites["enqueue"])["calls"]}
    sites["functionCopyConstructor"] = enqueue_calls[0x28]
    callbacks = image.address_references(sites["callback"])
    parents = sorted({image.function_at(reference[0])[0] for reference in callbacks})
    if len(callbacks) != 2 or len(parents) != 2:
        raise ValueError("expected exactly two callback registration parents")
    sites["jackRegistrationTeardown"], sites["jackRegistrationActive"] = parents
    return image, sites


def audit(catalog, baseline_archive, candidate_archive, baseline_id, candidate_id):
    if not __debug__:
        raise ValueError("do not disable Python assertions for the emulation fixtures")
    profiles = {profile["id"]: profile for profile in catalog["profiles"]}
    baseline_profile, candidate_profile = profiles[baseline_id], profiles[candidate_id]
    baseline, old_sites = checked_image(baseline_archive, baseline_profile)
    candidate, new_sites = checked_image(candidate_archive, candidate_profile)
    for key in ("callbackAbi", "routeLayout", "sourceEnum", "sampleRate", "blockFrames", "showPath"):
        if baseline_profile["runtime"][key] != candidate_profile["runtime"][key]:
            raise ValueError(f"runtime layout differs: {key}")
    if baseline_profile["runtime"]["jobLifetime"]["abi"] != candidate_profile["runtime"]["jobLifetime"]["abi"]:
        raise ValueError("capture ABI differs")
    if len(baseline.assets) != 6 or baseline.assets != candidate.assets:
        raise ValueError("launcher, updater, gadget or linked ABI library differs")
    comparisons = {}
    for role, old_address in old_sites.items():
        new_address = new_sites[role]
        old, new = canonical(baseline, old_address), canonical(candidate, new_address)
        if old["instructions"] != new["instructions"]:
            raise ValueError(f"instruction stream changed: {role}")
        # Verify direct edges between mapped functions, in addition to retaining
        # imported symbol names and all internal control flow in the signature.
        for old_call, new_call in zip(old["calls"], new["calls"], strict=True):
            for target_role, target in old_sites.items():
                if old_call[2] == target and new_call[2] != new_sites[target_role]:
                    raise ValueError(f"mapped call edge changed: {role} -> {target_role}")
        comparisons[role] = {
            "baselineOffset": hex(old_address), "candidateOffset": hex(new_address),
            "bytes": baseline.functions[old_address], "instructions": len(old["instructions"]),
            "canonicalSha256": old["sha256"], "relocationNormalizedEqual": True,
            "externalCalls": [{"relative": hex(before[0]), "instruction": before[1],
                               "baselineTarget": hex(before[2]), "candidateTarget": hex(after[2]),
                               "import": before[3]}
                              for before, after in zip(old["calls"], new["calls"], strict=True)],
        }
    return {
        "schemaVersion": 1,
        "baselineProfile": baseline_id, "candidateProfile": candidate_id,
        "baselineArchiveSha256": baseline.archive_sha256,
        "candidateArchiveSha256": candidate.archive_sha256,
        "baselineMixerSha256": baseline.sha256, "candidateMixerSha256": candidate.sha256,
        "method": "Full unwind-bounded ARM64 instruction comparison, retaining registers, fields, widths, stack sizes, internal branches and import names; PC-relative external addresses and RTTI locations normalized. Unmapped internal callee implementations are outside this comparison.",
        "functions": comparisons, "identicalAssetSha256": baseline.assets,
        "emulation": {"baseline": exercise(baseline, old_sites), "candidate": exercise(candidate, new_sites)},
        "limitations": ["Offline fixtures stub allocation, strings, events and thread-pool submission; PAC/BTI hints skipped.",
                        "No mixer flash, deployment, audio playback, real-time or device transport validation performed."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True, help="official 1.7.6 os.tar.gz")
    parser.add_argument("--candidate", type=Path, required=True, help="official 1.7.7 os.tar.gz")
    parser.add_argument("--catalog", type=Path, default=ROOT / "config/firmware-profiles.json")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline-id", default="rodecaster-pro-ii-1.7.6")
    parser.add_argument("--candidate-id", default="rodecaster-pro-ii-1.7.7")
    args = parser.parse_args()
    report = audit(json.loads(args.catalog.read_text(encoding="utf-8")), args.baseline, args.candidate,
                   args.baseline_id, args.candidate_id)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"PASS: {len(report['functions'])} complete functions match; both ARM64 fixture sets passed. Report: {args.output}")


if __name__ == "__main__":
    main()
