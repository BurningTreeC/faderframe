//! One established connection: a thread that writes, a thread that reads.
//!
//! What a connection sends waits in its outbox in four queues, taken in
//! this order: edits and other messages that must arrive in order, then
//! the newest presence of each participant (older ones are dropped: only
//! where someone is now matters), then the host's mix (a few packets at
//! most; a slow link drops the oldest), then a chunk of a media file (the
//! files in turn). A connection with nothing to say sends a ping every
//! [`PING_EVERY`]; one that hears nothing for [`SILENCE_LIMIT`] is gone.

use crate::media::{Sending, Work};
use crate::secure::{Channel, ChannelReader, ChannelWriter};
use crate::wire::MediaPart;
use std::collections::{BTreeMap, VecDeque};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// A connection that has sent nothing for this long sends a ping.
pub const PING_EVERY: Duration = Duration::from_secs(4);
/// A connection that has heard nothing for this long is taken for lost.
pub const SILENCE_LIMIT: Duration = Duration::from_secs(20);
/// Packets of the host's mix kept waiting at most (about 200 ms).
const AUDIO_QUEUE: usize = 10;

#[derive(Default)]
struct Queues {
    control: VecDeque<Vec<u8>>,
    presence: BTreeMap<u32, Vec<u8>>,
    audio: VecDeque<Vec<u8>>,
    sends: VecDeque<Sending>,
    /// Close once `control` is sent.
    last: bool,
    closed: bool,
}

pub(crate) struct Outbox {
    queues: Mutex<Queues>,
    wake: Condvar,
}

impl Outbox {
    fn lock(&self) -> MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn change(&self, f: impl FnOnce(&mut Queues)) {
        f(&mut self.lock());
        self.wake.notify_one();
    }

    pub fn send(&self, message: Vec<u8>) {
        self.change(|q| {
            if !q.last {
                q.control.push_back(message);
            }
        });
    }

    pub fn send_presence(&self, of: u32, message: Vec<u8>) {
        self.change(|q| {
            q.presence.insert(of, message);
        });
    }

    pub fn send_audio(&self, message: Vec<u8>) {
        self.change(|q| {
            if q.audio.len() >= AUDIO_QUEUE {
                q.audio.pop_front();
            }
            q.audio.push_back(message);
        });
    }

    pub fn send_file(&self, file: Sending) {
        self.change(|q| {
            // Asked again (a new connection's request after a lost one):
            // the newer request wins.
            q.sends.retain(|s| s.key != file.key);
            q.sends.push_back(file);
        });
    }

    /// Send `message` after what waits in order, then close.
    pub fn send_last(&self, message: Vec<u8>) {
        self.change(|q| {
            q.control.push_back(message);
            q.last = true;
        });
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    fn close(&self) {
        self.change(|q| q.closed = true);
    }

    /// Bytes waiting in order (a peer that is too slow to keep up).
    pub fn waiting(&self) -> usize {
        self.lock().control.iter().map(Vec::len).sum()
    }
}

enum Job {
    Message(Vec<u8>),
    File(Sending),
    Close,
}

/// What a link's reader does with a message.
pub(crate) enum Flow {
    Go,
    Stop,
}

pub(crate) struct Link {
    pub outbox: Arc<Outbox>,
    stopper: TcpStream,
}

impl Link {
    /// Start the threads. `handle` gets each message read (its bytes);
    /// `closed` is called once, with why, when the connection ends.
    pub fn spawn(
        channel: Channel,
        ping: Vec<u8>,
        media: fn(MediaPart) -> Option<Vec<u8>>,
        mut handle: impl FnMut(&Arc<Outbox>, Vec<u8>) -> Flow + Send + 'static,
        closed: impl FnOnce(String) + Send + 'static,
    ) -> std::io::Result<Self> {
        let Channel { reader, writer } = channel;
        let stopper = reader.stopper()?;
        let outbox = Arc::new(Outbox {
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
        });
        let out = Arc::clone(&outbox);
        std::thread::Builder::new()
            .name("ff-collab-write".into())
            .spawn(move || write_loop(writer, &out, &ping, media))?;
        let out = Arc::clone(&outbox);
        std::thread::Builder::new()
            .name("ff-collab-read".into())
            .spawn(move || {
                let why = read_loop(reader, &out, &mut handle);
                out.close();
                closed(why);
            })?;
        Ok(Self { outbox, stopper })
    }

    /// End the connection now.
    pub fn close(&self) {
        self.outbox.close();
        let _ = self.stopper.shutdown(std::net::Shutdown::Both);
    }
}

fn read_loop(
    mut reader: ChannelReader,
    outbox: &Arc<Outbox>,
    handle: &mut impl FnMut(&Arc<Outbox>, Vec<u8>) -> Flow,
) -> String {
    loop {
        match reader.read(Some(SILENCE_LIMIT)) {
            Ok(m) => {
                if let Flow::Stop = handle(outbox, m) {
                    reader.shutdown();
                    return "closed".into();
                }
            }
            Err(e) => {
                reader.shutdown();
                return match e.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                        format!("nothing heard for {} s", SILENCE_LIMIT.as_secs())
                    }
                    std::io::ErrorKind::UnexpectedEof => "the other side closed".into(),
                    _ if outbox.is_closed() => "closed".into(),
                    _ => e.to_string(),
                };
            }
        }
    }
}

fn next_job(outbox: &Outbox) -> Option<Job> {
    let mut q = outbox.lock();
    loop {
        if q.closed {
            return None;
        }
        if let Some(m) = q.control.pop_front() {
            return Some(Job::Message(m));
        }
        if q.last {
            return Some(Job::Close);
        }
        if let Some((&k, _)) = q.presence.iter().next() {
            return q.presence.remove(&k).map(Job::Message);
        }
        if let Some(m) = q.audio.pop_front() {
            return Some(Job::Message(m));
        }
        if let Some(f) = q.sends.pop_front() {
            return Some(Job::File(f));
        }
        let (g, timeout) = outbox
            .wake
            .wait_timeout(q, PING_EVERY)
            .unwrap_or_else(PoisonError::into_inner);
        q = g;
        if timeout.timed_out() && q.control.is_empty() && !q.closed {
            return Some(Job::Message(Vec::new()));
        }
    }
}

fn write_loop(
    mut writer: ChannelWriter,
    outbox: &Outbox,
    ping: &[u8],
    media: fn(MediaPart) -> Option<Vec<u8>>,
) {
    while let Some(job) = next_job(outbox) {
        let sent = match job {
            Job::Message(m) if m.is_empty() => writer.write(ping),
            Job::Message(m) => writer.write(&m),
            Job::Close => break,
            Job::File(mut f) => match f.next_part() {
                Ok(Work::Send(part)) => {
                    let r = media(part).map_or(Ok(()), |m| writer.write(&m));
                    outbox.lock().sends.push_back(f);
                    r
                }
                Ok(Work::Hashed) => {
                    outbox.lock().sends.push_back(f);
                    Ok(())
                }
                Ok(Work::Done) => Ok(()),
                Err(e) => {
                    tracing::warn!("collaboration: sending {} failed: {e}", f.key);
                    media(MediaPart::Missing { key: f.key.clone() })
                        .map_or(Ok(()), |m| writer.write(&m))
                }
            },
        };
        if sent.is_err() {
            break;
        }
    }
    outbox.close();
    writer.shutdown();
}
