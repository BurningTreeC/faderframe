//! A host and clients on this machine's loopback: hello and welcome,
//! messages both ways, a file fetched by its key, wrong codes turned away.
#![allow(clippy::unwrap_used)]

use faderframe_collab::{
    Client, ClientEvent, Fetched, Hello, Host, HostEvent, InviteCode, PROTOCOL, ToHost, ToPeer,
    Welcome,
};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn hello(name: &str) -> Hello {
    Hello {
        protocol: PROTOCOL,
        program: "test".into(),
        name: name.into(),
        resume: None,
    }
}

fn host() -> (Host, InviteCode, String) {
    let code = InviteCode::new().unwrap();
    let h = Host::start(SocketAddr::from(([127, 0, 0, 1], 0)), code.clone()).unwrap();
    let addr = h.local_addr().to_string();
    (h, code, addr)
}

fn host_event(h: &Host) -> HostEvent {
    let t = Instant::now();
    loop {
        if let Some(e) = h.poll() {
            return e;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "no host event");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn client_event(c: &Client) -> ClientEvent {
    let t = Instant::now();
    loop {
        if let Some(e) = c.poll() {
            return e;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "no client event");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn welcome(peer: u32) -> ToPeer {
    ToPeer::Welcome(Box::new(Welcome {
        peer,
        token: 7,
        colour: 0x3366ff,
        snapshot: Some(b"project".to_vec()),
        seq: 0,
        entries: Vec::new(),
        answered: 0,
        participants: Vec::new(),
        sample_rate: 48_000,
        lease: Vec::new(),
    }))
}

#[test]
fn a_peer_says_hello_is_welcomed_and_they_talk() {
    let (mut h, code, addr) = host();
    let c = Client::connect(&addr, code, hello("Ana"));
    assert!(matches!(client_event(&c), ClientEvent::Connected { .. }));
    let HostEvent::Hello { conn, hello, .. } = host_event(&h) else {
        panic!("no hello")
    };
    assert_eq!(hello.name, "Ana");
    h.send(conn, &welcome(1));
    let ClientEvent::Message(ToPeer::Welcome(w)) = client_event(&c) else {
        panic!("no welcome")
    };
    assert_eq!(w.snapshot.as_deref(), Some(&b"project"[..]));
    // A big step crosses in one piece.
    let step: Vec<u8> = (0..1_000_000u32).map(|i| i as u8).collect();
    c.send(&ToHost::Propose {
        local: 1,
        label: "Big".into(),
        step: step.clone(),
    });
    let HostEvent::Message {
        message: ToHost::Propose { step: got, .. },
        ..
    } = host_event(&h)
    else {
        panic!("no proposal")
    };
    assert_eq!(got, step);
    // Presence: only the newest of each waits.
    for i in 0..3u8 {
        h.send_presence(
            conn,
            0,
            faderframe_collab::wire::encode(&ToPeer::Presence {
                peer: 0,
                data: vec![i],
            })
            .unwrap(),
        );
    }
    let ClientEvent::Message(ToPeer::Presence { data, .. }) = client_event(&c) else {
        panic!("no presence")
    };
    assert!(data[0] <= 2);
    // The host leaves: the peer is told why.
    h.stop("the session ended");
    let mut why = None;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        match c.poll() {
            Some(ClientEvent::Message(ToPeer::Bye { reason })) => why = Some(reason),
            Some(ClientEvent::Closed { .. }) => break,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    assert_eq!(why.as_deref(), Some("the session ended"));
}

#[test]
fn a_wrong_code_is_refused() {
    let (h, _, addr) = host();
    let c = Client::connect(&addr, InviteCode::new().unwrap(), hello("Eve"));
    let ClientEvent::Closed { reason } = client_event(&c) else {
        panic!("connected without the code")
    };
    assert!(reason.contains("invite code"), "{reason}");
    assert!(matches!(host_event(&h), HostEvent::Refused { .. }));
}

#[test]
fn a_file_is_fetched_by_its_key_and_resumed() {
    let dir = std::env::temp_dir().join(format!("ff-collab-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("take.wav");
    let bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();
    let (h, code, addr) = host();
    h.offer_media("3000000:abc", &src);
    let dest = dir.join("here").join("take.wav");
    // A part from an earlier try.
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(faderframe_collab::part_of(&dest), &bytes[..700_000]).unwrap();
    let c = Client::connect(&addr, code, hello("Bo"));
    assert!(matches!(client_event(&c), ClientEvent::Connected { .. }));
    let HostEvent::Hello { conn, .. } = host_event(&h) else {
        panic!()
    };
    h.send(conn, &welcome(1));
    assert!(matches!(
        client_event(&c),
        ClientEvent::Message(ToPeer::Welcome(_))
    ));
    c.fetch("3000000:abc", &dest);
    c.fetch("nothing", &dir.join("nothing.wav"));
    let mut arrived = false;
    let mut missing = false;
    let mut first_progress = None;
    while !(arrived && missing) {
        // A file the host does not have: the host's session answers.
        if let Some(HostEvent::WantMedia { conn, key, .. }) = h.poll() {
            assert_eq!(key, "nothing");
            h.missing(conn, &key);
        }
        let Some(event) = c.poll() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        match event {
            ClientEvent::Media(Fetched::Arrived { key, path }) => {
                assert_eq!(key, "3000000:abc");
                assert_eq!(std::fs::read(path).unwrap(), bytes);
                arrived = true;
            }
            ClientEvent::Media(Fetched::Failed { key, .. }) => {
                assert_eq!(key, "nothing");
                missing = true;
            }
            ClientEvent::Media(Fetched::Progress { key, done, .. }) if key != "nothing" => {
                first_progress.get_or_insert(done);
            }
            _ => {}
        }
    }
    assert_eq!(
        first_progress,
        Some(700_000),
        "resumed where the part ended"
    );
}
