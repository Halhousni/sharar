fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut f = smr::SonFile::open(&a[1]).unwrap();
    let pos: usize = a[2].parse().unwrap();
    let first: u64 = a[3].parse().unwrap();
    let raw = f.read_adc_raw(pos, first, 8).unwrap();
    let c = &f.channels[pos];
    println!("scale {:e} offset {:e}", c.scale as f64, c.offset as f64);
    println!("{:?}", raw);
}
