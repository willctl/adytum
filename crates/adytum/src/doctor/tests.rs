use super::*;

const GIB: u64 = 1024 * 1024 * 1024;

fn fine() -> Machine {
    Machine {
        limactl: Some(KNOWN_LIMA_VERSIONS[0].to_owned()),
        home: Some(PathBuf::from("/Users/desk/.adytum")),
        lima_home_exists: true,
        image_cached: true,
        free_bytes: Some(200 * GIB),
        chip: Some("Apple M3 Max".to_owned()),
        macos: Some("26.5".to_owned()),
        ledger: LedgerStatus::Open { unsettled: 0 },
    }
}

fn get(machine: &Machine, id: &str) -> Finding {
    checks(machine).into_iter().find(|f| f.id == id).expect("every machine has this finding")
}

fn is_warn(f: &Finding) -> bool {
    matches!(f.verdict, Verdict::Warn { .. })
}

fn is_fail(f: &Finding) -> bool {
    matches!(f.verdict, Verdict::Fail { .. })
}

#[test]
fn a_fine_machine_has_nothing_to_do() {
    let all = checks(&fine());
    assert!(all.iter().all(|f| f.verdict == Verdict::Ok), "{all:?}");
    assert!(!has_failure(&all));
}

#[test]
fn limactl_missing_fails_and_an_unknown_version_warns_with_the_known_ones() {
    assert!(is_fail(&get(&Machine { limactl: None, ..fine() }, "limactl")));
    let f = get(&Machine { limactl: Some("9.9.9".to_owned()), ..fine() }, "limactl");
    assert!(is_warn(&f) && f.message.contains("9.9.9") && f.message.contains(KNOWN_LIMA_VERSIONS[0]), "{f:?}");
}

#[test]
fn a_long_adytum_folder_warns_and_then_fails_before_lima_hits_the_socket_limit() {
    // 104 bytes less "/", a 54-character name, "/default_ep.sock" and NUL leaves 32 for the Lima folder.
    let at = |n: usize| Machine { home: Some(PathBuf::from(format!("/{}", "a".repeat(n - "/lima".len() - 1)))), ..fine() };
    assert_eq!(get(&at(32), "socket-path").verdict, Verdict::Ok);
    assert!(is_warn(&get(&at(33), "socket-path")));
    let f = get(&at(80), "socket-path");
    assert!(is_fail(&f) && f.fix().is_some_and(|fix| fix.contains("ADYTUM_HOME")));
}

#[test]
fn what_the_first_create_makes_or_downloads_is_no_problem() {
    assert_eq!(get(&Machine { image_cached: false, ..fine() }, "image-cache").verdict, Verdict::Ok);
    assert_eq!(get(&Machine { lima_home_exists: false, ..fine() }, "lima-home").verdict, Verdict::Ok);
}

#[test]
fn under_20_gib_free_fails_with_the_number() {
    let f = get(&Machine { free_bytes: Some(5 * GIB + GIB / 2), ..fine() }, "disk");
    assert!(is_fail(&f) && f.message.contains("5.5 GiB"), "{f:?}");
    assert_eq!(get(&Machine { free_bytes: Some(20 * GIB), ..fine() }, "disk").verdict, Verdict::Ok);
    assert!(is_warn(&get(&Machine { free_bytes: None, ..fine() }, "disk")));
}

#[test]
fn a_ledger_to_set_up_again_fails_and_unsettled_operations_warn() {
    let m = Machine { ledger: LedgerStatus::SetUpAgain("it was made by another version".to_owned()), ..fine() };
    assert!(is_fail(&get(&m, "ledger")));
    assert!(!checks(&m).iter().any(|f| f.id == "ledger-settled"), "no count for a ledger that does not open");
    let m = Machine { ledger: LedgerStatus::Open { unsettled: 2 }, ..fine() };
    assert!(is_warn(&get(&m, "ledger-settled")));
    assert_eq!(get(&m, "ledger").verdict, Verdict::Ok);
    assert_eq!(get(&Machine { ledger: LedgerStatus::Missing, ..fine() }, "ledger").verdict, Verdict::Ok);
}

#[test]
fn nested_virtualization_is_reported_for_m3_and_later_on_macos_15_and_later() {
    let can = |chip: &str, macos: &str| get(&Machine { chip: Some(chip.to_owned()), macos: Some(macos.to_owned()), ..fine() }, "nested-virtualization");
    for (chip, macos) in [("Apple M3", "15.0"), ("Apple M3 Max", "26.5.2"), ("Apple M4 Pro", "15.4"), ("Apple M10", "30")] {
        let f = can(chip, macos);
        assert!(f.verdict == Verdict::Ok && f.message.contains(" can run"), "{chip} {macos}: {f:?}");
    }
    for (chip, macos) in [("Apple M1", "26.5"), ("Apple M2 Ultra", "15.0"), ("Apple M3", "14.6"), ("Intel(R) Core(TM) i9", "15.0")] {
        let f = can(chip, macos);
        assert!(f.verdict == Verdict::Ok && f.message.contains("cannot"), "{chip} {macos}: {f:?}");
    }
    let unknown = get(&Machine { chip: None, macos: None, ..fine() }, "nested-virtualization");
    assert_eq!(unknown.verdict, Verdict::Ok);
}

#[test]
fn no_home_fails() {
    assert!(has_failure(&checks(&Machine { home: None, ..fine() })));
}

#[test]
fn the_answer_prints_for_people_and_for_scripts() {
    let all = checks(&Machine { limactl: Some("9.9.9".to_owned()), free_bytes: Some(GIB), ..fine() });
    let text = render_human(&all);
    assert!(text.lines().any(|l| l.starts_with("● ok")), "{text}");
    assert!(text.lines().any(|l| l.starts_with("▲ warn") && l.contains("limactl")), "{text}");
    assert!(text.lines().any(|l| l.starts_with("✕ fail") && l.contains("disk")), "{text}");
    assert!(text.contains("       fix: "));
    assert!(!render_human(&checks(&fine())).contains("fix:"));

    let json: serde_json::Value = serde_json::from_str(&render_json(&all)).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["checks"][0], serde_json::json!({ "id": "limactl", "ok": false, "level": "warn", "message": all[0].message, "fix": all[0].fix() }));
    let last = json["checks"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((last["ok"].clone(), last["level"].clone(), last["fix"].clone()), (serde_json::json!(true), serde_json::json!("info"), serde_json::Value::Null));
    let fine_json: serde_json::Value = serde_json::from_str(&render_json(&checks(&fine()))).unwrap();
    assert_eq!(fine_json["ok"], true);
}

#[test]
fn version_lines_read_to_the_version() {
    assert_eq!(version("limactl version 2.1.2\n").as_deref(), Some("2.1.2"));
    assert_eq!(version("limactl version v2.1.2").as_deref(), Some("2.1.2"));
    assert_eq!(version(""), None);
}

#[test]
fn df_reads_available_blocks_even_when_the_file_system_has_spaces() {
    let mac = "Filesystem   1024-blocks      Used Available Capacity  Mounted on\n/dev/disk3s5  971350180 500000000 100000000    84%    /System/Volumes/Data\n";
    assert_eq!(parse_df(mac), Some(100_000_000 * 1024));
    let spaced = "Filesystem 1024-blocks Used Available Capacity Mounted on\nmap auto_home 0 0 0 100% /System/Volumes/Data/home\n";
    assert_eq!(parse_df(spaced), Some(0));
    assert_eq!(parse_df("nothing"), None);
    assert_eq!(parse_df("Filesystem\nbroken line"), None);
}

#[test]
fn a_cache_entry_is_named_by_the_hash_of_the_url() {
    // The folder Lima 2.1.2 made for this URL.
    assert_eq!(
        sha256_hex("https://cloud-images.ubuntu.com/releases/noble/release-20260518/ubuntu-24.04-server-cloudimg-arm64.img"),
        "2307d0ba402620cbfbfd84cb192964b33bc4f4c491640945aab32c0787ed31ef"
    );
}

#[tokio::test]
async fn this_machine_answers() {
    let all = checks(&gather().await);
    assert!(all.iter().any(|f| f.id == "limactl"));
    assert_ne!(render_human(&all).len(), 0);
}
