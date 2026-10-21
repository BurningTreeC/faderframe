//! JACK's physical ports as devices (grouped by client: JACK2's `system`,
//! or one client per interface under pipewire-jack), and which of them
//! each of the stream's channels connects to. Pure: tested without a
//! server.

use std::collections::HashMap;

/// One client's physical ports: a device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Group {
    pub client: String,
    /// Full port names that capture (the stream's inputs connect from).
    pub capture: Vec<String>,
    /// Full port names that play (the stream's outputs connect to).
    pub playback: Vec<String>,
}

fn client_of(port: &str) -> &str {
    port.split_once(':').map_or(port, |(c, _)| c)
}

/// The short name of a full port name ("system:capture_1" → "capture_1").
pub(crate) fn short(port: &str) -> &str {
    port.split_once(':').map_or(port, |(_, p)| p)
}

/// Group physical ports by client, in the order JACK lists them.
pub(crate) fn groups(capture: &[String], playback: &[String]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    let mut at = |port: &str| -> usize {
        let client = client_of(port);
        match out.iter().position(|g| g.client == client) {
            Some(i) => i,
            None => {
                out.push(Group {
                    client: client.into(),
                    ..Group::default()
                });
                out.len() - 1
            }
        }
    };
    let mut slots: Vec<(usize, bool, String)> = Vec::new();
    for p in capture {
        slots.push((at(p), false, p.clone()));
    }
    for p in playback {
        slots.push((at(p), true, p.clone()));
    }
    for (i, playback, p) in slots {
        if playback {
            out[i].playback.push(p);
        } else {
            out[i].capture.push(p);
        }
    }
    out
}

/// What the stream's channels of one direction connect to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Assignment {
    /// For each channel, the device port (full name), if any.
    pub ports: Vec<Option<String>>,
    /// Devices asked for that are not there.
    pub missing: Vec<String>,
}

/// Assign `channels` stream channels of a direction to the wanted
/// devices' ports, one device after another; a device that is not there
/// keeps its place (`known` channel counts), and when none is there the
/// first device with ports of that direction stands in.
pub(crate) fn assign(
    groups: &[Group],
    wanted: &[String],
    playback: bool,
    channels: usize,
    known: &HashMap<String, usize>,
) -> Assignment {
    fn ports_of(g: &Group, playback: bool) -> &[String] {
        if playback { &g.playback } else { &g.capture }
    }
    let mut out = Assignment {
        ports: vec![None; channels],
        missing: Vec::new(),
    };
    let mut spans: Vec<(Option<&Group>, usize)> = Vec::new();
    for name in wanted {
        match groups
            .iter()
            .find(|g| g.client == *name && !ports_of(g, playback).is_empty())
        {
            Some(g) => spans.push((Some(g), ports_of(g, playback).len())),
            None => {
                out.missing.push(name.clone());
                spans.push((None, known.get(name).copied().unwrap_or(0)));
            }
        }
    }
    if spans.iter().all(|(g, _)| g.is_none()) {
        spans = groups
            .iter()
            .find(|g| !ports_of(g, playback).is_empty())
            .map(|g| (Some(g), ports_of(g, playback).len()))
            .into_iter()
            .collect();
    }
    let mut channel = 0;
    for (group, width) in spans {
        let Some(g) = group else {
            channel += width;
            continue;
        };
        for port in ports_of(g, playback) {
            if channel >= channels {
                break;
            }
            out.ports[channel] = Some(port.clone());
            channel += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn physical_ports_group_into_devices() {
        let g = groups(
            &names(&["system:capture_1", "system:capture_2", "USB:capture_FL"]),
            &names(&["system:playback_1", "system:playback_2"]),
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].client, "system");
        assert_eq!(g[0].playback.len(), 2);
        assert_eq!(g[1].capture, names(&["USB:capture_FL"]));
        assert!(g[1].playback.is_empty());
        assert_eq!(short("system:capture_1"), "capture_1");
    }

    #[test]
    fn channels_follow_the_wanted_devices_and_keep_a_missing_ones_place() {
        let g = groups(
            &[],
            &names(&[
                "A:playback_1",
                "A:playback_2",
                "B:playback_1",
                "B:playback_2",
            ]),
        );
        let a = assign(&g, &[], true, 2, &HashMap::new());
        assert_eq!(
            a.ports,
            vec![Some("A:playback_1".into()), Some("A:playback_2".into())]
        );
        let both = names(&["B", "A"]);
        let a = assign(&g, &both, true, 3, &HashMap::new());
        assert_eq!(a.ports[0].as_deref(), Some("B:playback_1"));
        assert_eq!(a.ports[2].as_deref(), Some("A:playback_1"));
        // B unplugged: A keeps outputs 3–4.
        let only_a = groups(&[], &names(&["A:playback_1", "A:playback_2"]));
        let known = HashMap::from([("B".to_string(), 2)]);
        let a = assign(&only_a, &both, true, 4, &known);
        assert_eq!(a.ports[0], None);
        assert_eq!(a.ports[2].as_deref(), Some("A:playback_1"));
        assert_eq!(a.missing, names(&["B"]));
        // Nothing asked for there: the first device stands in.
        let a = assign(&only_a, &names(&["C"]), true, 2, &HashMap::new());
        assert_eq!(a.ports[0].as_deref(), Some("A:playback_1"));
        assert_eq!(a.missing, names(&["C"]));
    }
}
