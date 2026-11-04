//! AAF files from other writers (opt-in): `FADERFRAME_TEST_AAF=<file>[:<file>…]`
//! reads each at 48 kHz and prints what came out (`--nocapture`).

use faderframe_aaf::read::AafFile;

#[test]
#[ignore = "needs FADERFRAME_TEST_AAF"]
fn other_writers_files_read() {
    let Some(list) = std::env::var_os("FADERFRAME_TEST_AAF") else {
        return;
    };
    for path in std::env::split_paths(&list) {
        let mut f = AafFile::open_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let read = f.composition(48_000).unwrap();
        let c = &read.composition;
        println!("{}: {:?} timecode {:?}", path.display(), c.name, c.timecode);
        for t in &c.tracks {
            println!("  track {:?}", t.name);
            for k in &t.clips {
                println!(
                    "    {:>9} +{:<8} {:?} ch {} from {} gain {:.2} env {:?} in {:?} out {:?}",
                    k.position,
                    k.length,
                    c.media[k.media].name,
                    k.channel,
                    k.offset,
                    k.gain_db,
                    k.envelope,
                    k.fade_in,
                    k.fade_out
                );
            }
        }
        for m in &c.markers {
            println!("  marker {} {:?}", m.position, m.name);
        }
        for m in &c.media {
            println!(
                "  media {:?} {} ch {} Hz {} frames {:?}",
                m.name, m.channels, m.rate, m.frames, m.essence
            );
        }
        println!("  notes {:?}", read.notes);
        assert!(
            !c.tracks.is_empty(),
            "no sound tracks in {}",
            path.display()
        );
    }
}
