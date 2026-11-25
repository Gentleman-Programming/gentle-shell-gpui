//! Wire-format tests for signing in, API keys and packages on the Shell's
//! side (`login_*`, `set_api_key`, `remove_credentials`, `*_package(s)`):
//! the requests are capability-gated and exact, a secret never shows in a
//! request's `Debug`, and the answers and events decode as documented.

use pi_rpc::desktop_rpc::{
    ApiKeySaved, Capabilities, CredentialsRemoved, DesktopEvent, DesktopMethod, DesktopRecord,
    DesktopRequest, DesktopResponse, DesktopRpcError, DesktopSession, InstalledPackage, LoginKind,
    LoginOption, LoginPromptKind, PROMPT_COMMAND, PackageScope, SCHEMA, WIDGET_KEY, decode_payload,
};
use serde_json::{Value, json};

const ACCOUNT_METHODS: [&str; 8] = [
    "login_start",
    "login_complete",
    "login_cancel",
    "set_api_key",
    "remove_credentials",
    "list_packages",
    "install_package",
    "remove_package",
];

fn response(data: Value) -> DesktopResponse {
    let record = json!({
        "schema": SCHEMA, "kind": "response", "requestId": "desktop-1", "ok": true, "data": data,
    });
    match decode_payload(&record.to_string()).unwrap() {
        DesktopRecord::Response(response) => response,
        other => panic!("not a response: {other:?}"),
    }
}

fn event(name: &str, data: Value) -> DesktopEvent {
    let record =
        json!({ "schema": SCHEMA, "kind": "event", "seq": 7, "event": name, "data": data });
    match decode_payload(&record.to_string()).unwrap() {
        DesktopRecord::Event(event) => event,
        other => panic!("not an event: {other:?}"),
    }
}

fn capabilities(methods: &[&str]) -> Capabilities {
    response(json!({
        "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
        "methods": methods,
        "events": ["login_prompt", "login_finished", "package_progress", "package_finished"],
    }))
    .capabilities()
    .unwrap()
}

fn params(request: &DesktopRequest) -> Value {
    let payload: Value = serde_json::from_str(request.payload()).unwrap();
    assert_eq!(payload["schema"], SCHEMA);
    payload["params"].clone()
}

#[test]
fn every_request_is_exact_and_only_for_a_shell_that_lists_it() {
    let offered = capabilities(&ACCOUNT_METHODS);
    let cases: Vec<(DesktopRequest, &str, Value)> = vec![
        (
            DesktopRequest::login_start("r1", &offered, "anthropic").unwrap(),
            "login_start",
            json!({ "provider": "anthropic" }),
        ),
        (
            DesktopRequest::login_complete("r2", &offered, "l1", "http://localhost:1455/cb?code=x")
                .unwrap(),
            "login_complete",
            json!({ "loginId": "l1", "value": "http://localhost:1455/cb?code=x" }),
        ),
        (
            DesktopRequest::login_cancel("r3", &offered, "l1").unwrap(),
            "login_cancel",
            json!({ "loginId": "l1" }),
        ),
        (
            DesktopRequest::set_api_key("r4", &offered, "groq", "gsk-1").unwrap(),
            "set_api_key",
            json!({ "provider": "groq", "key": "gsk-1" }),
        ),
        (
            DesktopRequest::remove_credentials("r5", &offered, "groq").unwrap(),
            "remove_credentials",
            json!({ "provider": "groq" }),
        ),
        (
            DesktopRequest::list_packages("r6", &offered).unwrap(),
            "list_packages",
            json!({}),
        ),
        (
            DesktopRequest::install_package("r7", &offered, "npm:pi-btw", None).unwrap(),
            "install_package",
            json!({ "source": "npm:pi-btw" }),
        ),
        (
            DesktopRequest::remove_package(
                "r8",
                &offered,
                "npm:pi-btw",
                Some(PackageScope::Project),
            )
            .unwrap(),
            "remove_package",
            json!({ "source": "npm:pi-btw", "scope": "project" }),
        ),
    ];
    for (request, method, expected) in cases {
        assert_eq!(request.method().as_str(), method);
        assert_eq!(params(&request), expected, "{method}");
    }

    // An older Shell: nothing is built.
    let older = capabilities(&["capabilities", "get_auth_status"]);
    assert_eq!(
        DesktopRequest::login_start("r9", &older, "anthropic"),
        Err(DesktopRpcError::NotAdvertised(DesktopMethod::LoginStart))
    );
    assert_eq!(
        DesktopRequest::install_package("r9", &older, "npm:a", None),
        Err(DesktopRpcError::NotAdvertised(
            DesktopMethod::InstallPackage
        ))
    );
    // Only their typed constructors build them.
    for name in ACCOUNT_METHODS {
        let method = DesktopMethod::parse(name).unwrap();
        assert_eq!(
            DesktopRequest::new("r10", method, json!({})),
            Err(DesktopRpcError::TypedParamsRequired(method))
        );
    }
}

#[test]
fn empty_values_are_refused_without_quoting_a_secret() {
    let offered = capabilities(&ACCOUNT_METHODS);
    assert_eq!(
        DesktopRequest::login_start("r1", &offered, ""),
        Err(DesktopRpcError::InvalidProvider)
    );
    assert!(DesktopRequest::login_complete("r1", &offered, "", "code").is_err());
    assert!(DesktopRequest::login_complete("r1", &offered, "l1", "").is_err());
    assert!(DesktopRequest::set_api_key("r1", &offered, "groq", "").is_err());
    assert!(DesktopRequest::install_package("r1", &offered, "  ", None).is_err());
    // A key or a pasted value never shows in the request's Debug.
    let key = DesktopRequest::set_api_key("r1", &offered, "groq", "gsk-secret").unwrap();
    assert!(key.payload().contains("gsk-secret"));
    assert!(!format!("{key:?}").contains("gsk-secret"));
    let code = DesktopRequest::login_complete("r1", &offered, "l1", "code-secret").unwrap();
    assert!(!format!("{code:?}").contains("code-secret"));
    let start = DesktopRequest::login_start("r1", &offered, "anthropic").unwrap();
    assert!(format!("{start:?}").contains("anthropic"));
}

#[test]
fn the_session_issues_them_gated_and_one_at_a_time() {
    let offered = capabilities(&ACCOUNT_METHODS);
    let mut session = DesktopSession::new();
    let request = session.issue_login_start(&offered, "anthropic").unwrap();
    assert_eq!(request.request_id(), "desktop-1");
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::LoginStart))
    );
    assert!(session.issue_list_packages(&offered).is_err());
    session.abandon();
    let job = session
        .issue_package_job(&offered, false, "npm:a", None)
        .unwrap();
    assert_eq!(job.method(), DesktopMethod::RemovePackage);
    session.abandon();

    let older = capabilities(&["capabilities"]);
    assert_eq!(
        session.issue_set_api_key(&older, "groq", "k"),
        Err(DesktopRpcError::NotAdvertised(DesktopMethod::SetApiKey))
    );
    assert_eq!(session.pending(), None);
}

#[test]
fn answers_decode() {
    assert_eq!(
        response(json!({ "loginId": "l1" }))
            .login_started()
            .unwrap()
            .login_id,
        "l1"
    );
    assert!(response(json!({ "loginId": "" })).login_started().is_err());
    assert!(response(json!({})).login_acknowledged().is_ok());
    assert!(response(json!("ok")).login_acknowledged().is_err());
    assert_eq!(
        response(json!({ "saved": true })).api_key_saved().unwrap(),
        ApiKeySaved { saved: true }
    );
    assert_eq!(
        response(json!({ "removed": false }))
            .credentials_removed()
            .unwrap(),
        CredentialsRemoved { removed: false }
    );
    let list = response(json!({ "packages": [
        { "source": "npm:pi-btw", "version": "1.2.0", "scope": "user" },
        { "source": "git:github.com/a/b", "scope": "project" }
    ]}))
    .packages()
    .unwrap();
    assert_eq!(
        list.packages,
        vec![
            InstalledPackage {
                source: "npm:pi-btw".into(),
                version: Some("1.2.0".into()),
                scope: PackageScope::User,
            },
            InstalledPackage {
                source: "git:github.com/a/b".into(),
                version: None,
                scope: PackageScope::Project,
            },
        ]
    );
    assert_eq!(
        response(json!({ "jobId": "j1" }))
            .package_job()
            .unwrap()
            .job_id,
        "j1"
    );
    assert!(response(json!({ "jobId": "" })).package_job().is_err());
}

#[test]
fn login_prompts_decode_and_refuse_what_cannot_be_shown() {
    let open = event(
        "login_prompt",
        json!({ "loginId": "l1", "kind": "open_url", "url": "https://auth.example/authorize?x=1",
                "message": "Sign in", "redirectPrefix": "http://localhost:1455/" }),
    )
    .login_prompt()
    .unwrap();
    assert_eq!(open.kind, LoginPromptKind::OpenUrl);
    assert_eq!(
        open.redirect_prefix.as_deref(),
        Some("http://localhost:1455/")
    );

    let device = event(
        "login_prompt",
        json!({ "loginId": "l1", "kind": "device_code", "url": "https://github.com/login/device", "userCode": "ABCD-1234" }),
    )
    .login_prompt()
    .unwrap();
    assert_eq!(device.user_code.as_deref(), Some("ABCD-1234"));

    let choice = event(
        "login_prompt",
        json!({ "loginId": "l1", "kind": "input", "message": "How?", "options": [
            { "id": "browser", "label": "Browser" }, { "id": "device", "label": "Device code" }
        ]}),
    )
    .login_prompt()
    .unwrap();
    assert_eq!(
        choice.options[1],
        LoginOption {
            id: "device".into(),
            label: "Device code".into()
        }
    );
    assert!(!choice.secret);
    let secret = event(
        "login_prompt",
        json!({ "loginId": "l1", "kind": "input", "placeholder": "code", "secret": true }),
    )
    .login_prompt()
    .unwrap();
    assert!(secret.secret && secret.options.is_empty());

    for odd in [
        // A page the app's browser would load: only the web.
        json!({ "loginId": "l1", "kind": "open_url", "url": "file:///etc/passwd" }),
        json!({ "loginId": "l1", "kind": "open_url" }),
        json!({ "loginId": "l1", "kind": "device_code", "url": "https://x" }),
        json!({ "loginId": "l1", "kind": "progress", "redirectPrefix": "javascript:x" }),
        json!({ "loginId": "", "kind": "progress", "message": "…" }),
        json!({ "loginId": "l1", "kind": "teleport" }),
    ] {
        assert!(
            event("login_prompt", odd.clone()).login_prompt().is_err(),
            "{odd}"
        );
    }
    // Another event's data is never read as a prompt.
    assert!(
        event(
            "login_finished",
            json!({ "loginId": "l1", "kind": "progress" })
        )
        .login_prompt()
        .is_err()
    );

    let finished = event(
        "login_finished",
        json!({ "loginId": "l1", "ok": false, "error": "Cancelled" }),
    )
    .login_finished()
    .unwrap();
    assert!(!finished.ok);
    assert_eq!(finished.error.as_deref(), Some("Cancelled"));
}

#[test]
fn package_events_decode() {
    let line = event(
        "package_progress",
        json!({ "jobId": "j1", "line": "added 3 packages" }),
    )
    .package_progress()
    .unwrap();
    assert_eq!(
        (line.job_id.as_str(), line.line.as_str()),
        ("j1", "added 3 packages")
    );
    let done = event("package_finished", json!({ "jobId": "j1", "ok": true }))
        .package_finished()
        .unwrap();
    assert!(done.ok && done.error.is_none());
    assert!(
        event("package_finished", json!({ "jobId": "", "ok": true }))
            .package_finished()
            .is_err()
    );
}

#[test]
fn login_kinds_are_read_per_provider_and_unknown_ones_skipped() {
    let status = response(json!({ "providers": [
        { "provider": "anthropic", "displayName": "Anthropic", "configured": false, "type": null,
          "source": null, "loginKinds": ["oauth", "api_key", "passkey"] },
        { "provider": "groq", "displayName": "Groq", "configured": false, "type": null, "source": null }
    ], "truncated": false }))
    .auth_status()
    .unwrap();
    assert_eq!(
        status.providers[0].login_kinds,
        [LoginKind::OAuth, LoginKind::ApiKey]
    );
    // An older Shell lists none.
    assert!(status.providers[1].login_kinds.is_empty());
}
