fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut f = smr::SonFile::open(&a[1]).unwrap();
    let pos: usize = a[2].parse().unwrap();
    let lo: u64 = a[3].parse().unwrap();
    let hi: u64 = a[4].parse().unwrap();
    let raw = f.read_adc_raw(pos, lo, (hi - lo) as usize).unwrap();
    let c = &f.channels[pos];
    let (s, o) = (c.scale as f64, c.offset as f64);
    let n = raw.len() as f64;
    let sum_raw: f64 = raw.iter().map(|&r| r as f64).sum();
    let sum_int: i64 = raw.iter().map(|&r| r as i64).sum();
    let vals: Vec<f64> = raw.iter().map(|&r| r as f64 * s / 6553.6 + o).collect();
    println!("A mean of scaled values        {:?}", vals.iter().sum::<f64>() / n);
    println!("B scaled mean of raw           {:?}", (sum_raw / n) * s / 6553.6 + o);
    println!("C (sum raw*s/6553.6)/n + o     {:?}", sum_raw * s / 6553.6 / n + o);
    println!("D sum_int/n*(s/6553.6)+o       {:?}", (sum_int as f64 / n) * (s / 6553.6) + o);
    println!("E sum(scaled w/o offset)/n + o {:?}", raw.iter().map(|&r| r as f64 * s / 6553.6).sum::<f64>() / n + o);
    let f32v: Vec<f32> = raw.iter().map(|&r| (r as f32) * (c.scale) / 6553.6 + c.offset).collect();
    println!("F f32 values summed in f64     {:?}", f32v.iter().map(|&v| v as f64).sum::<f64>() / n);
    let mut k = 0.0f64; let mut comp = 0.0f64; // Kahan
    for &v in &vals { let y = v - comp; let t = k + y; comp = (t - k) - y; k = t; }
    println!("G kahan                        {:?}", k / n);
}
