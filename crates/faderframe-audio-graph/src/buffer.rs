use faderframe_core::ChannelLayout;

/// Multichannel, non-interleaved audio buffer with fixed capacity.
///
/// The *length* (frames of the current block) is set by the executor before
/// a node runs; accessors return slices of exactly that length.
#[derive(Clone, Debug)]
pub struct AudioBuffer {
    layout: ChannelLayout,
    channels: Vec<Box<[f32]>>,
    len: usize,
}

impl AudioBuffer {
    /// Allocate a zeroed buffer (control thread).
    pub fn new(layout: ChannelLayout, capacity: usize) -> Self {
        Self {
            layout,
            channels: (0..layout.channel_count())
                .map(|_| vec![0.0; capacity].into_boxed_slice())
                .collect(),
            len: 0,
        }
    }

    #[inline]
    pub fn layout(&self) -> ChannelLayout {
        self.layout
    }

    #[inline]
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.channels.first().map_or(0, |c| c.len())
    }

    /// Frames in the current block.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Set the current block length (clamped to capacity).
    #[inline]
    pub fn set_len(&mut self, frames: usize) {
        self.len = frames.min(self.capacity());
    }

    #[inline]
    pub fn channel(&self, ch: usize) -> &[f32] {
        &self.channels[ch][..self.len]
    }

    #[inline]
    pub fn channel_mut(&mut self, ch: usize) -> &mut [f32] {
        let len = self.len;
        &mut self.channels[ch][..len]
    }

    /// All active channels as disjoint mutable slices, without allocating.
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let len = self.len;
        self.channels.iter_mut().map(move |c| &mut c[..len])
    }

    /// Two distinct channels mutably at once (e.g. stereo processing).
    #[inline]
    pub fn channel_pair_mut(&mut self, a: usize, b: usize) -> (&mut [f32], &mut [f32]) {
        assert!(a != b, "channel_pair_mut needs distinct channels");
        let len = self.len;
        if a < b {
            let (lo, hi) = self.channels.split_at_mut(b);
            (&mut lo[a][..len], &mut hi[0][..len])
        } else {
            let (lo, hi) = self.channels.split_at_mut(a);
            (&mut hi[0][..len], &mut lo[b][..len])
        }
    }

    /// Zero the current block.
    #[inline]
    pub fn clear(&mut self) {
        let len = self.len;
        for c in &mut self.channels {
            c[..len].fill(0.0);
        }
    }

    /// Add `src` into `self` using the standard channel conversion rules
    /// (see [`for_each_channel_route`]).
    pub fn mix_from(&mut self, src: &AudioBuffer) {
        let n = self.len.min(src.len);
        let (sc, dc) = (src.num_channels(), self.num_channels());
        for_each_channel_route(sc, dc, |s, d, w| {
            let dst = &mut self.channels[d][..n];
            let srcs = &src.channels[s][..n];
            if w == 1.0 {
                for (o, i) in dst.iter_mut().zip(srcs) {
                    *o += *i;
                }
            } else {
                for (o, i) in dst.iter_mut().zip(srcs) {
                    *o += *i * w;
                }
            }
        });
    }

    /// Overwrite `self` with `src` (with channel conversion).
    pub fn copy_from(&mut self, src: &AudioBuffer) {
        self.clear();
        self.mix_from(src);
    }
}

/// Channel conversion rules used whenever an audio edge connects ports of
/// different layouts. Calls `route(src_channel, dst_channel, weight)`.
///
/// * equal channel counts — one-to-one;
/// * mono → N — the mono signal feeds every destination channel;
/// * N → mono — channels are averaged (`1/N` each), so a correlated stereo
///   signal keeps its level;
/// * otherwise — the first `min(N, M)` channels map one-to-one.
///
/// Explicit channel-conversion/panner nodes should be used where a different
/// behaviour is wanted.
#[inline]
pub fn for_each_channel_route(src: usize, dst: usize, mut route: impl FnMut(usize, usize, f32)) {
    if src == 0 || dst == 0 {
        return;
    }
    if src == dst {
        for c in 0..src {
            route(c, c, 1.0);
        }
    } else if src == 1 {
        for d in 0..dst {
            route(0, d, 1.0);
        }
    } else if dst == 1 {
        let w = 1.0 / src as f32;
        for s in 0..src {
            route(s, 0, w);
        }
    } else {
        for c in 0..src.min(dst) {
            route(c, c, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(layout: ChannelLayout, values: &[f32]) -> AudioBuffer {
        let mut b = AudioBuffer::new(layout, 8);
        b.set_len(4);
        for (c, v) in values.iter().enumerate() {
            b.channel_mut(c).fill(*v);
        }
        b
    }

    #[test]
    fn mono_to_stereo_duplicates() {
        let src = filled(ChannelLayout::Mono, &[0.5]);
        let mut dst = filled(ChannelLayout::Stereo, &[0.0, 0.0]);
        dst.mix_from(&src);
        assert_eq!(dst.channel(0), &[0.5; 4]);
        assert_eq!(dst.channel(1), &[0.5; 4]);
    }

    #[test]
    fn stereo_to_mono_averages() {
        let src = filled(ChannelLayout::Stereo, &[1.0, 0.0]);
        let mut dst = filled(ChannelLayout::Mono, &[0.25]);
        dst.mix_from(&src);
        assert_eq!(dst.channel(0), &[0.75; 4]);
    }

    #[test]
    fn discrete_maps_common_channels() {
        let src = filled(ChannelLayout::Discrete(4), &[1.0, 2.0, 3.0, 4.0]);
        let mut dst = filled(ChannelLayout::Stereo, &[0.0, 0.0]);
        dst.copy_from(&src);
        assert_eq!(dst.channel(0), &[1.0; 4]);
        assert_eq!(dst.channel(1), &[2.0; 4]);
    }

    #[test]
    fn channel_pair_access() {
        let mut b = filled(ChannelLayout::Stereo, &[1.0, 2.0]);
        let (r, l) = b.channel_pair_mut(1, 0);
        assert_eq!((r[0], l[0]), (2.0, 1.0));
    }
}
