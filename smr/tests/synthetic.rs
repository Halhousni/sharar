//! Parser tests against a fixture this crate writes itself.
//!
//! The reference tests in `reference.rs` compare against CED's example recordings, which are
//! not redistributable, so they skip on a machine without the Spike8 CD. These tests build a
//! small SON file in a temporary directory and run everywhere, which is what CI needs.

use std::fs;
use std::path::PathBuf;

use smr::SonFile;

const HEADER: usize = 512;
const CHAN: usize = 140;
const BLOCK: usize = 20;

/// A minimal SON file: one 16-bit ADC channel of two blocks, plus one marker channel.
///
/// Time base is 10 us per tick (`time_factor = 1e-5`), the ADC runs at 1 kHz
/// (`l_chan_dvd = 100` ticks per sample), the channel scale is 4.491539 units per volt with a
/// small offset, and the marker channel holds two markers at 0 s and 0.1 s.
struct Fixture {
    adc_counts: Vec<i16>,
    name: String,
}

fn put_i16(buf: &mut [u8], at: usize, v: i16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_i32(buf: &mut [u8], at: usize, v: i32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(buf: &mut [u8], at: usize, v: f32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f64(buf: &mut [u8], at: usize, v: f64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Pascal string: length byte first, then the characters, padded to `width` bytes.
fn put_pstr(buf: &mut [u8], at: usize, width: usize, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(width - 1);
    buf[at] = n as u8;
    buf[at + 1..at + 1 + n].copy_from_slice(&bytes[..n]);
}

fn write_adc_header(buf: &mut [u8], at: usize, title: &str, firstblock: i32, blocks: i16) {
    put_i32(buf, at + 6, firstblock);
    put_i16(buf, at + 14, blocks);
    put_i16(buf, at + 24, 0);
    put_pstr(buf, at + 26, 72, "fixture");
    put_i32(buf, at + 102, 100); // l_chan_dvd: 100 ticks per sample = 1 kHz
    put_pstr(buf, at + 108, 10, title);
    put_f32(buf, at + 118, 1000.0); // ideal rate, informational
    buf[at + 122] = 1; // Kind::Adc
    put_f32(buf, at + 124, 4.491539); // scale, units per volt
    put_f32(buf, at + 128, -0.000346_285); // offset
    put_pstr(buf, at + 132, 6, "mVolt");
}

fn write_marker_header(buf: &mut [u8], at: usize, title: &str, firstblock: i32, blocks: i16) {
    put_i32(buf, at + 6, firstblock);
    put_i16(buf, at + 14, blocks);
    put_pstr(buf, at + 26, 72, "marks");
    put_pstr(buf, at + 108, 10, title);
    buf[at + 122] = 5; // Kind::Marker
}

fn write_block(
    buf: &mut [u8],
    at: usize,
    succ: i32,
    start_tick: i32,
    end_tick: i32,
    chan: i16,
    items: i16,
) {
    put_i32(buf, at + 4, succ);
    put_i32(buf, at + 8, start_tick);
    put_i32(buf, at + 12, end_tick);
    put_i16(buf, at + 16, chan);
    put_i16(buf, at + 18, items);
}

impl Fixture {
    fn path(&self) -> PathBuf {
        std::env::temp_dir().join(&self.name)
    }

    /// Each test gets its own file: they run in parallel, and a shared path would let one
    /// test delete the file another is still reading.
    fn build(name: &str) -> Self {
        // 10 samples per block, 2 ADC blocks, then 1 marker block with 2 markers.
        let adc_counts: Vec<i16> = vec![
            32767, -32768, 16384, -16384, 0, 8192, -8192, 4096, -4096, 1000, // block 0
            2000, -2000, 3000, -3000, 4000, -4000, 5000, -5000, 6000, -6000, // block 1
        ];

        let adc0 = 1024;
        let adc0_data = adc0 + BLOCK;
        let adc1 = adc0_data + 10 * 2;
        let adc1_data = adc1 + BLOCK;
        let mark = adc1_data + 10 * 2;
        let mark_data = mark + BLOCK;
        let end = mark_data + 2 * 8;

        let mut buf = vec![0u8; end];

        // File header. system_id 6 means the time base is read from the file.
        put_i16(&mut buf, 0, 6);
        put_i16(&mut buf, 20, 1); // us_per_time
        put_i16(&mut buf, 30, 2); // number of channel headers
        put_i16(&mut buf, 32, CHAN as i16);
        put_f64(&mut buf, 44, 1e-5); // seconds per tick

        // Channel headers.
        write_adc_header(&mut buf, HEADER, "EMG", adc0 as i32, 2);
        write_marker_header(&mut buf, HEADER + CHAN, "Memory", mark as i32, 1);

        // ADC blocks: 10 samples of 100 ticks each, second block starting at tick 1000.
        write_block(&mut buf, adc0, adc1 as i32, 0, 900, 0, 10);
        for (i, c) in adc_counts[..10].iter().enumerate() {
            put_i16(&mut buf, adc0_data + i * 2, *c);
        }
        write_block(&mut buf, adc1, 0, 1000, 1900, 0, 10);
        for (i, c) in adc_counts[10..].iter().enumerate() {
            put_i16(&mut buf, adc1_data + i * 2, *c);
        }

        // Marker block: tick 0 and tick 10000 (0.1 s), codes 1 and 2.
        write_block(&mut buf, mark, 0, 0, 10000, 1, 2);
        put_i32(&mut buf, mark_data, 0);
        put_i32(&mut buf, mark_data + 4, 1);
        put_i32(&mut buf, mark_data + 8, 10000);
        put_i32(&mut buf, mark_data + 12, 2);

        let fx = Fixture {
            adc_counts,
            name: name.to_string(),
        };
        fs::write(fx.path(), &buf).unwrap();
        fx
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.path());
    }
}

/// CED's conversion: `count * scale / 6553.6 + offset`, where scale is units per volt and a
/// 16-bit ADC spans +/-5 V. The divisor is not 65536; using that under-reports by ten.
#[test]
fn adc_counts_convert_with_ced_scaling() {
    let fx = Fixture::build("sharar_fixture_scaling.smr");
    let mut f = SonFile::open(fx.path()).unwrap();
    let ch = &f.channels[0];
    assert_eq!(ch.units, "mVolt");
    assert!((ch.scale - 4.491539).abs() < 1e-6);

    let data = f.read_waveform(0, 0, fx.adc_counts.len()).unwrap();
    for (i, count) in fx.adc_counts.iter().enumerate() {
        let want = f32::from(*count) * 4.491_539_f32 / 6553.6_f32 - 0.000_346_285;
        assert!(
            (data[i] - want).abs() < 1e-4,
            "sample {i}: got {}, want {want}",
            data[i]
        );
    }

    // Full scale: 32767 counts is very nearly scale * 5, i.e. +/-22.46 units. With the old
    // divisor it came out at 2.246, a factor of ten low.
    let full: f64 = 32767.0 * 4.491_539 / 6553.6;
    assert!((full - 22.4577).abs() < 1e-3, "full scale {full}");
    assert!((data[0] as f64 - full).abs() < 1e-3);
}

/// `sample_at` must land on the right sample anywhere inside a block, not just at the first.
/// It used to divide tick offsets by a sample interval measured in seconds, which saturated
/// the result and returned the end of the first block for every later time.
#[test]
fn sample_at_lands_inside_the_block() {
    let fx = Fixture::build("sharar_fixture_sample_at.smr");
    let mut f = SonFile::open(fx.path()).unwrap();
    assert!((f.channels[0].sample_rate() - 1000.0).abs() < 1e-9);

    for (seconds, want) in [
        (0.0, 0u64),
        (0.003, 3),
        (0.009, 9),
        (0.010, 10),
        (0.015, 15),
        (0.019, 19),
    ] {
        let got = f.sample_at(0, seconds);
        assert_eq!(got, want, "t={seconds}s gave index {got}, want {want}");
    }

    // The value at 0.015 s is the sixth sample of the second block.
    let idx = f.sample_at(0, 0.015);
    let v = f.read_waveform(0, idx, 1).unwrap()[0];
    let want: f32 = -4000.0 * 4.491_539 / 6553.6 - 0.000_346_285;
    assert!((v - want).abs() < 1e-3, "got {v}, want {want}");
}

/// Marker channels carry tick/code pairs; both markers in the fixture must come back in time
/// order with their codes.
#[test]
fn markers_read_with_codes() {
    let fx = Fixture::build("sharar_fixture_markers.smr");
    let mut f = SonFile::open(fx.path()).unwrap();
    assert_eq!(f.channels[1].title, "Memory");
    let events = f.read_events(1).unwrap();
    assert_eq!(events.len(), 2);
    assert!((events[0].time - 0.0).abs() < 1e-9);
    assert_eq!(events[0].code, Some(1));
    assert!((events[1].time - 0.1).abs() < 1e-9);
    assert_eq!(events[1].code, Some(2));
}
