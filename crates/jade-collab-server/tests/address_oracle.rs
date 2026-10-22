//! The actual UTF-8 path codec against the executable Lean specification.
use jade_collab_server::{document_name_from_url, document_path};
use proptest::prelude::*;
use std::io::Write;
use std::process::{Command, Stdio};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    #[ignore = "requires JADE_LEAN_ADDRESS_ORACLE; see lean/README.md"]
    fn utf8_document_paths_match_the_proved_byte_codec(
        stems in prop::collection::vec(prop::collection::vec(
            any::<char>().prop_filter("valid document character", |c| !c.is_control() && !matches!(c,'/'|'\\'|':')),
            0..32),1..12),
    ) {
        let mut input = String::new();
        let mut expected = Vec::new();
        for stem in stems {
            let name = format!("doc-{}.docx",stem.into_iter().collect::<String>());
            input.push_str(&name.bytes().map(|b|b.to_string()).collect::<Vec<_>>().join(" "));
            input.push('\n');
            let path = document_path(&name);
            expected.push(path.strip_prefix("/documents/").unwrap().to_string());
            let decoded = document_name_from_url(&format!("ws://h{path}/ws?token=secret"));
            prop_assert_eq!(decoded,Some(name));
        }
        let path = std::env::var("JADE_LEAN_ADDRESS_ORACLE").expect("set JADE_LEAN_ADDRESS_ORACLE");
        let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        prop_assert!(output.status.success());
        let actual = String::from_utf8(output.stdout).unwrap();
        prop_assert_eq!(actual.lines().collect::<Vec<_>>(),expected);
    }
}
