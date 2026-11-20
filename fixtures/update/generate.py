#!/usr/bin/env python3
"""Writes fixtures/update/:
- dto.json: shared wire-contract cases for MixerTarget, StartRequest, Chunk,
  and Progress checked by both Rust (crates/rc2fx-device) and Go
  (desktop/installer and desktop/transport).
- envelope.bin: a complete valid 14-file RC2FX-EMBEDDED-PAYLOAD 1 release
  envelope checked by both Rust (rc2fx_device::mixer_update::payload::read)
  and Go (desktop/installer/embed).
- bad-envelopes/*.bin: malformed envelopes that both sides must refuse.
"""
import base64
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def build_envelope(files: list[tuple[str, bytes]]) -> bytes:
    body = bytearray()
    for name, data in files:
        body.extend(f"{sha256(data)}  {len(data)}  {name}\n".encode())
    for _, data in files:
        body.extend(data)
    header = (
        f"RC2FX-EMBEDDED-PAYLOAD 1\n"
        f"length {len(body)}\n"
        f"sha256 {sha256(bytes(body))}\n"
    ).encode()
    return header + bytes(body)


def make_envelopes():
    lists = json.loads((ROOT / "fixtures/payload/lists.json").read_text())
    payload_names = lists["payloadNames"]
    pinned = lists["pinned"]
    binaries = set(lists["binaries"])

    elf = bytearray(64)
    elf[0:6] = b"\x7fELF\x02\x01"
    elf[18] = 0xB7
    elf_bytes = bytes(elf)

    catalog = (ROOT / "config/firmware-profiles.json").read_bytes()
    release = b'{"schemaVersion":1,"version":"0.2.1","protocolVersion":1}\n'
    rc2fx_sh = (
        b"#!/bin/bash\n"
        b"rc2fx_build=release\n"
        b'case "${1:-}" in\n'
        b"  update-commit) exit 0 ;;\n"
        b"esac\n"
    )
    shim = b"#!/bin/sh\nexit 0\n"

    by_name: dict[str, bytes] = {}
    for name in payload_names:
        if name in binaries:
            by_name[name] = elf_bytes
        elif name == "firmware-profiles.json":
            by_name[name] = catalog
        elif name == "mixer-release.json":
            by_name[name] = release
        elif name == "rc2fx.sh":
            by_name[name] = rc2fx_sh
        elif name != "SHA256SUMS":
            by_name[name] = shim

    sums = "".join(f"{sha256(by_name[name])}  {name}\n" for name in pinned).encode()
    by_name["SHA256SUMS"] = sums

    ordered = [(name, by_name[name]) for name in payload_names]
    valid = build_envelope(ordered)
    (HERE / "envelope.bin").write_bytes(valid)

    bad_dir = HERE / "bad-envelopes"
    bad_dir.mkdir(exist_ok=True)
    # 1. Wrong magic header
    (bad_dir / "bad-magic.bin").write_bytes(b"NOT-A-PAYLOAD 1\n" + valid[25:])
    # 2. Body byte flipped (digest mismatch)
    corrupt = bytearray(valid)
    corrupt[-1] ^= 0x01
    (bad_dir / "bad-digest.bin").write_bytes(bytes(corrupt))
    # 3. Truncated body
    (bad_dir / "truncated.bin").write_bytes(valid[:-10])
    # 4. Development build line instead of release
    dev_by_name = dict(by_name)
    dev_by_name["rc2fx.sh"] = rc2fx_sh.replace(b"rc2fx_build=release", b"rc2fx_build=development")
    dev_ordered = [(name, dev_by_name[name]) for name in payload_names]
    (bad_dir / "dev-build.bin").write_bytes(build_envelope(dev_ordered))

    return valid


def make_dto(envelope: bytes):
    dto = {
        "mixerTargets": {
            "accept": [
                {
                    "name": "pro-ii-and-duo",
                    "json": {
                        "version": "0.2.1",
                        "payloadFormat": 1,
                        "protocolVersion": 1,
                        "minimumAppVersion": "0.1.7",
                        "firmwareProfiles": ["rodecaster-pro-ii-1.7.7", "rodecaster-duo-1.7.7"],
                    },
                },
            ],
            "reject": [
                {
                    "name": "unknown-field",
                    "json": {
                        "version": "0.2.1",
                        "payloadFormat": 1,
                        "protocolVersion": 1,
                        "minimumAppVersion": "0.1.7",
                        "firmwareProfiles": ["rodecaster-pro-ii-1.7.7"],
                        "extra": True,
                    },
                },
                {
                    "name": "missing-minimum-app",
                    "json": {
                        "version": "0.2.1",
                        "payloadFormat": 1,
                        "protocolVersion": 1,
                        "firmwareProfiles": ["rodecaster-pro-ii-1.7.7"],
                    },
                },
                {
                    "name": "empty-profiles",
                    "json": {
                        "version": "0.2.1",
                        "payloadFormat": 1,
                        "protocolVersion": 1,
                        "minimumAppVersion": "0.1.7",
                        "firmwareProfiles": [],
                    },
                },
            ],
        },
        "startRequests": {
            "accept": [
                {
                    "name": "whole-envelope",
                    "json": {
                        "version": "0.2.1",
                        "size": len(envelope),
                        "sha256": sha256(envelope),
                        "metadata": {
                            "root": ["{}"],
                            "timestamp": "{}",
                            "snapshot": "{}",
                            "targets": "{}",
                        },
                    },
                },
                {
                    "name": "delta-upload-with-downgrade",
                    "json": {
                        "version": "0.2.0",
                        "size": len(envelope),
                        "sha256": sha256(envelope),
                        "metadata": {
                            "root": ["{}", "{}"],
                            "timestamp": "{}",
                            "snapshot": "{}",
                            "targets": "{}",
                        },
                        "allowDowngrade": True,
                        "encoding": "delta-1",
                        "uploadSize": 1024,
                    },
                },
            ],
            "reject": [
                {
                    "name": "unknown-field",
                    "json": {
                        "version": "0.2.1",
                        "size": 100,
                        "sha256": "0" * 64,
                        "metadata": {
                            "root": ["{}"],
                            "timestamp": "{}",
                            "snapshot": "{}",
                            "targets": "{}",
                        },
                        "unexpected": 1,
                    },
                },
                {
                    "name": "missing-metadata",
                    "json": {
                        "version": "0.2.1",
                        "size": 100,
                        "sha256": "0" * 64,
                    },
                },
            ],
        },
        "chunks": {
            "accept": [
                {
                    "name": "first-chunk",
                    "rawHex": b"rc2fx-update-chunk".hex(),
                    "json": {
                        "offset": 0,
                        "data": base64.b64encode(b"rc2fx-update-chunk").decode(),
                    },
                },
                {
                    "name": "offset-chunk",
                    "rawHex": bytes([0, 1, 254, 255]).hex(),
                    "json": {
                        "offset": 196608,
                        "data": base64.b64encode(bytes([0, 1, 254, 255])).decode(),
                    },
                },
            ],
            "reject": [
                {
                    "name": "unknown-field",
                    "json": {"offset": 0, "data": "AA==", "extra": True},
                },
                {
                    "name": "missing-offset",
                    "json": {"data": "AA=="},
                },
            ],
        },
        "progress": [
            {
                "name": "accepting-plain",
                "json": {
                    "id": "0123456789abcdef",
                    "state": "accepting",
                    "version": "0.2.1",
                    "size": len(envelope),
                    "received": 4096,
                },
            },
            {
                "name": "accepting-delta",
                "json": {
                    "id": "fedcba9876543210",
                    "state": "accepting",
                    "version": "0.2.1",
                    "size": len(envelope),
                    "received": 512,
                    "encoding": "delta-1",
                    "uploadSize": 1024,
                },
            },
            {
                "name": "restarting",
                "json": {
                    "id": "0123456789abcdef",
                    "state": "restarting",
                    "version": "0.2.1",
                    "size": len(envelope),
                    "received": len(envelope),
                },
            },
            {
                "name": "failed",
                "json": {
                    "id": "0123456789abcdef",
                    "state": "failed",
                    "version": "0.2.1",
                    "size": len(envelope),
                    "received": len(envelope),
                    "error": "verification failed",
                },
            },
        ],
    }
    (HERE / "dto.json").write_text(json.dumps(dto, indent=2) + "\n")


if __name__ == "__main__":
    env = make_envelopes()
    make_dto(env)
