//! THD over level and frequency: the bus circuit against the channel stage.
use faderframe_circuit::console::ConsoleStage;
use faderframe_circuit::dsp::measure::{run, Tone};
use faderframe_circuit::preamp::{Preamp, MODELS};

fn main() {
    let rate = 48_000.0;
    let f: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2);
    for hz in [60.0, 200.0, 1000.0, 5000.0] {
        println!("{hz} Hz");
        for db in -8..=8 {
            let tone = Tone::near(rate, 9600, hz, 10f64.powf(db as f64 / 20.0));
            let mut c = Preamp::new(MODELS + f, rate, 0.0, 0.0).unwrap();
            let mc = run(tone, 28_800, |x| c.process(x));
            let mut s = ConsoleStage::new(f, 0.0, rate);
            let ms = run(tone, 28_800, |x| s.process(x));
            println!(
                "  {db:+3} dBFS: circuit {:7.3} %  stage {:7.3} %",
                mc.thd_percent(),
                ms.thd_percent()
            );
        }
    }
}
