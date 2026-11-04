//! The phone and web remote: FaderFrame's transport, mixer, cue mixes,
//! setlist, lyrics and clip launcher on a phone or tablet's browser, over
//! the local network.
//!
//! A small server of its own ([`server`]: HTTP/1.1 for the web app's
//! files, [`ws`] WebSockets for everything else; no TLS — the stance is
//! the local network's: only peers in its address ranges are answered and
//! only requests that name this computer, see [`net`]) is off until it is
//! turned on. A device pairs by the code FaderFrame shows, typed or
//! scanned from a QR code ([`qr`]); from then on it shows its token
//! ([`pairing`]). Roles decide what a device sees and does: everything, a
//! performer's own cue mix, or reading the lyrics and setlist.
//!
//! The session (`faderframe_session::web_remote`) builds a [`State`] a few
//! times a second and sends each client what changed ([`diff`]) as its
//! role sees it; what devices send arrives as [`RemoteInput`]s, which the
//! session turns into actions on its own thread — the path control
//! surfaces take, gestures included. The app itself ([`web`]) is plain
//! HTML, CSS and JavaScript built into the program.

#![forbid(unsafe_code)]

pub mod client;
pub mod diff;
pub mod hash;
pub mod http;
pub mod net;
pub mod pairing;
pub mod protocol;
pub mod qr;
pub mod secret;
pub mod server;
pub mod web;
pub mod ws;

pub use pairing::{PairedDevice, Role};
pub use protocol::{RemoteInput, Screen, State};
pub use server::{ClientId, Event, Server, ServerConfig};
