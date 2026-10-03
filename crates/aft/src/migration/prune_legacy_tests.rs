use super::*;

fn identity(pid: u32, executable: Option<&str>, args: &[&str]) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        executable: executable.map(str::to_owned),
        argv0: args.first().map(|arg| (*arg).to_owned()),
        args: Some(args.iter().map(|arg| (*arg).to_owned()).collect()),
    }
}

#[test]
fn the_census_classifies_aft_processes_and_refuses_lookalikes() {
    let spaced = "/Users/me/Library/Application Support/AFT Host/bin/aft";
    assert_eq!(
        classify(&identity(
            101,
            Some(spaced),
            &[spaced, "--subc", "/tmp/c.json"]
        )),
        Some(CensusFinding::Aft {
            pid: 101,
            daemon: true,
            command: format!("{spaced} --subc /tmp/c.json"),
        }),
        "an executable path with spaces is one field"
    );
    assert!(matches!(
        classify(&identity(102, None, &["ck-aft"])),
        Some(CensusFinding::Aft {
            pid: 102,
            daemon: false,
            ..
        })
    ));
    assert!(matches!(
        classify(&identity(104, Some("/opt/bin/aft-bridge"), &["aft-bridge"])),
        Some(CensusFinding::Unclassifiable { pid: 104, .. })
    ));
    assert!(matches!(
        classify(&ProcessIdentity { pid: 105, ..ProcessIdentity::default() }),
        Some(CensusFinding::Unclassifiable { pid: 105, ref reason, .. })
            if reason.contains("could not be read")
    ));
    assert_eq!(
        classify(&identity(106, Some("/usr/bin/craft"), &["craft"])),
        None
    );
    assert_eq!(
        classify(&identity(
            107,
            Some("/bin/bash"),
            &["/bin/bash", "/home/u/bin/aft"]
        )),
        None,
        "a shell running an aft script is not itself AFT; its aft child is"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_system_census_reads_every_process_and_never_lists_itself() {
    let findings = SystemCensus.take().unwrap();
    let me = std::process::id();
    assert!(findings.iter().all(|finding| match finding {
        CensusFinding::Aft { pid, .. } | CensusFinding::Unclassifiable { pid, .. } => *pid != me,
    }));
    let unreadable = findings
        .iter()
        .filter(|finding| matches!(finding, CensusFinding::Unclassifiable { reason, .. } if reason.contains("could not be read")))
        .collect::<Vec<_>>();
    assert!(unreadable.is_empty(), "{unreadable:?}");
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
