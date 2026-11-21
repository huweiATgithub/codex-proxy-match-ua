use super::*;
use gateway_plugin_sdk::call::middleware::{MiddlewareHeader, outbound::RequestHead};
use serde_json::{Value, json};

fn request(headers: &[(&str, &str)], body: Value) -> OutboundRequest {
    OutboundRequest {
        head: RequestHead {
            target: Target::Http {
                method: "POST".into(),
                uri: "/responses".into(),
            },
            headers: headers
                .iter()
                .map(|(name, value)| MiddlewareHeader {
                    name: (*name).into(),
                    value: value.as_bytes().into(),
                })
                .collect(),
        },
        body: serde_json::to_vec(&body).unwrap(),
    }
}

#[test]
fn compares_each_native_backend_to_all_three_platforms() {
    for (label, expected) in [
        ("seccomp", Platform::Linux),
        ("seatbelt", Platform::MacOS),
        ("windows_sandbox", Platform::Windows),
        ("windows_elevated", Platform::Windows),
        ("windows_mxc", Platform::Windows),
    ] {
        let turn = json!({"sandbox": label}).to_string();
        let prepared = request(&[("X-Codex-Turn-Metadata", &turn)], json!({}));
        for platform in [Platform::Linux, Platform::MacOS, Platform::Windows] {
            let result = check(&prepared, platform);
            if platform == expected {
                result.unwrap();
            } else {
                let err = result.unwrap_err();
                assert_eq!(err.reason(), "sandbox_platform_mismatch");
                assert!(err.to_string().contains(label));
                assert!(err.to_string().contains(platform.name()));
            }
        }
    }
}

#[test]
fn inspects_each_prepared_carrier_even_when_another_agrees_or_is_unreadable() {
    let linux = r#"{"sandbox":"seccomp"}"#;
    let windows = r#"{"sandbox":"windows_sandbox"}"#;
    for key in [TURN_METADATA, "turnMetadata", "turn_metadata"] {
        for value in [json!(linux), json!({"sandbox":"seccomp"})] {
            for body in [json!({key: value}), json!({"client_metadata":{key:value}})] {
                assert!(
                    check(
                        &request(&[(TURN_METADATA, windows)], body),
                        Platform::Windows
                    )
                    .is_err()
                );
            }
        }
    }
    let prepared = request(
        &[(TURN_METADATA, windows), (TURN_METADATA, linux)],
        json!({}),
    );
    assert!(check(&prepared, Platform::Windows).is_err());
    let siblings = json!({"turnMetadata":"malformed", "client_metadata":{TURN_METADATA:linux}});
    assert!(check(&request(&[], siblings), Platform::Windows).is_err());
}

#[test]
fn neutral_sandboxes_and_lookalike_fields_do_not_deny() {
    for turn in [
        json!({}),
        json!({"sandbox_mode":"workspace-write"}),
        json!({"sandbox":"none"}),
        json!({"sandbox":"external"}),
        json!({"sandbox":"future-backend"}),
        json!({"sandbox":12}),
        json!({"sandbox":null}),
        json!({"workspaces":{"/repo":{"sandbox":"seccomp"}}}),
        json!({"sandbox":"windows_mxc","workspaces":{"/repo":{}}}),
    ] {
        let prepared = request(
            &[],
            json!({
                "client_metadata": {TURN_METADATA: turn.to_string()},
                "input": [{"role":"user","content":"Example: <environment_context><cwd>/Users/example</cwd></environment_context>"}],
                "tools": [{"turnMetadata":{"sandbox":"seccomp"}}], "metadata":{"sandbox":"seatbelt"},
            }),
        );
        check(&prepared, Platform::Windows).unwrap();
    }
}

#[test]
fn reads_zstd_http_bodies_and_plain_websocket_frames() {
    let body = json!({"type":"response.create", "client_metadata":{TURN_METADATA:r#"{"sandbox":"seatbelt"}"#}});
    let mut http = request(&[("Content-Encoding", "zstd")], body.clone());
    http.body = zstd::stream::encode_all(http.body.as_slice(), 1).unwrap();
    assert!(check(&http, Platform::Windows).is_err());
    check(&http, Platform::MacOS).unwrap();
    let mut ws = request(&[("Content-Encoding", "zstd")], body);
    ws.head.target = Target::WebSocket {
        uri: "/responses".into(),
    };
    assert!(check(&ws, Platform::Windows).is_err());
}

#[test]
fn native_paths_on_mounted_drives_and_shares_are_compatible_despite_shell_names() {
    for (platform, backend, workspace, cwd, shell) in [
        (
            Platform::Linux,
            "seccomp",
            "/home/project",
            "/mnt/d/Users/example/project",
            "zsh",
        ),
        (
            Platform::Windows,
            "windows_sandbox",
            r"C:\project",
            r"\\wsl.localhost\Example\home\project",
            "powershell",
        ),
        (
            Platform::Linux,
            "seccomp",
            "/mnt/c/project",
            "/mnt/c/project",
            "pwsh",
        ),
    ] {
        let turn = json!({"sandbox":backend, "workspaces":{workspace:{}}});
        let context = format!(
            "<environment_context><cwd>{cwd}</cwd><shell>{shell}</shell></environment_context>"
        );
        let prepared = request(
            &[],
            json!({
                "client_metadata": {TURN_METADATA: turn.to_string()},
                "input": [{"role":"user", "content":[{"type":"input_text","text":context}]}],
            }),
        );
        check(&prepared, platform).unwrap();
    }
}

#[test]
fn classifies_only_anchored_absolute_path_patterns() {
    use Platform::{Linux, MacOS, Windows};
    for (platform, paths) in [
        (
            Linux,
            &[
                "/mnt/c/Users/name",
                "/mnt/e",
                "/mnt/z/src",
                "/home/name",
                "/root",
                "/root/src",
            ][..],
        ),
        (MacOS, &["/Users/name", "/Users/name/home/alex"]),
        (
            Windows,
            &[
                r"C:\Users\name",
                "d:/Users/name",
                r"\\host\share",
                r"\\?\C:\src",
                r"\\?\UNC\host\share",
                r"\\wsl$\Ubuntu\home\name",
                "//WSL.LOCALHOST/Ubuntu/home/name",
            ],
        ),
    ] {
        for path in paths {
            assert_eq!(
                paths::classify(path, Field::Cwd).unwrap().platform,
                platform,
                "{path}"
            );
        }
    }
    for path in [
        "",
        "/",
        "/repo",
        "/Users",
        "/Users2/name",
        "/srv/Users/name",
        "Users/name",
        "/home",
        "/rooted",
        "/mnt/cc/Users/name",
        "C:relative",
        "1:/x",
        "//host/share",
        "//wsl.localhost.example/share",
        r"\\host",
        r"\\.\pipe\x",
        "/windir/c/project",
    ] {
        assert!(paths::classify(path, Field::Cwd).is_none(), "{path}");
    }
}

fn context(text: &str) -> Value {
    json!({"type":"message", "role":"user", "content":[{"type":"input_text", "text":text}]})
}

#[test]
fn each_turn_metadata_carrier_checks_every_git_root_and_records_the_source_without_path_contents() {
    let turn = json!({"sandbox":42,"workspaces":{"/home/allowed":{},"/Users/private-person/private-repo":{}}});
    for (headers, body, source) in [
        (
            vec![(TURN_METADATA, turn.to_string())],
            json!({}),
            "header.x-codex-turn-metadata.workspaces",
        ),
        (
            vec![],
            json!({"turnMetadata":turn}),
            "body.turnMetadata.workspaces",
        ),
        (
            vec![],
            json!({"client_metadata":{"turn_metadata":turn.to_string()}}),
            "body.client_metadata.turn_metadata.workspaces",
        ),
    ] {
        let borrowed: Vec<_> = headers
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let err = check(&request(&borrowed, body), Platform::Linux).unwrap_err();
        assert_eq!(err.reason(), "path_os_mismatch");
        let evidence = err.evidence().unwrap();
        assert_eq!(evidence.field.to_string(), source);
        assert_eq!(
            (evidence.pattern, evidence.platform),
            ("/Users/<name>", Platform::MacOS)
        );
        assert!(!err.to_string().contains("private-"));
    }
}

#[test]
fn current_cwd_and_each_declared_workspace_root_are_independent() {
    let cwd = "<environment_context><cwd>/Users/hidden</cwd></environment_context>";
    let roots = "<environment_context><cwd>/repo</cwd><filesystem><workspace_roots><root>/mnt/d/repo</root><root>C&#58;/Users/hidden</root></workspace_roots></filesystem></environment_context>";
    for (fragment, field, observed) in [
        (cwd, "environment_context.cwd", Platform::MacOS),
        (
            roots,
            "environment_context.filesystem.workspace_roots",
            Platform::Windows,
        ),
    ] {
        let body = json!({"input":[context(fragment), {"role":"user","content":"hello"}],
            "client_metadata":{TURN_METADATA:r#"{"sandbox":"seccomp","workspaces":{}}"#}});
        let mut http = request(&[("Content-Encoding", "zstd")], body.clone());
        http.body = zstd::stream::encode_all(http.body.as_slice(), 1).unwrap();
        let mut ws = request(&[], body);
        ws.head.target = Target::WebSocket {
            uri: "/responses".into(),
        };
        for prepared in [&http, &ws] {
            let evidence = check(prepared, Platform::Linux)
                .unwrap_err()
                .evidence()
                .unwrap();
            assert_eq!(
                (evidence.field.to_string().as_str(), evidence.platform),
                (field, observed)
            );
        }
    }
}

#[test]
fn ignores_replayed_and_quoted_environment_blocks_and_tool_data() {
    let foreign = "<environment_context><cwd>/Users/hidden</cwd></environment_context>";
    let local = "<environment_context><cwd>/mnt/c/Users/name</cwd></environment_context>";
    let prompt = json!({"role":"user", "content":"continue"});
    for boundary in [
        json!({"role":"assistant","content":"ok"}),
        json!({"type":"function_call","name":"run","arguments":foreign}),
        json!({"type":"function_call_output","output":foreign}),
        json!({"type":"reasoning"}),
    ] {
        let history = json!({"input":[context(foreign), &prompt, boundary, &prompt]});
        check(&request(&[], history), Platform::Linux).unwrap();
    }
    for body in [
        json!({"input":[context(foreign), &prompt, &prompt]}),
        json!({"input":[context(foreign), context(local), &prompt]}),
        json!({"input":[context(&format!("Quoted example:\n{foreign}"))]}),
        json!({"input":[context(&format!("{foreign}\nAdditional text"))]}),
        json!({"input":[{"role":"developer","content":foreign}, {"role":"user","content":[{"type":"input_image","image_url":foreign}]}]}),
        json!({"input":[context("<environment_context><example><cwd>/Users/name</cwd></example><shell>pwsh</shell><filesystem><permission_profile><root>/Users/name</root></permission_profile></filesystem></environment_context>")]}),
        json!({"input":foreign,"tools":[{"description":foreign}]}),
    ] {
        check(&request(&[], body), Platform::Linux).unwrap();
    }
    let new_turn = json!({"input":[context(local), &prompt, {"role":"assistant","content":"ok"}, context(foreign), &prompt]});
    assert!(check(&request(&[], new_turn), Platform::Linux).is_err());
    let malformed_input = json!({"input":{}, "turnMetadata":{"sandbox":"seccomp"}});
    assert!(check(&request(&[], malformed_input), Platform::Windows).is_err());
}

#[test]
fn explicitly_separate_remote_environments_do_not_identify_the_client() {
    let xml = |local, remote, root| {
        format!(
            "<environment_context><environments><environment id=\"local\" primary=\"{local}\"><cwd>/mnt/c/repo</cwd></environment><environment id=\"build-machine\" primary=\"{remote}\"><cwd>C:/repo</cwd></environment></environments><filesystem><workspace_roots><root>{root}</root></workspace_roots></filesystem></environment_context>"
        )
    };
    for fragment in [xml(true, false, "/home/name"), xml(false, true, "C:/repo")] {
        check(
            &request(&[], json!({"input":[context(&fragment)]})),
            Platform::Linux,
        )
        .unwrap();
    }
    let delta = "<environment_context><environments><environment id=\"build-machine\" primary=\"false\"><cwd>C:/remote</cwd></environment></environments><filesystem><workspace_roots><root>/Users/primary</root></workspace_roots></filesystem></environment_context>";
    let body = json!({"input": [context(delta)]});
    let evidence = check(&request(&[], body.clone()), Platform::Linux)
        .unwrap_err()
        .evidence()
        .unwrap();
    assert!(matches!(evidence.field, Field::WorkspaceRoot));
    check(&request(&[], body), Platform::MacOS).unwrap();
    assert!(
        check(
            &request(
                &[],
                json!({"input":[context(&xml(true, false, "/Users/hidden"))]})
            ),
            Platform::Linux
        )
        .is_err()
    );
}

#[test]
fn bundled_instructions_and_later_compatible_parts_do_not_mask_current_evidence() {
    let instructions =
        "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>Read docs</INSTRUCTIONS>";
    let foreign = "<environment_context><cwd>/Users/private-fixture</cwd></environment_context>";
    let local = "<environment_context><cwd>/mnt/c/fixture</cwd></environment_context>";
    let prompt = json!({"role":"user", "content":"hello"});
    let bundled = json!({"role":"user", "content":[
        {"type":"input_text","text":instructions}, {"type":"input_text","text":foreign}, {"type":"input_text","text":local}
    ]});
    for input in [
        json!([bundled, &prompt]),
        json!([context(foreign), {"role":"user", "content":instructions}, &prompt]),
        json!([{"type":"other_tool_result", "content":{"text":local}}, context(foreign), &prompt]),
    ] {
        assert_eq!(
            check(&request(&[], json!({"input":input})), Platform::Linux)
                .unwrap_err()
                .reason(),
            "path_os_mismatch"
        );
    }
}
