//! The server on localhost: the app's files, who is turned away, pairing
//! over a WebSocket, inputs only from paired devices, forgetting one.
#![allow(clippy::unwrap_used)]

use faderframe_remote::client::{self, Client};
use faderframe_remote::pairing::Role;
use faderframe_remote::protocol::{RemoteInput, Screen, TransportButton};
use faderframe_remote::{Event, Server, ServerConfig};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(3);

fn server() -> Server {
    Server::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            any_network: false,
        },
        Vec::new(),
        Role::Full,
    )
    .unwrap()
}

/// Events until `want` says one is there (all of them returned).
fn events_until(s: &Server, want: impl Fn(&Event) -> bool) -> Vec<Event> {
    let until = Instant::now() + WAIT;
    let mut all = Vec::new();
    while Instant::now() < until {
        let got = s.poll();
        let found = got.iter().any(&want);
        all.extend(got);
        if found {
            return all;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("no such event in {all:?}");
}

fn pair(s: &Server, addr: SocketAddr) -> (Client, String) {
    let mut c = Client::connect(addr).unwrap();
    let code = s.code().unwrap();
    c.send(&json!({"t": "pair", "code": code, "name": "Test Phone"}))
        .unwrap();
    let paired = c.recv_type("paired", WAIT).unwrap().unwrap();
    let token = paired["token"].as_str().unwrap().to_string();
    let welcome = c.recv_type("welcome", WAIT).unwrap().unwrap();
    assert_eq!(welcome["name"], json!("Test Phone"));
    (c, token)
}

#[test]
fn the_app_is_served_and_strangers_are_turned_away() {
    let s = server();
    let addr = s.local_addr();
    let host = addr.to_string();
    let page = client::get(addr, "/", &host).unwrap();
    assert_eq!(page.status, 200);
    assert!(
        page.header("Content-Type")
            .unwrap()
            .starts_with("text/html")
    );
    assert!(
        page.header("Content-Security-Policy")
            .unwrap()
            .contains("default-src 'none'")
    );
    assert!(String::from_utf8_lossy(&page.body).contains("FaderFrame Remote"));
    let js = client::get(addr, "/app.js", &host).unwrap();
    assert_eq!(js.status, 200);
    assert_eq!(js.header("X-Content-Type-Options"), Some("nosniff"));
    assert_eq!(client::get(addr, "/nothing", &host).unwrap().status, 404);
    // A public name pointed here (DNS rebinding).
    assert_eq!(
        client::get(addr, "/", "evil.example.com").unwrap().status,
        421
    );
    assert_eq!(
        client::raw_request(addr, b"POST / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap(),
        405
    );
    assert_eq!(client::raw_request(addr, b"NONSENSE\r\n\r\n").unwrap(), 400);
    // A WebSocket from a page elsewhere.
    assert_eq!(
        Client::connect_as(addr, &host, Some("http://evil.example.com"))
            .unwrap()
            .err(),
        Some(403)
    );
    // From the app's own page.
    assert!(
        Client::connect_as(addr, &host, Some(&format!("http://{host}")))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn a_device_pairs_once_and_comes_back_with_its_token() {
    let s = server();
    let addr = s.local_addr();
    let first_code = s.code().unwrap();
    // Unpaired: nothing it asks for reaches the session.
    let mut stranger = Client::connect(addr).unwrap();
    stranger
        .send(&json!({"t": "transport", "button": "play"}))
        .unwrap();
    let r = stranger.recv_type("refused", WAIT).unwrap().unwrap();
    assert_eq!(r["why"], json!("not_paired"));
    let wrong = if first_code == "000000" {
        "111111"
    } else {
        "000000"
    };
    stranger
        .send(&json!({"t": "pair", "code": wrong, "name": "x"}))
        .unwrap();
    let r = stranger.recv_type("refused", WAIT).unwrap().unwrap();
    assert_eq!(r["why"], json!("wrong_code"));
    assert!(s.poll().is_empty());
    assert_eq!(s.wrong_codes(), 1);

    let (mut phone, token) = pair(&s, addr);
    let events = events_until(&s, |e| matches!(e, Event::Joined { .. }));
    let device = events
        .iter()
        .find_map(|e| match e {
            Event::Paired { device } => Some(device.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(device.name, "Test Phone");
    assert_eq!(device.role, Role::Full);
    assert_ne!(device.token_hash, token);
    // The code paired once.
    assert_ne!(s.code().unwrap(), first_code);
    let client = s
        .clients()
        .into_iter()
        .find(|c| c.device == Some(device.id))
        .unwrap()
        .client;

    phone
        .send(&json!({"t": "view", "screen": "mixer"}))
        .unwrap();
    phone
        .send(&json!({"t": "transport", "button": "play"}))
        .unwrap();
    let events = events_until(&s, |e| matches!(e, Event::Input { .. }));
    assert!(events.contains(&Event::View {
        client,
        screen: Screen::Mixer
    }));
    assert!(events.contains(&Event::Input {
        client,
        input: RemoteInput::Transport {
            button: TransportButton::Play
        }
    }));
    // What the session sends arrives.
    assert!(s.send(client, json!({"t": "notice", "text": "hi"}).to_string()));
    let n = phone.recv_type("notice", WAIT).unwrap().unwrap();
    assert_eq!(n["text"], json!("hi"));
    // Not understood: said so, and the connection stays.
    phone.send_text("{\"t\":\"explode\"}").unwrap();
    assert!(phone.recv_type("notice", WAIT).unwrap().is_some());

    phone.close().unwrap();
    events_until(&s, |e| *e == Event::Left { client });

    // Back with its token.
    let mut again = Client::connect(addr).unwrap();
    again.send(&json!({"t": "hello", "token": token})).unwrap();
    assert!(again.recv_type("welcome", WAIT).unwrap().is_some());
    events_until(
        &s,
        |e| matches!(e, Event::Joined { device: d, .. } if *d == device.id),
    );
    // A token nobody has.
    let mut other = Client::connect(addr).unwrap();
    other
        .send(&json!({"t": "hello", "token": "0".repeat(64)}))
        .unwrap();
    let r = other.recv_type("refused", WAIT).unwrap().unwrap();
    assert_eq!(r["why"], json!("unknown_token"));
}

#[test]
fn a_forgotten_device_is_told_and_disconnected() {
    let s = server();
    let addr = s.local_addr();
    let (mut phone, token) = pair(&s, addr);
    events_until(&s, |e| matches!(e, Event::Joined { .. }));
    s.set_devices(Vec::new());
    let bye = phone.recv_type("bye", WAIT).unwrap().unwrap();
    assert_eq!(bye["why"], json!("forgotten"));
    assert!(phone.recv(WAIT).is_err());
    // Its token no longer opens anything.
    let mut again = Client::connect(addr).unwrap();
    again.send(&json!({"t": "hello", "token": token})).unwrap();
    let r = again.recv_type("refused", WAIT).unwrap().unwrap();
    assert_eq!(r["why"], json!("unknown_token"));
}

#[test]
fn broken_and_oversized_messages_end_the_connection() {
    let s = server();
    let addr = s.local_addr();
    let mut c = Client::connect(addr).unwrap();
    c.send_text(&"x".repeat(64 * 1024)).unwrap();
    assert!(c.recv(WAIT).is_err());
    // An unmasked frame (servers may not take them).
    let mut c = Client::connect(addr).unwrap();
    c.send_raw(&[0x81, 0x02, b'h', b'i']).unwrap();
    assert!(c.recv(WAIT).is_err());
}

#[test]
fn stopping_closes_every_client_and_frees_the_port() {
    let s = server();
    let addr = s.local_addr();
    let (mut phone, _) = pair(&s, addr);
    drop(s);
    assert!(phone.recv(WAIT).is_err());
    // The port can be listened on again at once.
    let again = Server::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: addr.port(),
            any_network: false,
        },
        Vec::new(),
        Role::Viewer,
    )
    .unwrap();
    assert_eq!(again.local_addr(), addr);
    assert_eq!(again.code_role(), Role::Viewer);
}
