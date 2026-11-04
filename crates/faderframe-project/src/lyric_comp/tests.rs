use super::*;
use crate::Take;

const RATE: u32 = 48_000;

fn ms(v: f64) -> i64 {
    (v * f64::from(RATE) / 1000.0).round() as i64
}

/// A word over `a..b` milliseconds.
fn w(a: f64, b: f64, text: &str) -> FolderWord {
    FolderWord {
        start: ms(a),
        end: ms(b),
        text: text.into(),
        line: false,
        confidence: 90,
        index: 0,
    }
}

fn folder(takes: usize, len_ms: f64) -> TakeFolder {
    let mut f = TakeFolder::new(ms(len_ms));
    for i in 0..takes {
        f.add_take(Take {
            name: format!("Take {}", i + 1),
            source: AudioSourceId(i as u64 + 1),
            source_offset: 0,
            start: 0,
            end: ms(len_ms),
            gain_db: 0.0,
            rating: 0,
        });
    }
    f
}

/// The folder's invariants (`TakeFolder::normalize`).
fn holds(f: &TakeFolder) {
    assert_eq!(f.comp.first().map(|s| s.start), Some(0), "{:?}", f.comp);
    assert!(
        f.comp.windows(2).all(|p| p[0].start < p[1].start),
        "{:?}",
        f.comp
    );
    assert!(
        f.comp.windows(2).all(|p| p[0].take != p[1].take),
        "{:?}",
        f.comp
    );
    assert!(
        f.comp.iter().all(|s| s.start < f.length.max(1)),
        "{:?}",
        f.comp
    );
    assert!(
        f.comp
            .iter()
            .all(|s| s.take.is_none_or(|t| t < f.takes.len())),
        "{:?}",
        f.comp
    );
}

#[test]
fn heard_spans_merge_and_say_what_is_missing() {
    let mut s = SourceWords::new(RATE);
    assert_eq!(s.missing(0, 100), [(0, 100)]);
    let word = |a, b, t: &str| SungWord {
        start: a,
        end: b,
        text: t.into(),
        line: false,
        confidence: 0,
    };
    s.add((10, 40), vec![word(12, 20, "a"), word(25, 30, "b")]);
    s.add((60, 80), vec![word(62, 70, "c")]);
    assert_eq!(s.missing(0, 100), [(0, 10), (40, 60), (80, 100)]);
    assert_eq!(s.missing(15, 35), []);
    // Heard again: its words replace those starting inside.
    s.add((20, 61), vec![word(22, 28, "B"), word(50, 55, "x")]);
    assert_eq!(s.heard, [(10, 80)]);
    let texts: Vec<&str> = s.words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(texts, ["a", "B", "x", "c"]);
}

#[test]
fn a_takes_words_land_in_its_folder() {
    let mut f = folder(2, 3000.0);
    f.takes[1].source_offset = ms(500.0);
    f.takes[1].start = ms(1000.0);
    let mut words = BTreeMap::new();
    for (id, list) in [
        (1, vec![(100.0, 400.0, "one"), (2900.0, 3200.0, "late")]),
        (2, vec![(600.0, 900.0, "early"), (1700.0, 2000.0, " two ")]),
    ] {
        let mut s = SourceWords::new(RATE);
        s.add(
            (0, ms(10_000.0)),
            list.into_iter()
                .map(|(a, b, t)| SungWord {
                    start: ms(a),
                    end: ms(b),
                    text: t.into(),
                    line: false,
                    confidence: 80,
                })
                .collect(),
        );
        words.insert(AudioSourceId(id), s);
    }
    let tw = take_words(&f, &words, RATE);
    // Take 1: "late" ends past the folder (its middle is outside).
    assert_eq!(tw[0].len(), 1);
    assert_eq!((tw[0][0].start, tw[0][0].text.as_str()), (ms(100.0), "one"));
    // Take 2: source 600 ms is folder 100 ms, before its material.
    assert_eq!(tw[1].len(), 1);
    assert_eq!(tw[1][0].start, ms(1200.0));
    assert_eq!(tw[1][0].text, "two");
    assert_eq!(tw[1][0].index, 1);
}

/// Three takes of one line: the same words at slightly different times,
/// one word misheard in take 2 and one left out in take 3.
fn three_takes() -> Vec<Vec<FolderWord>> {
    vec![
        vec![
            w(100.0, 400.0, "Shine"),
            w(500.0, 800.0, "a"),
            w(900.0, 1300.0, "light"),
            w(1500.0, 1900.0, "on"),
            w(2000.0, 2500.0, "me."),
        ],
        vec![
            w(130.0, 420.0, "shine"),
            w(520.0, 790.0, "the"),
            w(930.0, 1350.0, "light"),
            w(1480.0, 1880.0, "on"),
            w(2050.0, 2550.0, "me"),
        ],
        vec![
            w(90.0, 380.0, "Shine"),
            w(880.0, 1260.0, "light"),
            w(1520.0, 1920.0, "on"),
            w(1980.0, 2400.0, "me"),
        ],
    ]
}

#[test]
fn the_same_line_lines_up_across_the_takes() {
    let al = align(three_takes(), RATE);
    let texts: Vec<&str> = al.columns.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["Shine", "a", "light", "on", "me."]);
    // The misheard word pairs by its onset; the left-out one is missing.
    assert_eq!(al.columns[1].takes, [Some(1), Some(1), None]);
    assert_eq!(al.columns[2].takes, [Some(2), Some(2), Some(1)]);
    assert_eq!(al.columns[4].takes, [Some(4), Some(4), Some(3)]);
    assert!(al.columns[0].line);
    assert_eq!(al.lines(), vec![0..5usize]);
}

/// Words that sound alike but are far apart in time stay apart; a word
/// only one take sang gets a column of its own, in its place.
#[test]
fn a_word_only_one_take_sang_gets_its_own_column() {
    let takes = vec![
        vec![
            w(0.0, 300.0, "oh"),
            w(400.0, 700.0, "yeah"),
            w(2000.0, 2300.0, "go"),
        ],
        vec![
            w(0.0, 300.0, "oh"),
            w(420.0, 700.0, "yeah"),
            w(1100.0, 1400.0, "baby"),
            w(2030.0, 2300.0, "go"),
        ],
    ];
    let al = align(takes, RATE);
    let texts: Vec<&str> = al.columns.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["oh", "yeah", "baby", "go"]);
    assert_eq!(al.columns[2].takes, [None, Some(2)]);
}

/// A word heard differently in every take still pairs, by its onset
/// from the word before that they agree on — even when a take is sung
/// late all along.
#[test]
fn differing_words_pair_by_their_onsets_from_the_last_agreed_word() {
    let late = 300.0;
    let takes = vec![
        vec![
            w(0.0, 300.0, "hold"),
            w(500.0, 800.0, "me"),
            w(1000.0, 1400.0, "tight"),
        ],
        vec![
            w(late, late + 300.0, "hold"),
            w(late + 520.0, late + 800.0, "knee"),
            w(late + 1010.0, late + 1400.0, "tight"),
        ],
    ];
    let al = align(takes, RATE);
    assert_eq!(al.columns.len(), 3);
    assert_eq!(al.columns[1].takes, [Some(1), Some(1)]);
}

#[test]
fn lines_follow_the_words_that_start_them() {
    let mut takes = three_takes();
    takes[0][3].line = true;
    takes[1][3].line = true;
    let al = align(takes, RATE);
    assert_eq!(al.lines(), [0..3, 3..5]);
    assert_eq!(al.line(4), 3..5);
    assert_eq!(al.line(1), 0..3);
}

#[test]
fn a_comped_word_plays_whole_from_its_take() {
    let al = align(three_takes(), RATE);
    let mut f = folder(3, 3000.0);
    f.use_take(0);
    // "light" from take 2, "me" from take 3, "a" from take 2.
    for (c, take) in [(2, 1), (4, 2), (1, 1)] {
        let plan = plan_comp(&al, &f, c..c + 1, take, RATE).unwrap();
        let start = plan.start.map(|e| e.plain());
        let end = plan.end.map(|e| e.plain());
        apply_comp(&mut f, &plan, start, end);
        holds(&f);
        let (a, b) = al.span(c, take).unwrap();
        assert!(plays_whole(&f, take, a, b), "column {c}: {:?}", f.comp);
        assert_eq!(al.chosen(c, &f), Some(take));
    }
    // The words around them still play from take 1.
    for c in [0, 3] {
        assert_eq!(al.chosen(c, &f), Some(0), "column {c}");
        let (a, b) = al.span(c, 0).unwrap();
        assert!(plays_whole(&f, 0, a, b), "column {c}: {:?}", f.comp);
    }
    // "a" then "light" from take 2: one piece, no boundary between them.
    assert_eq!(f.take_at(ms(850.0)), Some(1));
}

/// Each boundary sits in the gap: after the words before (both takes')
/// and before the comped word (both takes'), the crossfade inside.
#[test]
fn boundaries_go_in_the_gaps_between_the_words() {
    let al = align(three_takes(), RATE);
    let mut f = folder(3, 3000.0);
    f.use_take(0);
    let plan = plan_comp(&al, &f, 2..3, 1, RATE).unwrap();
    let s = plan.start.unwrap();
    let e = plan.end.unwrap();
    let half = f.crossfade / 2;
    // Before: "a" ends at 800 (take 1) and 790 (take 2); "light" starts at
    // 900 (take 1) and 930 (take 2).
    assert_eq!((s.lo, s.hi), (ms(800.0) + half, ms(900.0) - half));
    assert_eq!(s.other, Some(0));
    assert_eq!(s.word, ms(930.0));
    // After: "light" ends at 1350 (take 2), 1300 (take 1); "on" starts at
    // 1480 (take 2) and 1500 (take 1).
    assert_eq!((e.lo, e.hi), (ms(1350.0) + half, ms(1480.0) - half));
    assert_eq!(e.other, Some(0));
    // Played from take 2 already on the left: no start boundary.
    let mut g = f.clone();
    g.set_comp(0, ms(930.0), Some(1));
    let plan = plan_comp(&al, &g, 2..3, 1, RATE).unwrap();
    assert!(plan.start.is_none());
    // Silence on the left: the gap is the take's own.
    let mut g = f.clone();
    g.set_comp(0, ms(930.0), None);
    let plan = plan_comp(&al, &g, 2..3, 1, RATE).unwrap();
    let s = plan.start.unwrap();
    assert_eq!(s.other, None);
    assert_eq!((s.lo, s.hi), (ms(790.0) + half, ms(930.0) - half));
}

/// Words that run into each other leave no gap: the boundary goes right
/// at the word's edge; a boundary already in a gap is used again.
#[test]
fn touching_words_and_existing_boundaries() {
    let takes = vec![
        vec![w(0.0, 500.0, "run"), w(500.0, 1000.0, "together")],
        vec![w(0.0, 520.0, "run"), w(480.0, 1000.0, "together")],
    ];
    let al = align(takes, RATE);
    let mut f = folder(2, 1500.0);
    f.use_take(0);
    let plan = plan_comp(&al, &f, 1..2, 1, RATE).unwrap();
    let s = plan.start.unwrap();
    assert_eq!((s.lo, s.hi), (ms(480.0) - f.crossfade / 2, ms(480.0)));

    let al = align(three_takes(), RATE);
    let mut f = folder(3, 3000.0);
    f.use_take(0);
    f.set_comp(ms(820.0), ms(1400.0), Some(2));
    let plan = plan_comp(&al, &f, 1..2, 1, RATE).unwrap();
    // "a"'s end gap holds the boundary at 820 ms: used again.
    assert_eq!(plan.end.unwrap().existing, Some(ms(820.0)));
    apply_comp(
        &mut f,
        &plan,
        plan.start.map(|e| e.plain()),
        plan.end.map(|e| e.plain()),
    );
    holds(&f);
    assert_eq!(f.take_at(ms(819.0)), Some(1));
    assert_eq!(f.take_at(ms(820.0)), Some(2));
}

/// A phrase: its first word's start to its last word's end, the words
/// between from the same take; a take that sings none of it changes
/// nothing.
#[test]
fn a_phrase_comps_from_its_first_word_to_its_last() {
    let al = align(three_takes(), RATE);
    let mut f = folder(3, 3000.0);
    f.use_take(0);
    let plan = plan_comp(&al, &f, 1..4, 2, RATE).unwrap();
    // Take 3 has no "a": the phrase starts at its "light".
    assert_eq!(plan.words, (ms(880.0), ms(1920.0)));
    apply_comp(
        &mut f,
        &plan,
        plan.start.map(|e| e.plain()),
        plan.end.map(|e| e.plain()),
    );
    holds(&f);
    for c in [2, 3] {
        assert_eq!(al.chosen(c, &f), Some(2));
    }
    assert!(plan_comp(&al, &f, 1..2, 2, RATE).is_none());
}

/// Words are kept by source in the project file; a project without any
/// writes no field (older readers and files stay as they were).
#[test]
fn source_words_round_trip_through_the_project_file() {
    let mut p = crate::Project::new("Words", RATE);
    let json = serde_json::to_string(&p).unwrap();
    assert!(!json.contains("source_words"));
    let mut s = SourceWords::new(RATE);
    s.language = Some("en".into());
    s.add(
        (0, 48_000),
        vec![SungWord {
            start: 100,
            end: 900,
            text: "hello".into(),
            line: true,
            confidence: 87,
        }],
    );
    p.source_words.insert(AudioSourceId(7), s);
    let back: crate::Project = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back.source_words, p.source_words);
    // The command and its inverse.
    let mut q = crate::Project::new("Words", RATE);
    let inv = crate::Command::SetSourceWords {
        source: AudioSourceId(7),
        words: Some(Box::new(p.source_words[&AudioSourceId(7)].clone())),
    }
    .apply(&mut q)
    .unwrap();
    assert_eq!(q.source_words, p.source_words);
    inv.apply(&mut q).unwrap();
    assert!(q.source_words.is_empty());
}
