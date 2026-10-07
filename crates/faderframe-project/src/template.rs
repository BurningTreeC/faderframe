//! Project templates: a project's set-up without its content.
//!
//! A template keeps everything a project is set up with — tracks and their
//! routing, devices with their state, sends, groups, VCAs, folders,
//! modulators, MIDI mappings, automation, tempo, meter, key and chords,
//! markers and sections, loop and punch, the album's delivery settings and
//! the launcher's scenes — and leaves out what was recorded or imported:
//! clips (arranger, launcher and video), their media, freezes, aliases, lyrics
//! (transcribed from the audio) and album songs that are files.

use crate::Project;
use crate::album::SongSource;

/// `project` without its content (see the module).
pub fn without_content(project: &Project) -> Project {
    let mut p = project.clone();
    p.clips.clear();
    p.sources.clear();
    p.clip_links.clear();
    p.lyrics.clear();
    for t in &mut p.tracks {
        t.clips.clear();
        t.freeze = None;
    }
    // Video tracks stay, their clips and files go.
    p.video.sources.clear();
    for t in &mut p.video.tracks {
        t.clips.clear();
    }
    let launcher = &mut p.launcher;
    launcher.slots.clear();
    launcher.follow.clear();
    launcher.launch.clear();
    p.album
        .songs
        .retain(|s| matches!(s.source, SongSource::Section(_) | SongSource::ThisProject));
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::demo_project;

    #[test]
    fn a_template_keeps_the_set_up_and_leaves_the_content() {
        let demo = demo_project(48_000);
        assert!(!demo.clips.is_empty() && !demo.sources.is_empty());
        let t = without_content(&demo);
        assert!(t.clips.is_empty() && t.sources.is_empty());
        assert!(
            t.tracks
                .iter()
                .all(|t| t.clips.is_empty() && t.freeze.is_none())
        );
        assert!(t.launcher.slots.is_empty());
        // The set-up is all there.
        assert_eq!(t.tracks.len(), demo.tracks.len());
        for (a, b) in t.tracks.iter().zip(&demo.tracks) {
            assert_eq!((a.id, &a.name, a.kind), (b.id, &b.name, b.kind));
            assert_eq!(a.inserts, b.inserts);
            assert_eq!(a.sends, b.sends);
            assert_eq!((&a.input, &a.output), (&b.input, &b.output));
            assert_eq!(a.automation, b.automation);
            assert_eq!(a.modulators, b.modulators);
            assert_eq!((a.folder, a.group, a.vca), (b.folder, b.group, b.vca));
            assert_eq!(a.volume_db, b.volume_db);
        }
        assert_eq!(t.timeline, demo.timeline);
        assert_eq!(t.markers, demo.markers);
        assert_eq!(t.sections, demo.sections);
        assert_eq!(t.keys, demo.keys);
        assert_eq!(t.chords, demo.chords);
        assert_eq!(t.groups, demo.groups);
        assert_eq!(t.midi_mappings, demo.midi_mappings);
        assert_eq!(t.launcher.scenes, demo.launcher.scenes);
        // New ids never meet the demo's.
        assert_eq!(t.ids, demo.ids);
    }
}
