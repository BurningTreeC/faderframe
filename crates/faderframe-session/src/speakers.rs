//! Speaker layouts the user defines (`faderframe_core::surround::LayoutDef`):
//! the project's library, offered as channel formats; saving one that
//! replaces another moves the tracks using it in the same undo step.

use crate::{Result, Session, SessionError};
use faderframe_core::ChannelLayout;
use faderframe_core::surround::LayoutDef;
use faderframe_project::Command;

impl Session {
    /// The project's speaker layouts.
    pub fn speaker_layouts(&self) -> &[LayoutDef] {
        &self.project.speaker_layouts
    }

    /// Keep `layout` in the library (checked: a layout VBAP can pan over,
    /// a name no other one has). `replace`: the name of the layout it
    /// replaces — tracks using that one move to this one.
    pub fn save_speaker_layout(&mut self, replace: Option<&str>, layout: LayoutDef) -> Result<()> {
        let format = layout
            .intern()
            .map_err(|e| SessionError::Other(format!("speaker layout: {e}")))?;
        let name = layout.name.trim().to_string();
        let mut list = self.project.speaker_layouts.clone();
        if list
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case(&name) && Some(d.name.as_str()) != replace)
        {
            return Err(SessionError::Other(format!(
                "speaker layout: there is a layout called {name} already"
            )));
        }
        let old = replace.and_then(|r| list.iter().position(|d| d.name == r));
        let mut commands = Vec::new();
        match old {
            Some(i) => {
                // The tracks on the old layout move to the new one.
                if let Ok(was) = list[i].intern() {
                    for t in &self.project.tracks {
                        if t.layout == ChannelLayout::Surround(was) {
                            commands.push(Command::SetTrackLayout {
                                track: t.id,
                                layout: ChannelLayout::Surround(format),
                            });
                        }
                    }
                }
                list[i] = layout;
            }
            None => list.push(layout),
        }
        commands.insert(0, Command::SetSpeakerLayouts { layouts: list });
        self.batch("Speaker Layout", commands)
    }

    /// Take the layout `name` out of the library (refused while a track
    /// uses it).
    pub fn remove_speaker_layout(&mut self, name: &str) -> Result<()> {
        let Some(i) = self
            .project
            .speaker_layouts
            .iter()
            .position(|d| d.name == name)
        else {
            return Ok(());
        };
        if let Ok(f) = self.project.speaker_layouts[i].intern() {
            let users: Vec<&str> = self
                .project
                .tracks
                .iter()
                .filter(|t| t.layout == ChannelLayout::Surround(f))
                .map(|t| t.name.as_str())
                .collect();
            if !users.is_empty() {
                return Err(SessionError::Other(format!(
                    "{name} is used by {}: give them another channel format first",
                    users.join(", ")
                )));
            }
        }
        let mut list = self.project.speaker_layouts.clone();
        list.remove(i);
        self.batch(
            "Remove Speaker Layout",
            vec![Command::SetSpeakerLayouts { layouts: list }],
        )
    }
}
