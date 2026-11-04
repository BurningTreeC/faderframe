#![allow(clippy::unwrap_used)]

use super::mts::{self, Mts};
use super::*;

/// Ptolemy's intense diatonic in ratios, with Scala's comment lines.
const JUST: &str = "! ptolemy.scl
!
Ptolemy's intense diatonic
 7
!
 9/8
 5/4
 4/3
 3/2
 5/3
 15/8
 2/1
";

/// Quarter-comma meantone in cents, a label after each pitch and a whole
/// number for the octave.
const MEANTONE: &str = "!meantone.scl
1/4-comma meantone (cents)
12
76.049 C#
193.157 D
310.265 Eb
386.314 E
503.422 F
579.471 F#
696.578 G
772.627 G#
889.735 A
1006.843 Bb
1082.892 B
2 octave
";

/// The white keys play a seven-note scale (degree 5 = A at 440 Hz), the
/// black keys nothing.
const WHITE: &str = "! white.kbm
!
12      ! size of the pattern
0       ! first key
127     ! last key
60      ! middle key
69      ! reference key
440.0   ! reference frequency
7       ! octave degree
! the pattern
0
x
1
x
2
3
x
4
x
5
x
6
";

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn twelve_tet_at_440_is_the_standard_table() {
    let t = Tuning::twelve_tet();
    assert!(t.is_twelve_tet());
    let table = t.table(A4_HZ);
    assert!(table.is_standard());
    assert_eq!(table, TuningTable::STANDARD);
    assert!(close(table.frequency(69).unwrap(), 440.0, 1e-9));
    assert!(close(table.frequency(60).unwrap(), 261.625_565_3, 1e-6));
    assert_eq!(table.fingerprint(), TuningTable::STANDARD.fingerprint());
}

#[test]
fn the_reference_pitch_moves_every_key_alike() {
    let table = Tuning::twelve_tet().table(432.0);
    assert!(!table.is_standard());
    assert!(close(table.frequency(69).unwrap(), 432.0, 1e-9));
    assert!(close(table.frequency(81).unwrap(), 864.0, 1e-9));
    let shift = 12.0 * (432.0f64 / 440.0).log2();
    for k in 0..128u8 {
        assert!(close(table.offset(k), shift, 1e-9));
    }
    // A mapping's frequency moves with it too.
    let just = ScalaScale::parse(JUST).unwrap();
    let white = KeyboardMap::parse(WHITE).unwrap();
    let t = Tuning::scala("ptolemy", just, Some(white)).unwrap();
    assert!(close(t.table(415.0).frequency(69).unwrap(), 415.0, 1e-9));
}

#[test]
fn equal_temperaments_step_from_middle_c() {
    let t = Tuning::equal(19);
    assert_eq!(t.name, "19-EDO");
    assert!(!t.is_twelve_tet());
    let table = t.table(A4_HZ);
    assert!(close(table.pitch(60).unwrap(), 60.0, 1e-9));
    assert!(close(table.pitch(61).unwrap(), 60.0 + 12.0 / 19.0, 1e-9));
    assert!(close(table.pitch(79).unwrap(), 72.0, 1e-9), "an octave");
    assert!(
        close(table.pitch(41).unwrap(), 48.0, 1e-9),
        "an octave down"
    );
    let d = t.key_degree(66).unwrap();
    assert_eq!((d.degree, d.period), (6, 0));
    assert!(close(d.cents, 1200.0 * 6.0 / 19.0, 1e-9));
    let d = t.key_degree(40).unwrap();
    assert_eq!((d.degree, d.period), (18, -2));
}

#[test]
fn a_scala_file_reads_ratios_cents_comments_and_labels() {
    let s = ScalaScale::parse(JUST).unwrap();
    assert_eq!(s.description, "Ptolemy's intense diatonic");
    assert_eq!(s.len(), 7);
    let cents: Vec<f64> = s.degrees.iter().map(|d| d.cents).collect();
    for (got, want) in cents.iter().zip([
        203.910, 386.314, 498.045, 701.955, 884.359, 1088.269, 1200.0,
    ]) {
        assert!(close(*got, want, 1e-3), "{got} vs {want}");
    }
    assert_eq!(s.degrees[3].text, "3/2");
    let m = ScalaScale::parse(MEANTONE).unwrap();
    assert_eq!(m.len(), 12);
    assert!(close(m.degrees[0].cents, 76.049, 1e-9));
    assert!(close(m.period_cents(), 1200.0, 1e-9), "'2' is 2/1");
    let t = Tuning::scala("meantone", m.clone(), None).unwrap();
    assert!(!t.is_twelve_tet());
    // Middle C stays; E is a pure major third above it.
    let table = t.table(A4_HZ);
    assert!(close(table.pitch(60).unwrap(), 60.0, 1e-9));
    assert!(close(
        table.frequency(64).unwrap() / table.frequency(60).unwrap(),
        1.25,
        1e-6
    ));
    // What it writes reads back the same.
    let again = ScalaScale::parse(&m.to_scl("meantone")).unwrap();
    assert_eq!(again, m);
    // A Scala file of equal cents is twelve-tone equal temperament.
    let equal = ScalaScale::equal(12);
    let back = ScalaScale::parse(&equal.to_scl("12")).unwrap();
    assert!(Tuning::scala("12", back, None).unwrap().is_twelve_tet());
}

#[test]
fn broken_scala_files_say_where() {
    let err = |text: &str| ScalaScale::parse(text).unwrap_err();
    assert!(err("! nothing\n").message.contains("description"));
    assert!(err("desc\n").message.contains("number of notes"));
    assert_eq!(err("desc\nmany\n1/1\n").line, Some(2));
    assert!(err("desc\n3\n9/8\n5/4\n").message.contains("3 notes"));
    let e = err("desc\n2\n9/8\n-3/2\n");
    assert_eq!(e.line, Some(4));
    assert!(e.to_string().starts_with("line 4: "));
    assert!(err("desc\n1\n3/0\n").message.contains("ratio"));
    assert!(err("desc\n1\nabc\n").message.contains("ratio"));
    assert!(err("desc\n1\n0.0\n").message.contains("period"));
    assert!(err("desc\n0\n").message.contains("1 to"));
}

#[test]
fn a_keyboard_map_leaves_keys_out_and_fixes_its_reference() {
    let just = ScalaScale::parse(JUST).unwrap();
    let white = KeyboardMap::parse(WHITE).unwrap();
    assert_eq!(white.size, 12);
    assert_eq!((white.middle, white.reference_key), (60, 69));
    assert_eq!(white.map[1], None);
    assert_eq!(white.map[11], Some(6));
    let t = Tuning::scala("ptolemy", just, Some(white)).unwrap();
    let table = t.table(A4_HZ);
    // A (degree 5, 5/3) is 440 Hz, so C is 264 Hz: just intonation.
    assert!(close(table.frequency(69).unwrap(), 440.0, 1e-9));
    assert!(close(table.frequency(60).unwrap(), 264.0, 1e-9));
    assert!(close(table.frequency(64).unwrap(), 330.0, 1e-9));
    assert!(close(table.frequency(67).unwrap(), 396.0, 1e-9));
    assert!(close(table.frequency(72).unwrap(), 528.0, 1e-9));
    assert!(close(table.frequency(48).unwrap(), 132.0, 1e-9));
    for black in [61, 63, 66, 68, 70, 73] {
        assert!(!table.is_mapped(black), "{black}");
        assert_eq!(t.key_degree(black), None);
    }
    assert_eq!(t.key_degree(71).unwrap().degree, 6);
    assert_eq!(t.key_degree(72).unwrap().degree, 0);
    assert_eq!(t.key_degree(72).unwrap().period, 1);
    // What it writes reads back the same.
    let back = KeyboardMap::parse(&t.keyboard().to_kbm()).unwrap();
    assert_eq!(back, t.keyboard());
}

#[test]
fn keys_outside_the_range_stay_equal_tempered_and_short_maps_are_unmapped() {
    let text = "8\n60\n72\n60\n60\n261.6255653\n12\n0\n2\n4\n";
    let k = KeyboardMap::parse(text).unwrap();
    assert_eq!(k.map.len(), 8, "missing entries are unmapped");
    assert_eq!(k.map[3], None);
    let t = Tuning::equal(12).with_mapping(Some(k)).unwrap();
    let table = t.table(A4_HZ);
    assert!(close(table.pitch(59).unwrap(), 59.0, 1e-9));
    assert!(close(table.pitch(73).unwrap(), 73.0, 1e-9));
    assert!(
        close(table.pitch(61).unwrap(), 62.0, 1e-6),
        "entry 1 = degree 2"
    );
    assert!(!table.is_mapped(63));
    assert!(
        close(table.pitch(68).unwrap(), 72.0, 1e-6),
        "the next pattern"
    );
}

#[test]
fn a_map_must_map_its_reference_key() {
    let just = ScalaScale::parse(JUST).unwrap();
    let mut white = KeyboardMap::parse(WHITE).unwrap();
    white.reference_key = 70;
    let e = Tuning::scala("x", just, Some(white)).unwrap_err();
    assert!(e.message.contains("reference key 70"));
    let bad = |text: &str| KeyboardMap::parse(text).unwrap_err().message;
    assert!(bad("12\n0\n127\n60\n69\n").contains("frequency"));
    assert!(bad("1\n0\n128\n60\n69\n440\n1\n0\n").contains("last key"));
    assert!(bad("1\n0\n127\n60\n69\n-1\n1\n0\n").contains("above 0"));
    assert!(bad("1\n0\n127\n60\n69\n440\n1\ny\n").contains("neither"));
}

#[test]
fn mts_frequency_data_follows_the_standard() {
    // The specification's examples.
    assert_eq!(mts::frequency_bytes(0.0), [0, 0, 0]);
    assert_eq!(mts::frequency_bytes(69.0), [0x45, 0, 0]);
    assert_eq!(mts::frequency_bytes(69.0 + 1.0 / 16384.0), [0x45, 0, 1]);
    assert_eq!(mts::frequency_bytes(69.5), [0x45, 0x40, 0]);
    assert_eq!(mts::frequency_bytes(127.99999), [0x7F, 0x7F, 0x7E]);
    assert_eq!(mts::frequency_bytes(200.0), [0x7F, 0x7F, 0x7E]);
    assert_eq!(mts::frequency_bytes(-3.0), [0, 0, 0]);
    // Rounding up to the next semitone.
    assert_eq!(mts::frequency_bytes(60.999_99), [61, 0, 0]);
    assert_eq!(mts::pitch_of([0x7F, 0x7F, 0x7F]), None);
    assert!(close(mts::pitch_of([0x45, 0x40, 0]).unwrap(), 69.5, 1e-12));
}

#[test]
fn a_bulk_dump_carries_the_table_its_name_and_checksum() {
    let table = Tuning::equal(19).table(A4_HZ);
    let dump = mts::bulk_dump(&table, 3, "19-EDO tuning çé long name");
    assert_eq!(dump.len(), mts::BULK_LEN);
    assert_eq!(&dump[..6], &[0xF0, 0x7E, 0x7F, 0x08, 0x01, 3]);
    assert_eq!(&dump[6..22], b"19-EDO tuning ??");
    assert_eq!(*dump.last().unwrap(), 0xF7);
    assert!(dump[1..dump.len() - 1].iter().all(|b| *b < 0x80));
    // Key 61: one step of 19 above middle C.
    let k = 22 + 3 * 61;
    assert_eq!(dump[k], 60);
    let Some(Mts::Bulk {
        program,
        name,
        pitches,
    }) = mts::parse(&dump)
    else {
        panic!("not read back")
    };
    assert_eq!((program, name.as_str()), (3, "19-EDO tuning ??"));
    for (k, p) in pitches.iter().enumerate() {
        assert!(close(
            p.unwrap(),
            table.pitch(k as u8).unwrap(),
            0.5 / 16384.0
        ));
    }
    // A broken checksum is no dump.
    let mut broken = dump.clone();
    broken[100] ^= 1;
    assert_eq!(mts::parse(&broken), None);
}

#[test]
fn real_time_changes_cover_the_keyboard_and_leave_unmapped_keys_at_home() {
    let just = ScalaScale::parse(JUST).unwrap();
    let white = KeyboardMap::parse(WHITE).unwrap();
    let table = Tuning::scala("p", just, Some(white)).unwrap().table(A4_HZ);
    let messages = mts::note_changes(&table, 0);
    assert_eq!(messages.len(), 2);
    let mut seen = Vec::new();
    for m in &messages {
        assert_eq!(&m[..5], &[0xF0, 0x7F, 0x7F, 0x08, 0x02]);
        assert_eq!(m.len(), 8 + 4 * mts::KEYS_PER_CHANGE);
        let Some(Mts::NoteChange {
            realtime, changes, ..
        }) = mts::parse(m)
        else {
            panic!("not read back")
        };
        assert!(realtime);
        seen.extend(changes);
    }
    assert_eq!(seen.len(), 128);
    for (k, p) in seen {
        let want = table.pitch(k).unwrap_or(f64::from(k));
        assert!(close(p.unwrap(), want, 0.5 / 16384.0), "{k}");
    }
    // "No change" travels as 7F 7F 7F.
    let one = mts::note_change(1, &[(60, None), (62, Some(62.25))]);
    assert_eq!(
        one,
        [
            0xF0, 0x7F, 0x7F, 0x08, 0x02, 1, 2, 60, 0x7F, 0x7F, 0x7F, 62, 62, 0x20, 0, 0xF7
        ]
    );
}
