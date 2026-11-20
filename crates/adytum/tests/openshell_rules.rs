//! The refusals and parsers that need no runner. Each refusal is tested
//! before the rule that makes it.

use adytum::openshell::*;

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn win() -> Host {
    Host::new(true, Some("D:\\Windows".to_owned()))
}

fn mac() -> Host {
    Host::new(false, None)
}

/// The error's code, or "taken" when the value was accepted.
fn why<T>(r: Result<T, adytum::Error>) -> &'static str {
    match r {
        Ok(_) => "taken",
        Err(e) => e.code(),
    }
}

#[test]
fn a_distribution_must_be_a_plain_word() {
    for bad in ["Ubuntu; calc", "Ubuntu 24.04", "Ub\u{fc}ntu", "a&b", "", &"x".repeat(81)] {
        assert_eq!(why(Distro::new(bad)), "invalid", "{bad:?}");
    }
    assert!(Distro::new("Ubuntu-24.04").is_ok());
}

#[test]
fn native_is_argv_and_wsl_names_its_distribution() {
    let args = s(&["sandbox", "create", "--from", "; rm -rf ~"]);
    let native = Profile::Native.invocation(&mac(), &args).unwrap();
    assert_eq!((native.file.as_str(), &native.args), ("openshell", &args));
    let wsl = Profile::Wsl(Distro::new("Ubuntu-24.04").unwrap()).invocation(&win(), &args).unwrap();
    assert_eq!(wsl.args[..4], s(&["--distribution", "Ubuntu-24.04", "--exec", "openshell"])[..]);
    assert_eq!(wsl.args[7], "; rm -rf ~", "the input stays one literal argument");
    assert!(wsl.file.ends_with("wsl.exe") && wsl.file.starts_with("D:\\Windows"), "{}", wsl.file);
}

#[test]
fn a_profile_that_does_not_fit_the_machine_is_refused() {
    let wsl = Profile::Wsl(Distro::new("Ubuntu").unwrap());
    assert_eq!(why(wsl.invocation(&mac(), &[])), "unsupported");
    assert_eq!(why(Profile::Native.invocation(&win(), &[])), "unsupported");
    assert_eq!(why(wsl.path_translation(&mac(), "C:\\p")), "unsupported");
}

#[test]
fn wsl_paths_are_translated_by_wslpath_and_checked() {
    let wsl = Profile::Wsl(Distro::new("Ubuntu").unwrap());
    let inv = wsl.path_translation(&win(), "C:\\work\\p").unwrap().unwrap();
    assert_eq!(inv.args, s(&["--distribution", "Ubuntu", "--exec", "wslpath", "-a", "C:\\work\\p"]));
    assert_eq!(Profile::Native.path_translation(&mac(), "/x").unwrap(), None);
    assert_eq!(parse_wslpath("/mnt/c/work/p\r\n").unwrap(), "/mnt/c/work/p");
    for bad in ["C:\\p", "relative", "/a\n/b", "/mnt/c:x", ""] {
        assert_eq!(why(parse_wslpath(bad)), "invalid", "{bad:?}");
    }
}

#[test]
fn what_wsl_list_prints_becomes_names_only() {
    let names: Vec<String> = parse_wsl_list("\u{feff}U\0b\0u\0n\0t\0u\0\r\n\0docker-desktop\r\nbad name\r\n").iter().map(|d| d.as_str().to_owned()).collect();
    assert_eq!(names, ["Ubuntu"]);
}

#[test]
fn a_sandbox_name_cannot_read_as_a_flag_and_a_new_one_fits_openshell() {
    for bad in ["", "-rf", "a b", "a;b", "a/b", "a\nb", "_x"] {
        assert!(SandboxName::new(bad).is_err(), "{bad:?}");
    }
    assert!(SandboxName::new("xeno-0123456789abcd").is_ok());
    assert!(SandboxName::for_create("xeno-0123456789abcd").is_ok(), "19 characters");
    assert_eq!(why(SandboxName::for_create("xeno-0123456789abcde")), "invalid");
}

#[test]
fn image_provider_and_label_are_plain() {
    assert_eq!(why(ImageRef::new("--privileged")), "invalid");
    assert_eq!(why(ImageRef::new("a b")), "invalid");
    assert!(ImageRef::new("ghcr.io/org/img:1@sha256:ab").is_ok());
    assert_eq!(why(ProviderName::new("-x")), "invalid");
    assert_eq!(why(ProviderName::new("a b")), "invalid");
    assert_eq!(why(Label::new("k", "a b")), "invalid");
    assert_eq!(why(Label::new("", "v")), "invalid");
    assert_eq!(Label::new("job", "x-1").unwrap().pair(), "job=x-1");
}

#[test]
fn each_command_is_the_argv_openshell_reads() {
    let name = SandboxName::for_create("box-1").unwrap();
    let label = Label::new("run", "box-1").unwrap();
    let c = Create {
        name: name.clone(),
        image: ImageRef::new("img:1").unwrap(),
        label: Some(label.clone()),
        provider: Some(ProviderName::new("work").unwrap()),
        policy: Some("/home/p.yaml".into()),
        command: s(&["/bin/sh", "-c", "exec \"$@\"", "x", "agent"]),
    };
    assert_eq!(
        create(&c).unwrap(),
        s(&[
            "sandbox",
            "create",
            "--name",
            "box-1",
            "--from",
            "img:1",
            "--label",
            "run=box-1",
            "--detach",
            "--provider",
            "work",
            "--policy",
            "/home/p.yaml",
            "--",
            "/bin/sh",
            "-c",
            "exec \"$@\"",
            "x",
            "agent"
        ])
    );
    let bare = Create { label: None, provider: None, policy: None, command: vec![], ..c };
    assert_eq!(create(&bare).unwrap(), s(&["sandbox", "create", "--name", "box-1", "--from", "img:1", "--detach"]));
    assert_eq!(exec(&name, Some(30), &s(&["node", "-e", "1"])), s(&["sandbox", "exec", "box-1", "--no-login-shell", "--timeout", "30", "--", "node", "-e", "1"]));
    assert_eq!(exec(&name, None, &s(&["true"])), s(&["sandbox", "exec", "box-1", "--no-login-shell", "--", "true"]));
    assert_eq!(upload(&name, "/src", "/workspace", true).unwrap(), s(&["sandbox", "upload", "--no-git-ignore", "box-1", "/src", "/workspace"]));
    assert_eq!(get(&name), s(&["sandbox", "get", "box-1", "--output", "json"]));
    assert_eq!(policy_full(&name), s(&["policy", "get", "box-1", "--full"]));
    assert_eq!(logs(&name, "10m"), s(&["logs", "box-1", "--since", "10m", "--source", "sandbox"]));
    assert_eq!(delete(&name), s(&["sandbox", "delete", "box-1"]));
    assert_eq!(list_labelled(&label), s(&["sandbox", "list", "--selector", "run=box-1", "-o", "json"]));
    assert_eq!(list(), s(&["sandbox", "list", "-o", "json"]));
}

#[test]
fn a_policy_or_upload_path_must_be_absolute_and_stay_put() {
    let name = SandboxName::new("b").unwrap();
    for bad in ["rel", "/a/../b", "/a\nb", ""] {
        assert!(upload(&name, bad, "/w", false).is_err(), "{bad:?}");
        assert!(upload(&name, "/s", bad, false).is_err(), "{bad:?}");
        let c = Create { name: name.clone(), image: ImageRef::new("i").unwrap(), label: None, provider: None, policy: Some(bad.into()), command: vec![] };
        assert!(create(&c).is_err(), "{bad:?}");
    }
}

#[test]
fn a_listing_is_complete_only_when_empty_with_no_next_page() {
    assert!(parse_list(r#"{"sandboxes":[]}"#).unwrap().confirms_none());
    assert!(parse_list(r#"{"sandboxes":[],"next_page_token":""}"#).unwrap().confirms_none());
    assert!(!parse_list(r#"{"sandboxes":[],"next_page_token":"abc"}"#).unwrap().confirms_none());
    assert!(!parse_list(r#"{"sandboxes":[{"name":"a"}]}"#).unwrap().confirms_none());
    assert!(!parse_list("{}").unwrap().confirms_none(), "a missing list proves nothing");
    assert_eq!(parse_list("not json").unwrap_err().code(), "tool_failed");
}

#[test]
fn a_phase_is_found_wherever_the_version_puts_it() {
    let at = |t: &str| phase_of(&serde_json::from_str(t).unwrap());
    assert_eq!(at(r#"{"phase":"Ready"}"#), Some("Ready".into()));
    assert_eq!(at(r#"{"sandbox":{"phase":"Ready"}}"#), Some("Ready".into()));
    assert_eq!(at(r#"{"phase":null,"status":{"phase":"Provisioning"}}"#), Some("Provisioning".into()));
    assert_eq!(at("{}"), None);
}

#[test]
fn policy_denied_counts_only_as_a_whole_word() {
    assert!(policy_denied("error: policy_denied for host"));
    assert!(policy_denied("policy_denied"));
    assert!(policy_denied("x policy_denied_by_x policy_denied."));
    assert!(!policy_denied("policy_denied_by_x"));
    assert!(!policy_denied("my_policy_denied"));
    assert!(!policy_denied("connection reset"));
}

#[test]
fn a_policy_file_is_small_hashed_and_pinned() {
    let dir = std::path::PathBuf::from("/tmp").join(format!("aos-pol-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.yaml");
    std::fs::write(&file, "version: 1\n").unwrap();
    let first = PolicyFile::read(&file).unwrap();
    assert_eq!(first.content, "version: 1\n");
    assert_eq!(first.hash, "09bfcc6a14b83e2192b8673677725c84883ee9cd0c70e45c9ec09daa8f2b2847");
    assert!(first.unchanged_since(&PolicyFile::read(&file).unwrap()).is_ok());
    std::fs::write(&file, "version: 2\n").unwrap();
    assert_eq!(why(first.unchanged_since(&PolicyFile::read(&file).unwrap())), "policy_changed");
    std::fs::write(dir.join("big.yaml"), vec![b'#'; 256 * 1024 + 1]).unwrap();
    assert_eq!(why(PolicyFile::read(&dir.join("big.yaml"))), "invalid");
    assert_eq!(why(PolicyFile::read(&dir)), "invalid");
    std::fs::write(dir.join("max.yaml"), vec![b'#'; 256 * 1024]).unwrap();
    assert!(PolicyFile::read(&dir.join("max.yaml")).is_ok());
    assert!(matches!(PolicyFile::read(&dir.join("missing")), Err(adytum::Error::Io { .. })));
    std::fs::remove_dir_all(&dir).unwrap();
}
