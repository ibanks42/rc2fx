//! Inline mixer-side tests for `ImageCheck::from_record` and its compact JSON
//! output. Go-side decoding of `ImageCheck` reports is tested in
//! `desktop/core/image_check_fixtures_test.go`; both sides share the struct
//! definition generated from `schema/core.go`.

use rc2fx_core::ImageCheck;

#[test]
fn records_produce_expected_reports() {
    let mut long_list = String::from("status=damaged\nmanifest=abc\n");
    for i in 0..35 {
        long_list.push_str(&format!("damaged=file{i}\n"));
    }
    let long_list_expect = format!(
        r#"{{"status":"damaged","damaged":[{}],"unlisted":3}}"#,
        (0..32)
            .map(|i| format!(r#""file{i}""#))
            .collect::<Vec<_>>()
            .join(",")
    );

    let mut both_capped = String::from("status=damaged\nmanifest=abc\n");
    for i in 0..33 {
        both_capped.push_str(&format!("repaired=r{i}\ndamaged=d{i}\n"));
    }
    let both_capped_expect = format!(
        r#"{{"status":"damaged","repaired":[{}],"damaged":[{}],"unlisted":2}}"#,
        (0..32)
            .map(|i| format!(r#""r{i}""#))
            .collect::<Vec<_>>()
            .join(","),
        (0..32)
            .map(|i| format!(r#""d{i}""#))
            .collect::<Vec<_>>()
            .join(",")
    );

    let long_values = format!(
        "status=repaired\nmanifest=abc\nchecked={}\nrepaired={}\n",
        "é".repeat(70),
        "x".repeat(300)
    );
    let long_values_expect = format!(
        r#"{{"status":"repaired","checked":"{}","repaired":["{}"]}}"#,
        "é".repeat(64),
        "x".repeat(256)
    );

    let repaired_rec = "schema=1\nstatus=repaired\nmanifest=abc\nchecked=2026-11-15T22:10:00Z\nrepaired=usr/bin/rode-init-update-env.sh\n";
    let cases: Vec<(&str, Option<&str>, Option<&str>, &str)> = vec![
        (
            "repaired",
            Some(repaired_rec),
            Some("abc"),
            r#"{"status":"repaired","checked":"2026-11-15T22:10:00Z","repaired":["usr/bin/rode-init-update-env.sh"]}"#,
        ),
        (
            "no-file-list",
            Some(repaired_rec),
            None,
            r#"{"status":"none"}"#,
        ),
        ("no-record", None, Some("abc"), r#"{"status":"pending"}"#),
        (
            "stale-record",
            Some(repaired_rec),
            Some("other"),
            r#"{"status":"pending"}"#,
        ),
        (
            "unknown-status",
            Some("status=bogus\nmanifest=abc\n"),
            Some("abc"),
            r#"{"status":"pending"}"#,
        ),
        (
            "digest-prefix",
            Some("status=ok\nmanifest=abcd\n"),
            Some("abc"),
            r#"{"status":"pending"}"#,
        ),
        (
            "no-manifest-line",
            Some("status=ok\n"),
            Some("abc"),
            r#"{"status":"pending"}"#,
        ),
        (
            "no-status-line",
            Some("manifest=abc\nrepaired=etc/hostname\n"),
            Some("abc"),
            r#"{"status":"pending"}"#,
        ),
        (
            "empty-record",
            Some(""),
            Some("abc"),
            r#"{"status":"pending"}"#,
        ),
        (
            "long-list",
            Some(&long_list),
            Some("abc"),
            &long_list_expect,
        ),
        (
            "both-lists-capped",
            Some(&both_capped),
            Some("abc"),
            &both_capped_expect,
        ),
        (
            "crlf",
            Some("status=ok\r\nmanifest=abc\r\nchecked=x\r\n"),
            Some("abc"),
            r#"{"status":"ok","checked":"x"}"#,
        ),
        (
            "final-carriage-return",
            Some("status=ok\nmanifest=abc\nchecked=x\r"),
            Some("abc"),
            r#"{"status":"ok","checked":"x\r"}"#,
        ),
        (
            "no-final-newline",
            Some("status=ok\nmanifest=abc"),
            Some("abc"),
            r#"{"status":"ok"}"#,
        ),
        (
            "other-lines",
            Some(
                "# comment\nschema=1\nnoequals\nlater=thing\nmanifest=abc\nrepaired=a=b\nstatus=ok\n",
            ),
            Some("abc"),
            r#"{"status":"ok","repaired":["a=b"]}"#,
        ),
        (
            "last-status-wins",
            Some("status=ok\nstatus=damaged\nmanifest=abc\n"),
            Some("abc"),
            r#"{"status":"damaged"}"#,
        ),
        (
            "unknown-status-clears",
            Some("status=ok\nstatus=bogus\nmanifest=abc\n"),
            Some("abc"),
            r#"{"status":"pending"}"#,
        ),
        (
            "last-manifest-wins",
            Some("status=ok\nmanifest=old\nmanifest=abc\n"),
            Some("abc"),
            r#"{"status":"ok"}"#,
        ),
        (
            "last-checked-wins",
            Some("status=ok\nmanifest=abc\nchecked=one\nchecked=two\n"),
            Some("abc"),
            r#"{"status":"ok","checked":"two"}"#,
        ),
        (
            "long-values",
            Some(&long_values),
            Some("abc"),
            &long_values_expect,
        ),
        (
            "empty-values",
            Some("status=ok\nmanifest=abc\nchecked=\nrepaired=\n"),
            Some("abc"),
            r#"{"status":"ok","checked":"","repaired":[""]}"#,
        ),
    ];

    for (name, record, digest, expect) in cases {
        let check = ImageCheck::from_record(record, digest);
        assert_eq!(serde_json::to_string(&check).unwrap(), expect, "{name}");
    }
}
