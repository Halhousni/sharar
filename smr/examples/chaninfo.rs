fn main() {
    let f = smr::SonFile::open(std::env::args().nth(1).unwrap()).unwrap();
    println!("time_factor {} duration {}", f.time_factor, f.duration());
    for (i, c) in f.comments.iter().enumerate() { println!("comment {i}: {:?}", c); }
    for c in &f.channels {
        println!("{} idx{} {:?} '{}' interval {} blocks {} items {} first {:?}", c.index + 1, c.index, c.kind, c.title, c.sample_interval,
            c.blocks.len(), c.items, c.blocks.first());
    }
}
