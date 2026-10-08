//! Containers (see `faderframe_project::container`): a new one starts with
//! a dry chain and an empty one; chains are added, renamed and removed and
//! devices go into and out of them, each one undoable `SetContainer`
//! (a chain's mix is `Command::SetChainMix`, from the views).

use crate::{Result, Session, SessionError};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_project::container::{Chain, MAX_CHAINS, MAX_DEPTH};
use faderframe_project::{Command, PluginRef, PluginSlot};

/// The chains a new container starts with.
pub fn default_chains() -> Vec<Chain> {
    vec![Chain::new("Dry"), Chain::new("Chain 2")]
}

impl Session {
    fn chains_of(&self, track: TrackId, container: PluginInstanceId) -> Result<Vec<Chain>> {
        self.project
            .track(track)
            .and_then(|t| t.containers.get(&container))
            .cloned()
            .ok_or_else(|| SessionError::Other("no such container".into()))
    }

    fn set_chains(
        &mut self,
        track: TrackId,
        container: PluginInstanceId,
        chains: Vec<Chain>,
    ) -> Result<()> {
        self.edit(Command::SetContainer {
            track,
            container,
            chains: Some(chains),
        })
    }

    pub(crate) fn add_chain(&mut self, track: TrackId, container: PluginInstanceId) -> Result<()> {
        let mut chains = self.chains_of(track, container).unwrap_or_default();
        if chains.len() >= MAX_CHAINS {
            return Err(SessionError::Other(format!(
                "a container has at most {MAX_CHAINS} chains"
            )));
        }
        chains.push(Chain::new(format!("Chain {}", chains.len() + 1)));
        self.set_chains(track, container, chains)
    }

    pub(crate) fn remove_chain(
        &mut self,
        track: TrackId,
        container: PluginInstanceId,
        chain: usize,
    ) -> Result<()> {
        let mut chains = self.chains_of(track, container)?;
        if chain < chains.len() {
            chains.remove(chain);
            self.set_chains(track, container, chains)?;
        }
        Ok(())
    }

    pub(crate) fn rename_chain(
        &mut self,
        track: TrackId,
        container: PluginInstanceId,
        chain: usize,
        name: String,
    ) -> Result<()> {
        let mut chains = self.chains_of(track, container)?;
        if let Some(c) = chains.get_mut(chain) {
            c.name = name;
            self.set_chains(track, container, chains)?;
        }
        Ok(())
    }

    /// The keys chain `chain`'s devices get.
    pub(crate) fn set_chain_keys(
        &mut self,
        track: TrackId,
        container: PluginInstanceId,
        chain: usize,
        low: u8,
        high: u8,
    ) -> Result<()> {
        let mut chains = self.chains_of(track, container)?;
        let (low, high) = (low.min(127), high.min(127));
        if let Some(c) = chains.get_mut(chain) {
            (c.key_low, c.key_high) = (low.min(high), low.max(high));
            self.set_chains(track, container, chains)?;
        }
        Ok(())
    }

    /// How deep the container sits in other containers (0: in the track's
    /// inserts).
    fn depth_of(&self, track: TrackId, container: PluginInstanceId) -> usize {
        let Some(t) = self.project.track(track) else {
            return 0;
        };
        let mut depth = 0;
        let mut at = container;
        while let Some((outer, _)) = t.container_of(at) {
            depth += 1;
            at = outer;
            if depth > MAX_DEPTH {
                break;
            }
        }
        depth
    }

    /// A device into chain `chain` of `container`, at `index`; a container
    /// comes with its first chains.
    pub(crate) fn insert_into_chain(
        &mut self,
        track: TrackId,
        container: PluginInstanceId,
        chain: usize,
        index: usize,
        plugin: PluginRef,
    ) -> Result<PluginInstanceId> {
        if plugin.format == faderframe_project::PluginFormat::Builtin
            && faderframe_core::builtin::is_input_stage(&plugin.id)
        {
            return Err(SessionError::Other(
                "microphone preamps go in the mixer's preamp section".into(),
            ));
        }
        if self.is_midi_effect(&plugin) {
            return Err(SessionError::Other(format!(
                "{} works on notes: it goes before the instrument, not in a container",
                plugin.name
            )));
        }
        let nested = plugin.is_container();
        if nested && self.depth_of(track, container) + 1 >= MAX_DEPTH {
            return Err(SessionError::Other(format!(
                "containers nest {MAX_DEPTH} deep at most"
            )));
        }
        let mut chains = self.chains_of(track, container)?;
        let Some(c) = chains.get_mut(chain) else {
            return Err(SessionError::Other("no such chain".into()));
        };
        let slot = PluginSlot {
            id: self.project.ids.allocate(),
            plugin,
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        };
        let id = slot.id;
        let index = index.min(c.inserts.len());
        c.inserts.insert(index, slot);
        let mut commands = vec![Command::SetContainer {
            track,
            container,
            chains: Some(chains),
        }];
        if nested {
            commands.push(Command::SetContainer {
                track,
                container: id,
                chains: Some(default_chains()),
            });
        }
        self.edit(Command::Batch {
            label: "Add to Container".into(),
            commands,
        })?;
        Ok(id)
    }

    /// Take a device out of the container chain that holds it.
    pub(crate) fn remove_from_chain(
        &mut self,
        track: TrackId,
        plugin: PluginInstanceId,
    ) -> Result<()> {
        let Some((container, _)) = self
            .project
            .track(track)
            .and_then(|t| t.container_of(plugin))
        else {
            return Ok(());
        };
        let mut chains = self.chains_of(track, container)?;
        for c in &mut chains {
            c.inserts.retain(|s| s.id != plugin);
        }
        self.set_chains(track, container, chains)
    }

    /// The containers of a track and what they hold, for the editors.
    pub fn container_chains(
        &self,
        track: TrackId,
        container: PluginInstanceId,
    ) -> Option<&[Chain]> {
        self.project
            .track(track)?
            .containers
            .get(&container)
            .map(Vec::as_slice)
    }
}
