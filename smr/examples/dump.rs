//! Print a recording's channels and, optionally, raw samples or events, for checking the
//! reader against another implementation. `dump FILE [CHANNEL [FIRST COUNT]]`.
use smr::SonFile;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut f = SonFile::open(&args[0]).expect("open");
    println!("son64={} tick={:e} duration={:.6}", f.son64, f.time_factor, f.duration());
    let Some(ch) = args.get(1).and_then(|a| a.parse::<usize>().ok()) else {
        for (i, c) in f.channels.iter().enumerate() {
            println!(
                "{i}\t{}\t{:?}\t{}\t{}\t{}\t{:e}\t{}\t{}",
                c.index, c.kind, c.title, c.units, c.items, c.sample_interval, c.scale, c.offset
            );
        }
        return;
    };
    let kind = f.channels[ch].kind;
    if kind.is_waveform() {
        let first = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0);
        let count = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(10);
        for v in f.read_waveform(ch, first, count).unwrap() {
            println!("{v}");
        }
    } else if kind.is_wavemark() {
        for m in f.read_wavemarks(ch).unwrap() {
            println!("{:.9}\t{}\t{}", m.time, m.code, m.samples.len());
        }
    } else {
        for e in f.read_events(ch).unwrap() {
            println!("{:.9}\t{:?}\t{:?}", e.time, e.code, e.text);
        }
    }
}
