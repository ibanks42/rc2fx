use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;

const BASE_PROFILE_COUNT: usize = 4;
const PROFILE_ID_MARKER: &str = "\n      \"id\": ";
const PROFILES_SUFFIX: &str = "\n  ]\n}\n";
const PAD_MARKER: &str = "\"notes\": \"";

pub fn resolve_case_text(dir: &Path, default_base: &str, case: &Value) -> String {
    let name = case["name"].as_str().expect("case missing string name");
    assert!(!name.is_empty(), "case missing name");
    let has_file = case.get("file").is_some();
    let has_base = case.get("base").is_some();
    let has_edits = case.get("edits").is_some();
    let has_generate = case.get("generate").is_some();

    if has_file {
        assert!(
            !has_base && !has_edits && !has_generate,
            "{name}: file cannot be combined with base, edits, or generate"
        );
        let file = case["file"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: file must be a string"));
        return std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("{name}: read {file}: {e}"));
    }

    assert!(
        has_edits ^ has_generate,
        "{name}: case must specify exactly one of file, edits, or generate"
    );

    let base_rel = if has_base {
        case["base"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: base must be a string"))
    } else {
        default_base
    };
    let base_text = std::fs::read_to_string(dir.join(base_rel))
        .unwrap_or_else(|e| panic!("{name}: read base {base_rel}: {e}"));

    if has_edits {
        let edits = case["edits"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: edits must be an array"));
        apply_edits(name, base_text, edits)
    } else {
        apply_generate(name, base_text, &case["generate"])
    }
}

fn apply_edits(name: &str, mut text: String, edits: &[Value]) -> String {
    assert!(!edits.is_empty(), "{name}: edits must not be empty");
    for (idx, edit) in edits.iter().enumerate() {
        let find = edit["find"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: edit {idx} missing string find"));
        let replace = edit["replace"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: edit {idx} missing string replace"));
        assert!(!find.is_empty(), "{name}: edit {idx} has empty find");
        let count = text.matches(find).count();
        assert_eq!(
            count, 1,
            "{name}: edit {idx} find matched {count} times, expected 1: {find:?}"
        );
        text = text.replacen(find, replace, 1);
    }
    text
}

fn apply_generate(name: &str, base: String, spec: &Value) -> String {
    let obj = spec
        .as_object()
        .unwrap_or_else(|| panic!("{name}: generate must be an object"));
    assert_eq!(
        obj.len(),
        1,
        "{name}: generate must have exactly one directive, got {obj:?}"
    );
    if let Some(count) = obj.get("profiles") {
        let count = count
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: generate.profiles must be a non-negative integer"))
            as usize;
        generate_profiles(name, base, count)
    } else if let Some(pad_bytes) = obj.get("padBytes") {
        let pad_bytes = pad_bytes
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: generate.padBytes must be a non-negative integer"))
            as usize;
        generate_pad_bytes(name, base, pad_bytes)
    } else {
        panic!("{name}: unknown generate directive {obj:?}");
    }
}

fn generate_profiles(name: &str, base: String, count: usize) -> String {
    assert!(
        (BASE_PROFILE_COUNT..=256).contains(&count),
        "{name}: generate.profiles must be in {BASE_PROFILE_COUNT}..=256, got {count}"
    );
    let base_profiles = base.matches(PROFILE_ID_MARKER).count();
    assert_eq!(
        base_profiles, BASE_PROFILE_COUNT,
        "{name}: generate.profiles requires a base with {BASE_PROFILE_COUNT} profiles, found {base_profiles}"
    );
    let prefix = base
        .strip_suffix(PROFILES_SUFFIX)
        .unwrap_or_else(|| panic!("{name}: base missing expected closing suffix"));
    let mut out = String::with_capacity(base.len() + (count - BASE_PROFILE_COUNT) * 512);
    out.push_str(prefix);
    for idx in BASE_PROFILE_COUNT..count {
        let mixer_hash = format!("{idx:02x}").repeat(32);
        write!(
            out,
            concat!(
                ",\n    {{\n",
                "      \"minimumAppVersion\": \"0.1.1\",\n",
                "      \"id\": \"x-{idx}\",\n",
                "      \"model\": \"rodecaster-duo\",\n",
                "      \"version\": \"x-{idx}\",\n",
                "      \"mixerSha256\": \"{mixer_hash}\",\n",
                "      \"launcherSha256\": \"f27368e6d68296ebb9c6f22401f2fe00317643bee23c08b86d44d183408fa263\",\n",
                "      \"archiveSha256\": \"4f3c7a022f07fd5b05955846e6a9052ad6ae214e12c04cda2d4749d775a304ed\",\n",
                "      \"archiveMd5\": \"09393fef8a9f3b627a39801148d85d1d\",\n",
                "      \"runtime\": null,\n",
                "      \"notes\": \"\"\n",
                "    }}"
            ),
            idx = idx,
            mixer_hash = mixer_hash,
        )
        .unwrap();
    }
    out.push_str(PROFILES_SUFFIX);
    out
}

fn generate_pad_bytes(name: &str, base: String, pad_bytes: usize) -> String {
    assert!(pad_bytes > 0, "{name}: generate.padBytes must be positive");
    let pos = base
        .find(PAD_MARKER)
        .unwrap_or_else(|| panic!("{name}: base missing notes marker"));
    let insert_at = pos + PAD_MARKER.len();
    let mut out = String::with_capacity(base.len() + pad_bytes);
    out.push_str(&base[..insert_at]);
    for _ in 0..pad_bytes {
        out.push('x');
    }
    out.push_str(&base[insert_at..]);
    out
}
