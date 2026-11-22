//! Live Go <-> Rust firmware model, catalog, and ELF verification parity tests.
//! Constructs synthetic AArch64 ELF images and catalog edge cases in code and
//! checks that `rc2fx-firmware` (Rust) and `desktop/firmware` (Go, invoked via
//! `desktop/internal/interop firmware`) reach identical verdicts.

use rc2fx_firmware::{Catalog, Model};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Builds a minimal valid 1024-byte AArch64 little-endian ELF64 PIE with one
/// RX PT_LOAD segment mapping vaddr 0x1000..0x1300 to file offset 0x100..0x400.
fn make_elf(two_segments: bool) -> Vec<u8> {
    let mut buf = vec![0u8; 1024];
    buf[0..4].copy_from_slice(b"\x7fELF");
    buf[4] = 2; // ELFCLASS64
    buf[5] = 1; // ELFDATA2LSB
    buf[6] = 1; // EV_CURRENT
    buf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN (PIE)
    buf[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
    buf[20..24].copy_from_slice(&1u32.to_le_bytes());
    buf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff = 64
    buf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize = 56
    let phnum: u16 = if two_segments { 2 } else { 1 };
    buf[56..58].copy_from_slice(&phnum.to_le_bytes());

    let write_phdr = |buf: &mut [u8],
                      idx: usize,
                      p_type: u32,
                      p_flags: u32,
                      off: u64,
                      vaddr: u64,
                      filesz: u64| {
        let base = 64 + idx * 56;
        buf[base..base + 4].copy_from_slice(&p_type.to_le_bytes());
        buf[base + 4..base + 8].copy_from_slice(&p_flags.to_le_bytes());
        buf[base + 8..base + 16].copy_from_slice(&off.to_le_bytes());
        buf[base + 16..base + 24].copy_from_slice(&vaddr.to_le_bytes());
        buf[base + 24..base + 32].copy_from_slice(&vaddr.to_le_bytes());
        buf[base + 32..base + 40].copy_from_slice(&filesz.to_le_bytes());
        buf[base + 40..base + 48].copy_from_slice(&filesz.to_le_bytes());
        buf[base + 48..base + 56].copy_from_slice(&0x1000u64.to_le_bytes());
    };
    if two_segments {
        // First segment: RO data at vaddr 0x0, second segment: RX code at vaddr 0x1000.
        write_phdr(&mut buf, 0, 1, 4, 0x0, 0x0, 0x100);
        write_phdr(&mut buf, 1, 1, 5, 0x100, 0x1000, 0x300);
    } else {
        write_phdr(&mut buf, 0, 1, 5, 0x100, 0x1000, 0x300);
    }
    // Five 16-byte instruction blocks at vaddr 0x1000, 0x1020, 0x1040, 0x1060, 0x1080
    // (file offsets 0x100, 0x120, 0x140, 0x160, 0x180).
    let insns: [u32; 4] = [0xd503_201f, 0xaa00_03e0, 0xd65f_03c0, 0xd503_201f];
    for slot in 0..5 {
        for (i, word) in insns.iter().enumerate() {
            let at = 0x100 + slot * 0x20 + i * 4;
            buf[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    buf
}

fn catalog_for(image: &[u8], launcher: &[u8], runtime_qualified: bool) -> String {
    let base: Value =
        serde_json::from_str(include_str!("../../../config/firmware-profiles.json")).unwrap();
    let mut p = base["profiles"][0].clone();
    p["id"] = json!("rodecaster-pro-ii-1.7.6");
    p["model"] = json!("rodecaster-pro-ii");
    p["version"] = json!("1.7.6");
    p["mixerSha256"] = json!(sha256_hex(image));
    p["launcherSha256"] = json!(sha256_hex(launcher));
    if runtime_qualified {
        let exp = "1f2003d5e00300aac0035fd61f2003d5";
        let rt = p["runtime"].as_object_mut().unwrap();
        rt.insert(
            "callback".into(),
            json!({"offset": "0x1000", "expected": exp}),
        );
        rt.insert(
            "jobLifetime".into(),
            json!({
                "abi": "capture-values-v1",
                "enqueue": {"offset": "0x1020", "expected": exp},
                "manager": {"offset": "0x1040", "expected": exp},
                "invoker": {"offset": "0x1060", "expected": exp},
                "body": {"offset": "0x1080", "expected": exp},
            }),
        );
    } else {
        p.as_object_mut().unwrap().remove("runtime");
    }
    serde_json::to_string(&json!({
        "schemaVersion": base["schemaVersion"],
        "profiles": [p]
    }))
    .unwrap()
}

fn call_go_firmware(payload: &Value) -> Value {
    let desktop = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop");
    let mut child = Command::new("go")
        .args(["run", "./internal/interop", "firmware"])
        .current_dir(&desktop)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `go run ./internal/interop firmware`");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(payload).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "go interop firmware failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn firmware_models_catalogs_and_selection_match_go_desktop() {
    let boards = vec![
        (
            "rodecaster-pro-ii",
            "LABEL primary\n  FDT /boot/meson-axg-rodecaster-pro-ii.dtb\n",
        ),
        (
            "rodecaster-pro-ii-mini",
            "LABEL primary\n  FDT /boot/meson-axg-rodecaster-pro-ii-mini.dtb\n",
        ),
        (
            "rodecaster-pro-ii\n",
            "fdt /boot/meson-axg-rodecaster-pro-ii.dtb",
        ),
        (
            "  rodecaster-pro-ii-mini \r\n",
            "LABEL x\r\n\tFdT\t/boot/meson-axg-rodecaster-pro-ii-mini.dtb\r\n",
        ),
        (
            "rodecaster-pro-ii-mini",
            "FDT /boot/meson-axg-rodecaster-pro-ii.dtb",
        ),
        (
            "rodecaster-pro-ii",
            "LABEL a\nFDT /boot/meson-axg-rodecaster-pro-ii.dtb\nFDT /boot/meson-axg-rodecaster-pro-ii.dtb\n",
        ),
        ("rodecaster-pro-ii", "# FDT ignored"),
        ("rodecaster-pro-ii", ""),
        ("rodecaster-pro-ii", "FDT"),
        (
            "rodecaster-pro-ii",
            "FDT /boot/meson-axg-rodecaster-pro-ii.dtb extra",
        ),
        (
            "rodecaster-pro-ii",
            "FDTDIR /boot\nFDT /boot/meson-axg-rodecaster-pro-ii.dtb",
        ),
        ("rodecaster", "FDT /boot/meson-axg-rodecaster-pro-ii.dtb"),
        ("rodecaster-pro-ii", "FDT /boot/other.dtb"),
    ];
    let parse_ids = vec![
        "rodecaster-pro-ii",
        "rodecaster-duo",
        "rodecaster-pro-ii-mini",
        "RODECASTER-DUO",
        "",
        "ProIi",
    ];

    let shipped = include_str!("../../../config/firmware-profiles.json").to_owned();
    let shipped_compact =
        serde_json::to_string(&serde_json::from_str::<Value>(&shipped).unwrap()).unwrap();
    let catalog_cases = vec![
        ("shipped-default", shipped.clone()),
        ("shipped-compact", shipped_compact),
        ("empty", "".into()),
        ("null", "null".into()),
        ("array", "[]".into()),
        ("no-profiles", r#"{"schemaVersion":1,"profiles":[]}"#.into()),
        (
            "profiles-null",
            r#"{"schemaVersion":1,"profiles":null}"#.into(),
        ),
        (
            "profiles-object",
            r#"{"schemaVersion":1,"profiles":{}}"#.into(),
        ),
        (
            "bad-schema-version",
            shipped.replacen("\"schemaVersion\": 1", "\"schemaVersion\": 99", 1),
        ),
        (
            "bad-callback-abi",
            shipped.replacen("callbackAbi\": \"", "callbackAbi\": \"bogus-", 1),
        ),
    ];

    let base_elf = make_elf(false);
    let two_seg_elf = make_elf(true);
    let mut flipped_elf = base_elf.clone();
    flipped_elf[0x100] ^= 0xff;
    let mut noexec_elf = base_elf.clone();
    noexec_elf[64 + 4..64 + 8].copy_from_slice(&4u32.to_le_bytes()); // PF_R only
    let mut not_load_elf = base_elf.clone();
    not_load_elf[64..68].copy_from_slice(&2u32.to_le_bytes()); // PT_DYNAMIC
    let mut elf32 = base_elf.clone();
    elf32[4] = 1;
    let mut not_pie = base_elf.clone();
    not_pie[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    let mut x86_64 = base_elf.clone();
    x86_64[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    let truncated_elf = base_elf[..96].to_vec();
    let mut short_seg = base_elf.clone();
    short_seg[64 + 32..64 + 40].copy_from_slice(&4u64.to_le_bytes()); // filesz=4 < site len
    let mut small_stride = base_elf.clone();
    small_stride[54..56].copy_from_slice(&32u16.to_le_bytes());
    let mut many_headers = base_elf.clone();
    many_headers[56..58].copy_from_slice(&500u16.to_le_bytes());

    let launcher = b"launcher-bytes".to_vec();
    let wrong_launcher = b"wrong-launcher!".to_vec();

    type SelectCase<'a> = (&'a str, &'a str, String, Vec<u8>, &'a str, Vec<u8>);
    // Build select cases dynamically using exact hashes of each synthetic ELF.
    let select_cases: Vec<SelectCase<'_>> = vec![
        (
            "valid-elf",
            "verifyImage",
            catalog_for(&base_elf, &launcher, true),
            base_elf.clone(),
            "",
            launcher.clone(),
        ),
        (
            "two-segments",
            "verifyImage",
            catalog_for(&two_seg_elf, &launcher, true),
            two_seg_elf,
            "",
            launcher.clone(),
        ),
        (
            "flipped",
            "verifyImage",
            catalog_for(&flipped_elf, &launcher, true),
            flipped_elf,
            "",
            launcher.clone(),
        ),
        (
            "noexec",
            "verifyImage",
            catalog_for(&noexec_elf, &launcher, true),
            noexec_elf,
            "",
            launcher.clone(),
        ),
        (
            "not-load",
            "verifyImage",
            catalog_for(&not_load_elf, &launcher, true),
            not_load_elf,
            "",
            launcher.clone(),
        ),
        (
            "elf32",
            "verifyImage",
            catalog_for(&elf32, &launcher, true),
            elf32,
            "",
            launcher.clone(),
        ),
        (
            "not-pie",
            "verifyImage",
            catalog_for(&not_pie, &launcher, true),
            not_pie,
            "",
            launcher.clone(),
        ),
        (
            "x86-64",
            "verifyImage",
            catalog_for(&x86_64, &launcher, true),
            x86_64,
            "",
            launcher.clone(),
        ),
        (
            "truncated",
            "verifyImage",
            catalog_for(&truncated_elf, &launcher, true),
            truncated_elf,
            "",
            launcher.clone(),
        ),
        (
            "short-segment",
            "verifyImage",
            catalog_for(&short_seg, &launcher, true),
            short_seg,
            "",
            launcher.clone(),
        ),
        (
            "small-stride",
            "verifyImage",
            catalog_for(&small_stride, &launcher, true),
            small_stride,
            "",
            launcher.clone(),
        ),
        (
            "many-headers",
            "verifyImage",
            catalog_for(&many_headers, &launcher, true),
            many_headers,
            "",
            launcher.clone(),
        ),
        (
            "unqualified",
            "verifyImage",
            catalog_for(&base_elf, &launcher, false),
            base_elf.clone(),
            "",
            launcher.clone(),
        ),
        (
            "inspect-ok",
            "inspectForModel",
            catalog_for(&base_elf, &launcher, true),
            base_elf.clone(),
            "rodecaster-pro-ii",
            launcher.clone(),
        ),
        (
            "inspect-wrong-launcher",
            "inspectForModel",
            catalog_for(&base_elf, &launcher, true),
            base_elf.clone(),
            "rodecaster-pro-ii",
            wrong_launcher.clone(),
        ),
        (
            "inspect-unqualified",
            "inspectForModel",
            catalog_for(&base_elf, &launcher, false),
            base_elf.clone(),
            "rodecaster-pro-ii",
            launcher.clone(),
        ),
        (
            "launcher-ok",
            "verifyLauncher",
            catalog_for(&base_elf, &launcher, true),
            base_elf.clone(),
            "",
            launcher.clone(),
        ),
        (
            "launcher-wrong",
            "verifyLauncher",
            catalog_for(&base_elf, &launcher, true),
            base_elf,
            "",
            wrong_launcher,
        ),
    ];

    let req = json!({
        "boards": boards.iter().map(|(h, e)| json!({"hostname": h, "extlinux": e})).collect::<Vec<_>>(),
        "parse": parse_ids,
        "catalogs": catalog_cases.iter().map(|(n, t)| json!({"name": n, "text": t})).collect::<Vec<_>>(),
        "select": select_cases.iter().map(|(n, op, cat, img, m, l)| json!({
            "name": n, "op": op, "catalog": cat,
            "imageHex": hex_encode(img), "model": m, "launcherHex": hex_encode(l),
        })).collect::<Vec<_>>(),
    });
    let go = call_go_firmware(&req);

    // 1. Models metadata must match Rust Model::ALL identically.
    let go_models = go["models"].as_array().unwrap();
    assert_eq!(go_models.len(), Model::ALL.len());
    for (m, gm) in Model::ALL.into_iter().zip(go_models) {
        assert_eq!(m.id(), gm["id"].as_str().unwrap());
        assert_eq!(m.hostname(), gm["hostname"].as_str().unwrap());
        assert_eq!(m.board_dtb(), gm["boardDtb"].as_str().unwrap());
        assert_eq!(m.route_layout(), gm["routeLayout"].as_str().unwrap());
        assert_eq!(m.source_enum(), gm["sourceEnum"].as_str().unwrap());
        assert_eq!(Some(m.runtime_enabled()), gm["runtimeEnabled"].as_bool());
    }

    // 2. Board detection matches Go.
    for ((h, e), gb) in boards.iter().zip(go["boards"].as_array().unwrap()) {
        let rs = Model::from_board(h, e);
        assert_eq!(rs.is_ok(), gb["ok"].as_bool().unwrap(), "board {h:?} {e:?}");
        if let Ok(m) = rs {
            assert_eq!(m.id(), gb["value"].as_str().unwrap());
        } else {
            assert_eq!(rs.unwrap_err(), gb["error"].as_str().unwrap());
        }
    }

    // 3. ParseModel matches Go.
    for (id, gp) in parse_ids.iter().zip(go["parse"].as_array().unwrap()) {
        let rs = Model::parse(id);
        assert_eq!(rs.is_ok(), gp["ok"].as_bool().unwrap(), "parse {id:?}");
        if let Ok(m) = rs {
            assert_eq!(m.id(), gp["value"].as_str().unwrap());
        }
    }

    // 4. Catalog parsing matches Go.
    for ((name, text), gc) in catalog_cases.iter().zip(go["catalogs"].as_array().unwrap()) {
        let rs = Catalog::parse(text);
        assert_eq!(
            rs.is_ok(),
            gc["ok"].as_bool().unwrap(),
            "catalog {name}: rust={:?} go={gc}",
            rs.err()
        );
    }

    // 5. Image/launcher verification and inspection match Go.
    for ((name, op, cat_text, img, model_id, launcher), gs) in
        select_cases.iter().zip(go["select"].as_array().unwrap())
    {
        let cat = Catalog::parse(cat_text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let rs = match *op {
            "verifyImage" => cat.profiles[0]
                .verify_image(img)
                .map(|()| cat.profiles[0].id.clone()),
            "inspectForModel" => cat
                .inspect_for_model(Model::parse(model_id).unwrap(), img, launcher)
                .map(|p| p.id.clone()),
            "verifyLauncher" => cat.profiles[0]
                .verify_launcher(launcher)
                .map(|()| cat.profiles[0].id.clone()),
            other => panic!("unknown op {other}"),
        };
        assert_eq!(
            rs.is_ok(),
            gs["ok"].as_bool().unwrap(),
            "select {name}: rust={rs:?} go={gs}"
        );
        match rs {
            Ok(id) => assert_eq!(id, gs["value"].as_str().unwrap(), "{name}"),
            Err(err) => assert_eq!(err, gs["error"].as_str().unwrap(), "{name}"),
        }
    }
}
