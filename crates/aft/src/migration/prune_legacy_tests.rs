use super::*;

#[test]
fn the_census_classifies_aft_processes_and_refuses_lookalikes() {
    let output = "\
  101 /usr/local/bin/aft /usr/local/bin/aft --subc /tmp/conn.json
  102 ck-aft           ck-aft
  103 node             node /opt/opencode/bin/opencode
  104 aft-bridge       aft-bridge --serve
  105 craft            craft build
  106 bash             /bin/bash /home/user/bin/aft serve
  200 aft              aft cache prune-legacy
";
    let findings = parse_ps_output(output, 200).unwrap();
    assert_eq!(
        findings,
        vec![
            CensusFinding::Aft {
                pid: 101,
                daemon: true,
                command: "/usr/local/bin/aft --subc /tmp/conn.json".to_owned(),
            },
            CensusFinding::Aft {
                pid: 102,
                daemon: false,
                command: "ck-aft".to_owned(),
            },
            CensusFinding::Unclassifiable {
                pid: 104,
                command: "aft-bridge --serve".to_owned(),
                reason: "its name mentions AFT but is not an aft or ck-aft executable".to_owned(),
            },
        ],
        "the shell running an aft script is not itself classified; its aft child would be"
    );
}

#[test]
fn an_unparsable_census_line_is_an_error() {
    assert!(parse_ps_output("not-a-pid aft aft\n", 1).is_err());
}

#[test]
fn dates_render_in_utc() {
    assert_eq!(format_ms(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_ms(1_798_761_600_000), "2027-01-01T00:00:00Z");
}

#[test]
fn reserved_names_are_not_legacy_sets() {
    let storage = tempfile::tempdir().unwrap();
    fs::create_dir_all(storage.path().join("semantic").join("models")).unwrap();
    fs::create_dir_all(storage.path().join("semantic").join("abc")).unwrap();
    fs::write(
        storage
            .path()
            .join("semantic")
            .join("abc")
            .join("semantic.bin"),
        b"x",
    )
    .unwrap();
    let inventory = inventory(storage.path(), SystemTime::now()).unwrap();
    assert_eq!(
        inventory
            .sets
            .iter()
            .map(|set| set.key.as_str())
            .collect::<Vec<_>>(),
        ["abc"]
    );
    assert_eq!(inventory.sets[0].eligibility, Eligibility::NotImported);
}

#[test]
fn a_tree_walk_stops_at_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..10 {
        fs::write(dir.path().join(format!("f{index}")), b"12345").unwrap();
    }
    let (_, _, complete) = tree_size(dir.path(), 4).unwrap();
    assert!(!complete);
    let (_, entries, complete) = tree_size(dir.path(), 100).unwrap();
    assert!(complete);
    assert_eq!(entries, 10);
}
