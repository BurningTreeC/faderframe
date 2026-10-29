//! Measuring beside the render: the profiles of the tapped signals are
//! built on a bounded number of threads while the render runs, never one
//! thread per track.
//!
//! The render thread gathers each signal's audio into chunks of [`CHUNK`]
//! frames and hands a full chunk to the measuring thread that owns the
//! signal (signals are dealt round-robin to [`threads`] threads; one owner
//! per signal keeps its audio in order). The way to each thread holds at
//! most [`IN_FLIGHT`] chunks: a render faster than its measurement waits
//! for it (memory stays bounded, whatever the range's length); chunks come
//! back to the render thread to be filled again. A signal to be held
//! ([`super::held`]) is held by the same thread as it is measured.

use super::held::HeldAudio;
use faderframe_mix_intelligence::TrackProfile;
use faderframe_mix_intelligence::profile::ProfileBuilder;
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};

/// Frames gathered per signal before they are handed over.
pub(super) const CHUNK: usize = 16_384;
/// Chunks waiting for a measuring thread at most.
const IN_FLIGHT: usize = 4;
/// The most measuring threads.
const MAX_THREADS: usize = 8;

/// Measuring threads for `signals` signals: half the machine's cores (the
/// render uses the rest), at most [`MAX_THREADS`], never more than there
/// are signals.
pub(super) fn threads(signals: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cores / 2).clamp(1, MAX_THREADS).min(signals.max(1))
}

type Chunk = Vec<Vec<f32>>;
/// A signal's chunk on its way to its measuring thread.
type Handed = (usize, Chunk);

/// A signal's profile and, when it was to be held, its audio.
pub(super) type MeasuredSignal = (TrackProfile, Option<HeldAudio>);

/// Build `builders` (one per signal, by index; each with the audio to hold
/// it in, or none) from audio `render` hands to the feed it is given
/// (`feed(signal, piece)`) on [`threads`] threads. Returns what `render`
/// returned and each signal's profile and held audio (or why it could not
/// be measured).
pub(super) fn in_parallel<R>(
    builders: Vec<(ProfileBuilder, Option<HeldAudio>)>,
    render: impl FnOnce(&mut dyn FnMut(usize, &[&[f32]])) -> R,
) -> (R, Vec<Result<MeasuredSignal, String>>) {
    let n = builders.len();
    let k = threads(n);
    type Owned = (usize, (ProfileBuilder, Option<HeldAudio>));
    let mut owned: Vec<Vec<Owned>> = (0..k).map(|_| Vec::new()).collect();
    for (i, b) in builders.into_iter().enumerate() {
        owned[i % k].push((i, b));
    }
    std::thread::scope(|scope| {
        let (back_tx, back_rx) = channel::<Chunk>();
        let mut senders: Vec<SyncSender<Handed>> = Vec::with_capacity(k);
        let mut handles = Vec::with_capacity(k);
        for (w, mine) in owned.into_iter().enumerate() {
            let (tx, rx): (SyncSender<Handed>, Receiver<Handed>) = sync_channel(IN_FLIGHT);
            senders.push(tx);
            let back = back_tx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("faderframe-mix-measure-{w}"))
                .spawn_scoped(scope, move || {
                    let mut mine = mine;
                    let mut failed: Vec<(usize, String)> = Vec::new();
                    for (i, chunk) in rx {
                        if let Some((_, (b, held))) = mine.iter_mut().find(|(x, _)| *x == i)
                            && !failed.iter().any(|(x, _)| *x == i)
                        {
                            let views: Vec<&[f32]> = chunk.iter().map(Vec::as_slice).collect();
                            if let Err(e) = b.push(&views) {
                                failed.push((i, e.to_string()));
                            }
                            if let Some(h) = held {
                                h.push(&views);
                            }
                        }
                        // Back to be filled again (the render may be done).
                        let _ = back.send(chunk);
                    }
                    mine.into_iter()
                        .map(|(i, (b, held))| {
                            let r = match failed.iter().find(|(x, _)| *x == i) {
                                Some((_, e)) => Err(e.clone()),
                                None => Ok((b.finish(), held.map(HeldAudio::finish))),
                            };
                            (i, r)
                        })
                        .collect::<Vec<_>>()
                });
            handles.push(handle);
        }
        drop(back_tx);
        // What each signal has gathered so far.
        let mut gathered: Vec<Option<Chunk>> = (0..n).map(|_| None).collect();
        let mut spare: Vec<Chunk> = Vec::new();
        let send = |i: usize, chunk: Chunk, spare: &mut Vec<Chunk>| {
            spare.extend(back_rx.try_iter());
            if let Err(e) = senders[i % k].send((i, chunk)) {
                // The thread is gone (it panicked): its signals are lost.
                spare.push(e.0.1);
            }
        };
        let result = {
            let mut feed = |i: usize, piece: &[&[f32]]| {
                if i >= n {
                    return;
                }
                let mut done = 0;
                let len = piece.first().map_or(0, |c| c.len());
                while done < len {
                    let g = gathered[i].get_or_insert_with(|| {
                        let mut c = spare.pop().unwrap_or_default();
                        c.resize_with(piece.len(), || Vec::with_capacity(CHUNK));
                        c.truncate(piece.len());
                        for ch in c.iter_mut() {
                            ch.clear();
                        }
                        c
                    });
                    let have = g.first().map_or(0, Vec::len);
                    let take = (CHUNK - have).min(len - done);
                    for (dst, src) in g.iter_mut().zip(piece) {
                        dst.extend_from_slice(&src[done..done + take]);
                    }
                    done += take;
                    if have + take >= CHUNK
                        && let Some(full) = gathered[i].take()
                    {
                        send(i, full, &mut spare);
                    }
                }
            };
            render(&mut feed)
        };
        // What is left of each signal, then the threads finish.
        for (i, g) in gathered.iter_mut().enumerate() {
            if let Some(rest) = g.take()
                && rest.first().is_some_and(|c| !c.is_empty())
            {
                send(i, rest, &mut spare);
            }
        }
        drop(senders);
        let mut out: Vec<Result<MeasuredSignal, String>> = (0..n)
            .map(|_| Err("the measuring thread stopped".to_string()))
            .collect();
        for h in handles {
            if let Ok(Ok(done)) = h.map(|h| h.join()) {
                for (i, r) in done {
                    out[i] = r;
                }
            }
        }
        (result, out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_mix_intelligence::AnalysisConfig;

    #[test]
    fn signals_measured_in_parallel_are_measured_as_in_one_go() {
        let rate = 48_000;
        let config = AnalysisConfig::for_rate(rate);
        let sig = |hz: f64, ch: usize| -> Vec<Vec<f32>> {
            (0..ch)
                .map(|c| {
                    (0..rate as usize * 3)
                        .map(|i| {
                            (0.3 * (std::f64::consts::TAU * hz * (c + 1) as f64 * i as f64
                                / f64::from(rate))
                            .sin()) as f32
                        })
                        .collect()
                })
                .collect()
        };
        let signals: Vec<Vec<Vec<f32>>> = (0..11)
            .map(|i| sig(100.0 + 37.0 * i as f64, 1 + i % 2))
            .collect();
        let whole: Vec<TrackProfile> = signals
            .iter()
            .map(|s| {
                let views: Vec<&[f32]> = s.iter().map(Vec::as_slice).collect();
                faderframe_mix_intelligence::analyse(&faderframe_mix_intelligence::ProfileRequest {
                    channels: &views,
                    rate,
                    config: &config,
                    spans: &[(1000, 90_000)],
                    cancel: None,
                })
                .unwrap_or_else(|e| panic!("{e}"))
            })
            .collect();
        let builders: Vec<(ProfileBuilder, Option<HeldAudio>)> = signals
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    ProfileBuilder::new(rate, s.len(), &config, &[(1000, 90_000)])
                        .unwrap_or_else(|e| panic!("{e}")),
                    (i % 2 == 0).then(|| HeldAudio::new(s.len(), s[0].len())),
                )
            })
            .collect();
        // Pieces of an odd size, the signals interleaved as a render gives
        // them.
        let (calls, got) = in_parallel(builders, |feed| {
            let mut calls = 0;
            let mut at = 0;
            while at < rate as usize * 3 {
                let end = (at + 1001).min(rate as usize * 3);
                for (i, s) in signals.iter().enumerate() {
                    let piece: Vec<&[f32]> = s.iter().map(|c| &c[at..end]).collect();
                    feed(i, &piece);
                }
                calls += 1;
                at = end;
            }
            calls
        });
        assert!(calls > 100);
        for (i, (g, w)) in got.into_iter().zip(&whole).enumerate() {
            let (profile, held) = g.unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(&profile, w);
            // Held where asked, every frame of it.
            assert_eq!(
                held.map(|h| h.frames()),
                (i % 2 == 0).then_some(rate as usize * 3)
            );
        }
    }
}
