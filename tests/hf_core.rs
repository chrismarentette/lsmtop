//! M8 core units — pure ports of the HF-CLI helpers.
use lsmtop::hf::*;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&p).ok();
    p
}

#[test]
fn token_resolution_order() {
    let dir = tmp("tok");
    let f = dir.join("token");
    std::fs::write(&f, " filetok \n").unwrap();
    // config wins
    assert_eq!(hf_token_of(Some(" cfgtok "), Some("envtok"), Some(&f)), "cfgtok");
    // env next
    assert_eq!(hf_token_of(Some("  "), Some("envtok"), Some(&f)), "envtok");
    // hub file last
    assert_eq!(hf_token_of(None, None, Some(&f)), "filetok");
    // nothing
    assert_eq!(hf_token_of(None, None, None), "");
    assert_eq!(hf_token_of(None, None, Some(&dir.join("absent"))), "");
}

#[test]
fn cli_lookup_own_dir_then_path() {
    let dir = tmp("cli");
    let fake = dir.join("hf");
    std::fs::write(&fake, "#!/bin/sh\ntrue\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(hf_cli_path_from(Some(&dir), None).as_deref(), Some(fake.to_str().unwrap()));
    // PATH order: hf before huggingface-cli
    let p2 = tmp("cli2");
    for n in ["huggingface-cli", "hf"] {
        let f = p2.join(n);
        std::fs::write(&f, "#!/bin/sh\ntrue\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(hf_cli_path_from(None, Some(&p2.to_string_lossy())).unwrap().ends_with("/hf"));
    // absent everywhere -> None (bark path)
    assert_eq!(hf_cli_path_from(None, Some("")), None);
}

#[test]
fn parse_ref_matrix() {
    type Case = (&'static str, Option<(&'static str, Option<&'static str>)>);
    let cases: Vec<Case> = vec![
        ("https://huggingface.co/org/repo", Some(("org/repo", None))),
        ("https://hf.co/org/repo/", Some(("org/repo", None))),
        ("http://huggingface.co/models/org/repo", Some(("org/repo", None))),
        ("https://huggingface.co/datasets/org/repo", Some(("org/repo", None))),
        (
            "https://huggingface.co/org/repo/resolve/main/q4/model.gguf?download=true",
            Some(("org/repo", Some("q4/model.gguf"))),
        ),
        ("org/repo", Some(("org/repo", None))),
        ("org/repo/model.gguf", Some(("org/repo", Some("model.gguf")))),
        ("  https://huggingface.co/org/repo#readme", Some(("org/repo", None))),
        ("", None),
        ("org", None),
        ("models/org", None),
        ("https://huggingface.co", None),
    ];
    for (input, want) in cases {
        let got = parse_hf_ref(input);
        let want = want.map(|(r, f)| (r.to_string(), f.map(str::to_string)));
        assert_eq!(got, want, "input {input:?}");
    }
}

#[test]
fn target_dir_sanitizes_and_avoids_org_collision() {
    let base = tmp("models");
    let d1 = hf_target_dir(base.to_str().unwrap(), "OrgA/Model-GGUF");
    assert_eq!(d1, base.join("Model-GGUF"));
    std::fs::create_dir_all(&d1).unwrap();
    std::fs::write(d1.join(".lsmtop-hf-repo"), "OrgA/Model-GGUF").unwrap();
    // same repo again -> same dir
    assert_eq!(hf_target_dir(base.to_str().unwrap(), "OrgA/Model-GGUF"), d1);
    // different org reusing the name -> org-prefixed sibling, both survive
    let d2 = hf_target_dir(base.to_str().unwrap(), "OrgB/Model-GGUF");
    assert_eq!(d2, base.join("OrgB-Model-GGUF"));
    // hostile chars
    let d3 = hf_target_dir(base.to_str().unwrap(), "org/../../evil!!name");
    assert!(!d3.to_string_lossy().contains(".."));
    assert_eq!(d3.file_name().unwrap(), std::ffi::OsStr::new("evil--name"));
}

#[test]
fn tree_parse_keeps_gguf_case_insensitive() {
    let v: serde_json::Value = serde_json::from_str(
        r#"[{"path":"a/model.GGUF","size":100},
            {"path":"b/x.txt","size":9},
            {"path":"top.gguf","size":"77"},
            {"path":"nosize.gguf"}]"#,
    )
    .unwrap();
    let files = parse_gguf_tree(&v);
    assert_eq!(files.len(), 3, ".gguf kept case-insensitively");
    assert_eq!(files[0].path, "a/model.GGUF");
    assert_eq!(files[1].path, "top.gguf");
    assert_eq!(files[1].size, 77, "numeric string parses as int()");
    assert_eq!(files[2].path, "nosize.gguf");
    assert_eq!(files[2].size, 0);
}

#[test]
fn tree_url_and_status_hints() {
    assert_eq!(
        repo_tree_url("org/repo"),
        "https://huggingface.co/api/models/org/repo/tree/main?recursive=true"
    );
    assert!(tree_status_hint(401).unwrap().contains("token"));
    assert!(tree_status_hint(404).unwrap().contains("404"));
    assert_eq!(tree_status_hint(200), None);
}

#[test]
fn download_argv_never_contains_token() {
    let files = vec!["m-q4.gguf".to_string(), "m-q8.gguf".to_string()];
    let cmd = build_download_cmd("/usr/bin/hf", "org/repo", &files, Path::new("/models/repo"));
    assert_eq!(
        cmd,
        vec![
            "/usr/bin/hf",
            "download",
            "org/repo",
            "m-q4.gguf",
            "m-q8.gguf",
            "--local-dir",
            "/models/repo"
        ]
    );
    assert!(!cmd.join(" ").contains("SECRET")); // token lives in env only
    assert_eq!(
        download_log_path(Path::new("/d"), "org/repo"),
        PathBuf::from("/d/org_repo.log")
    );
}

#[test]
fn spawn_detached_writes_log_without_token_in_visible_cmd() {
    let dir = tmp("spawn");
    let cli = dir.join("fake-hf.sh");
    std::fs::write(
        &cli,
        "#!/bin/sh\necho \"got: $@ env:${HF_TOKEN:-none}\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = dir.join("org_repo.log");
    let cmd = build_download_cmd(cli.to_str().unwrap(), "org/repo", &["f.gguf".into()], &dir);
    let mut child = spawn_download(&cmd, &dir, &log, Some("SECTOK")).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("got: download org/repo f.gguf --local-dir"), "{text}");
    assert!(text.contains("env:SECTOK"), "token passed via env");
    assert!(!cmd.join(" ").contains("SECTOK"));
}

#[test]
fn file_selection_numbers_all_and_names() {
    let mut l = BTreeMap::new();
    l.insert(1, "a/x.gguf".to_string());
    l.insert(2, "b/y.gguf".to_string());
    l.insert(3, "c/z.gguf".to_string());
    assert_eq!(file_selection(&l, "1,3", false), vec!["a/x.gguf", "c/z.gguf"]);
    assert_eq!(file_selection(&l, "ALL", false).len(), 3);
    assert_eq!(file_selection(&l, "", true).len(), 3);
    // unknown number drops, literal name passes through
    assert_eq!(file_selection(&l, "9, custom.gguf", false), vec!["custom.gguf"]);
}

#[test]
fn absent_cli_bark_names_the_installer() {
    let b = install_bark();
    assert!(b.contains("pip install -U 'huggingface_hub'"));
    assert!(b.contains("huggingface-cli"));
}

#[test]
#[ignore = "live network: run with --ignored when validating the HF API path"]
fn live_tree_api_roundtrip() {
    // small public repo with GGUFs
    let url = repo_tree_url("ggml-org/models");
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("lsmtop")
        .build()
        .unwrap()
        .get(&url)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().unwrap();
    let files = parse_gguf_tree(&v);
    assert!(!files.is_empty(), "real repo must list GGUFs");
    assert!(files.iter().all(|f| f.path.to_lowercase().ends_with(".gguf")));
}
