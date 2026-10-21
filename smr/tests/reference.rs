//! Ground-truth comparison against the reference reader output.

use std::path::Path;

use smr::SonFile;

/// The example recordings come from the retail Spike8 CD and are not redistributed with
/// this repository. Tests that need them say so and pass when they are absent.
fn sample(path: &str) -> Option<String> {
    if Path::new(path).exists() {
        Some(path.to_string())
    } else {
        eprintln!("skipping: {path} not present (extract the Spike8 CD example data)");
        None
    }
}

#[test]
fn emg_waveform_matches_reference() {
    let Some(path) = sample("../testdata/Example_Data_Files/EMG Waveform.smr") else { return };
    let mut f = SonFile::open(&path).unwrap();
    assert_eq!(f.channels.len(), 2, "one ADC channel plus one event channel");

    let emg = &f.channels[0];
    assert_eq!(emg.title, "EMG");
    assert_eq!(emg.kind, smr::Kind::Adc);
    assert_eq!(emg.units, "mVolt");
    assert!((emg.sample_rate() - 1000.0).abs() < 1e-6, "rate {}", emg.sample_rate());

    // neo reports gain scale/6553.6 = 0.0006853544618934393, i.e. scale = 4.4915...
    assert!((emg.scale - 0.00068535446 * 6553.6).abs() < 1e-3);

    let data = f.read_waveform(0, 0, 8).unwrap();
    assert_eq!(data.len(), 8);
    assert!(data.iter().all(|v| v.is_finite()));

    let mem = &f.channels[1];
    assert_eq!(mem.title, "Memory");
    let events = f.read_events(1).unwrap();
    // The block declares 12 items and the payload holds 12 tick/marker pairs at
    // 6-second intervals. neo reports only the first of them; the file is authoritative.
    assert_eq!(events.len(), 12);
    assert!((events[0].time - 0.00105).abs() < 1e-9, "got {}", events[0].time);
    assert!((events[11].time - 65.99049).abs() < 1e-6, "got {}", events[11].time);
    assert!(events.windows(2).all(|w| w[1].time > w[0].time));
}

#[test]
fn demo_matches_reference() {
    let Some(path) = sample("../testdata/Base_Demo/Demo.smr") else { return };
    let mut f = SonFile::open(&path).unwrap();
    assert_eq!(f.channels.len(), 4, "one waveform, two event channels, one marker channel");

    let adc: Vec<_> = f.channels.iter().filter(|c| c.kind.is_waveform()).collect();
    assert_eq!(adc.len(), 1);
    let sine = adc[0];
    assert_eq!(sine.title, "Sinewave");
    assert_eq!(sine.units, "Volts");
    assert!((sine.sample_rate() - 100.0).abs() < 1e-9, "rate {}", sine.sample_rate());

    let ev: Vec<_> = f.channels.iter().filter(|c| c.kind.is_event()).collect();
    assert_eq!(ev.len(), 3, "Stimulus, Response, Keyboard");

    let titles: Vec<&str> = ev.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, vec!["Stimulus", "Response", "Keyboard"]);

    let counts: Vec<u64> = ev.iter().map(|c| c.items).collect();
    assert_eq!(counts, vec![752, 1044, 38]);

    // Keyboard markers carry ASCII codes; 't' is the first.
    let kb_index = f.channels.iter().position(|c| c.title == "Keyboard").unwrap();
    let kb = f.read_events(kb_index).unwrap();
    assert_eq!(kb.len(), 38);
    assert_eq!(kb[0].code, Some(116));
    assert!((kb[0].time - 2.55488).abs() < 1e-5, "got {}", kb[0].time);
}

#[test]
fn duration_matches_last_block() {
    let Some(path) = sample("../testdata/Base_Demo/Demo.smr") else { return };
    let f = SonFile::open(&path).unwrap();
    let d = f.duration();
    assert!((d - 120.921).abs() < 0.001, "duration {d}");
}

#[test]
fn wavemarks_and_text_marks_load() {
    let Some(path) = sample("../testdata/Example_Data_Files/Extracellular Spikes.smr") else {
        return;
    };
    let mut f = SonFile::open(&path).unwrap();
    assert_eq!(f.channels.len(), 2);

    let spikes = &f.channels[0];
    assert_eq!(spikes.kind, smr::Kind::AdcMark);
    assert_eq!(spikes.title, "Spikes");
    // Five blocks: four of 150 events and a final partial block of 46.
    assert_eq!(spikes.items, 646);
    assert_eq!(spikes.item_bytes(), 8 + 200);

    let marks = f.read_wavemarks(0).unwrap();
    assert_eq!(marks.len(), 646);
    assert_eq!(marks[0].samples.len(), 100, "200 bytes of int16 is 100 points");
    assert!(marks[0].samples.iter().all(|v| v.is_finite()));
    assert!(marks.windows(2).all(|w| w[1].time >= w[0].time));

    let drug = &f.channels[1];
    assert_eq!(drug.kind, smr::Kind::TextMark);
    assert_eq!(drug.item_bytes(), 8 + 36);
    let texts = f.read_events(1).unwrap();
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0].code, Some(6));
    let label = texts[0].text.as_deref().unwrap_or("");
    assert!(label.starts_with("Drug"), "label was {label:?}");
}
