//! SON64 (`.smrx`) reader tests against a fixture this crate writes itself.
//!
//! The layout follows `smr/src/son64.rs`. The fixture has one ADC channel split over two
//! data blocks under a level-1 index, one rising-event channel, one deleted slot, and a
//! left-over data block from an earlier generation of the ADC slot that must be ignored.

use std::fs;

use smr::{Kind, SonFile};

const BLOCK: usize = 65_536;
const CHAN_TABLE: usize = 2_048;
const RECORD: usize = 272;
const SLOTS: usize = 4;
const STR_TABLE: usize = CHAN_TABLE + SLOTS * RECORD;
const TICK: f64 = 1e-5;
const DIVIDE: u64 = 100; // 1 kHz

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}
fn p16(buf: &mut [u8], at: usize, v: u16) {
    put(buf, at, &v.to_le_bytes());
}
fn p32(buf: &mut [u8], at: usize, v: u32) {
    put(buf, at, &v.to_le_bytes());
}
fn p64(buf: &mut [u8], at: usize, v: u64) {
    put(buf, at, &v.to_le_bytes());
}
fn pf64(buf: &mut [u8], at: usize, v: f64) {
    put(buf, at, &v.to_le_bytes());
}

fn block_header(buf: &mut [u8], at: usize, level: u64, channel: u16, generation: u16, count: u32) {
    p64(buf, at, level << 8);
    p16(buf, at + 8, channel);
    p16(buf, at + 10, generation);
    p32(buf, at + 12, count);
}

fn fixture() -> Vec<u8> {
    let mut f = vec![0u8; BLOCK * 8];
    put(&mut f, 0, b"S64pl\0");
    put(&mut f, 16, b"S2091349");
    pf64(&mut f, 32, TICK);
    p32(&mut f, 44, CHAN_TABLE as u32);
    p32(&mut f, 52, STR_TABLE as u32);
    p32(&mut f, 56, SLOTS as u32);
    p32(&mut f, 60, RECORD as u32);
    p32(&mut f, 64, 4); // file comment: string 4

    // Strings, 1-based: each has a reference count, text, NUL, padded to 4 bytes.
    let strings = ["mV", "Wave", "a comment", "made by test", "Stim"];
    let mut at = STR_TABLE + 8;
    for s in strings {
        p32(&mut f, at, 1);
        put(&mut f, at + 4, s.as_bytes());
        at += 4 + (s.len() + 1 + 3) / 4 * 4;
    }
    p32(&mut f, STR_TABLE, (at - STR_TABLE) as u32);
    p32(&mut f, STR_TABLE + 4, strings.len() as u32);

    // Slot 0: ADC, generation 1. Root index at block 1; data in blocks 2 and 3.
    // Block 4 is a left-over of generation 0 and carries the wrong generation.
    let c = CHAN_TABLE;
    p64(&mut f, c, BLOCK as u64);
    p32(&mut f, c + 0x20, 2);
    p16(&mut f, c + 0x2c, 1);
    f[c + 0x2e] = 1;
    p32(&mut f, c + 0x34, 2); // title "Wave"
    p32(&mut f, c + 0x38, 1); // units "mV"
    p32(&mut f, c + 0x3c, 3); // comment
    p64(&mut f, c + 0x40, DIVIDE);
    pf64(&mut f, c + 0x48, 1000.0);
    pf64(&mut f, c + 0x50, 2.0); // scale
    pf64(&mut f, c + 0x58, 0.5); // offset

    // Slot 1: deleted (kind 0 but with a stale title).
    p32(&mut f, CHAN_TABLE + RECORD + 0x34, 2);

    // Slot 2: rising events. Root index at block 5, data in block 6.
    let c = CHAN_TABLE + 2 * RECORD;
    p64(&mut f, c, 5 * BLOCK as u64);
    p32(&mut f, c + 0x20, 8);
    f[c + 0x2e] = 3;
    p32(&mut f, c + 0x34, 5); // title "Stim"

    // ADC index: two entries.
    block_header(&mut f, BLOCK, 1, 0, 1, 2);
    p64(&mut f, BLOCK + 16, 0);
    p64(&mut f, BLOCK + 24, 2 * BLOCK as u64);
    p64(&mut f, BLOCK + 32, 50 * DIVIDE);
    p64(&mut f, BLOCK + 40, 3 * BLOCK as u64);

    // ADC data: counts 0..49 then 100..149, one run per block.
    for (blk, first_tick, base) in [(2usize, 0u64, 0i16), (3, 50 * DIVIDE, 100)] {
        let at = blk * BLOCK;
        block_header(&mut f, at, 0, 0, 1, 1);
        p64(&mut f, at + 16, first_tick);
        p64(&mut f, at + 24, 50);
        for i in 0..50 {
            put(&mut f, at + 32 + 2 * i, &(base + i as i16).to_le_bytes());
        }
    }
    // Stale block, generation 0: would add 20 samples if it were read.
    block_header(&mut f, 4 * BLOCK, 0, 0, 0, 1);
    p64(&mut f, 4 * BLOCK + 16, 0);
    p64(&mut f, 4 * BLOCK + 24, 20);

    // Event index and data: three events at 0.01, 0.02 and 0.5 s.
    block_header(&mut f, 5 * BLOCK, 1, 2, 0, 1);
    p64(&mut f, 5 * BLOCK + 16, 1_000);
    p64(&mut f, 5 * BLOCK + 24, 6 * BLOCK as u64);
    block_header(&mut f, 6 * BLOCK, 0, 2, 0, 3);
    for (i, t) in [1_000u64, 2_000, 50_000].into_iter().enumerate() {
        p64(&mut f, 6 * BLOCK + 16 + 8 * i, t);
    }
    f
}

#[test]
fn smrx_channels_and_data_read_back() {
    let path = std::env::temp_dir().join(format!("sharar_synth_{}.smrx", std::process::id()));
    fs::write(&path, fixture()).unwrap();
    let mut son = SonFile::open(&path).unwrap();
    fs::remove_file(&path).ok();

    assert!(son.son64);
    assert_eq!(son.creator, "S2091349");
    assert_eq!(son.comments, vec!["made by test".to_string()]);
    assert!((son.time_factor - TICK).abs() < 1e-18);
    assert_eq!(son.channels.len(), 2, "deleted slot is skipped");

    let adc = &son.channels[0];
    assert_eq!(adc.kind, Kind::Adc);
    assert_eq!(adc.title, "Wave");
    assert_eq!(adc.units, "mV");
    assert_eq!(adc.comment, "a comment");
    assert_eq!(adc.items, 100, "the stale-generation block is ignored");
    assert!((adc.sample_rate() - 1000.0).abs() < 1e-9);

    // 2 units per volt over +-5 V: count * 2 / 6553.6 + 0.5.
    let v = son.read_waveform(0, 48, 4).unwrap();
    let expect = |c: f32| c * 2.0 / 6553.6 + 0.5;
    assert_eq!(v, vec![expect(48.0), expect(49.0), expect(100.0), expect(101.0)]);

    let past = son.read_waveform(0, 98, 4).unwrap();
    assert!(past[2].is_nan() && past[3].is_nan());
    // The ADC ends at tick 5000 + 49 * 100 = 9900 (0.099 s). The last event, at 0.5 s, is
    // later, and the duration is the latest time on any channel.
    let adc_end = son.channels[0].blocks.last().unwrap().end_tick;
    assert_eq!(adc_end, 9_900);
    assert!((son.duration() - 0.5).abs() < 1e-12, "duration {}", son.duration());

    let ev = son.read_events(1).unwrap();
    assert_eq!(son.channels[1].kind, Kind::EventRise);
    let times: Vec<f64> = ev.iter().map(|e| e.time).collect();
    assert_eq!(times.len(), 3);
    for (got, want) in times.iter().zip([0.01, 0.02, 0.5]) {
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
    }
}

#[test]
fn truncated_smrx_is_an_error_not_a_panic() {
    let full = fixture();
    for cut in [0usize, 5, 100, 1_500, 2_100, 3_000] {
        let path = std::env::temp_dir().join(format!("sharar_cut_{}_{cut}.smrx", std::process::id()));
        fs::write(&path, &full[..cut]).unwrap();
        let result = std::panic::catch_unwind(|| SonFile::open(&path).map(|_| ()));
        fs::remove_file(&path).ok();
        assert!(result.is_ok(), "open panicked on a file cut at {cut} bytes");
    }
}
