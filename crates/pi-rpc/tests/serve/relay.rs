//! Reaching a server through a relay (`gentle-shell relay`): a fake relay
//! in front of the TLS front ([`super::TlsFront`]) in front of the fake
//! server, so a request crosses the relay's handshake, then TLS pinned end
//! to end.

use super::*;

/// A relay (`gentle-shell relay`, its client side only) splicing every
/// client to the server listening on `to`, or saying the server is offline.
struct FakeRelay {
    url: String,
    /// Every client's hello line.
    hellos: Arc<Mutex<Vec<Value>>>,
}

impl FakeRelay {
    fn start(to: u16, offline: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("gsr://{}", listener.local_addr().unwrap());
        let hellos = Arc::new(Mutex::new(Vec::new()));
        let kept = hellos.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut client) = stream else { return };
                let kept = kept.clone();
                std::thread::spawn(move || {
                    let mut line = Vec::new();
                    let mut byte = [0u8; 1];
                    while client.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {
                        line.push(byte[0]);
                    }
                    kept.lock()
                        .unwrap()
                        .push(serde_json::from_slice(&line).unwrap_or_default());
                    if offline {
                        let _ = client.write_all(
                            b"{\"t\":\"error\",\"code\":\"offline\",\"message\":\"That server is not connected to this relay\"}\n",
                        );
                        return;
                    }
                    let Ok(server) = TcpStream::connect(("127.0.0.1", to)) else {
                        return;
                    };
                    client.write_all(b"{\"t\":\"ok\"}\n").unwrap();
                    let (mut up_from, mut up_to) =
                        (client.try_clone().unwrap(), server.try_clone().unwrap());
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut up_from, &mut up_to);
                        let _ = up_to.shutdown(std::net::Shutdown::Write);
                    });
                    let (mut down_from, mut down_to) = (server, client);
                    let _ = std::io::copy(&mut down_from, &mut down_to);
                    let _ = down_to.shutdown(std::net::Shutdown::Write);
                });
            }
        });
        Self { url, hellos }
    }
}

#[test]
fn a_session_through_the_relay_reads_and_resumes_like_a_direct_one() {
    use pi_rpc::serve::Trust;
    let fake = Fake::start();
    let front = TlsFront::start(&fake, "a");
    let relay = FakeRelay::start(front.port, false);
    let trust = Trust::pinned(&front.fingerprint, Some(&relay.url));
    let client = ServeClient::with_trust(&relay.url, Some(TOKEN.into()), trust.clone());
    let (process, rx) = PiProcess::serve(client.clone(), "/srv/project", None, None).unwrap();
    assert_eq!(process.server_session(), Some("ses_1"));
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    let mut stderr = Vec::new();
    let state = pollster::block_on(process.request(json!({"type": "get_state"}))).unwrap();
    assert_eq!(state.data["ok"], "get_state");
    fake.emit(json!({"type": "agent_start"}));
    let Incoming::Event(event) = next(&rx, &mut stderr) else {
        panic!("expected the event")
    };
    assert_eq!(event["type"], "agent_start");

    // A dropped stream resumes after the last record, through the relay.
    let seen = fake.state.lock().unwrap().records.len();
    fake.drop_streams();
    fake.emit(json!({"type": "agent_end"}));
    let Incoming::Event(event) = next(&rx, &mut stderr) else {
        panic!("expected the replayed event")
    };
    assert_eq!(event["type"], "agent_end");
    assert!(
        fake.log()
            .contains(&format!("GET /api/sessions/ses_1/events?after={seen}"))
    );

    // The app quits; the next launch reattaches through the relay.
    assert!(process.detach());
    drop(process);
    let (process, _rx) =
        PiProcess::serve(client, "/srv/project", Some("ses_1"), Some("pi-1")).unwrap();
    assert_eq!(process.server_session(), Some("ses_1"));
    process.detach();

    // Every connection named the server by its certificate.
    let hellos = relay.hellos.lock().unwrap().clone();
    assert!(hellos.len() >= 4, "{hellos:?}");
    for hello in hellos {
        assert_eq!(
            hello,
            json!({"t": "gsr/client", "v": 1, "id": front.fingerprint})
        );
    }
}

#[test]
fn a_connection_code_reaches_its_server_through_the_relay() {
    let fake = Fake::start();
    let front = TlsFront::start(&fake, "a");
    let relay = FakeRelay::start(front.port, false);
    // Its addresses don't answer here (another network): the relay does.
    let code = ConnectionCode {
        urls: vec![gone().replace("http://", "https://")],
        fingerprint: Some(front.fingerprint.clone()),
        relay: Some(relay.url.clone()),
        ..code_for(&fake, SERVER_ID, &[])
    };
    let url = pi_rpc::serve::locate(&code).unwrap();
    assert_eq!(url, relay.url);
    let paired = pi_rpc::serve::pair_code(&code, &url, "test").unwrap();
    assert_eq!(paired.token, TOKEN);
    assert_eq!(
        paired.trust,
        pi_rpc::serve::Trust::pinned(&front.fingerprint, Some(&relay.url))
    );
    // The code's certificate is pinned as it is: no proof needed.
    assert!(!fake.log().iter().any(|l| l.contains("proof")));

    // A relay the server isn't connected to says so; the request may be
    // sent elsewhere.
    let offline = FakeRelay::start(front.port, true);
    let trust = pi_rpc::serve::Trust::pinned(&front.fingerprint, Some(&offline.url));
    let moved = Arc::new(Mutex::new(Vec::new()));
    let asked = moved.clone();
    let error = ServeClient::with_trust(&offline.url, Some(TOKEN.into()), trust)
        .relocating(Arc::new(move |failed: &str| {
            asked.lock().unwrap().push(failed.to_string());
            None
        }))
        .server()
        .unwrap_err();
    assert!(
        error.message.contains("isn't connected to it right now"),
        "{}",
        error.message
    );
    assert_eq!(*moved.lock().unwrap(), std::slice::from_ref(&offline.url));
}
