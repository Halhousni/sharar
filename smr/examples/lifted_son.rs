//! Open a data file with Spike2's own translated file engine and ask it a few things.
use s2lift::commands;
use smr::script::vm::lifted::Bare;

fn main() {
    let path = std::env::args().nth(1).expect("a .smr file");
    let mut m = commands::machine();
    let mut host = Bare::default();
    let obj = m.alloc(0x20000);
    let p = m.alloc(2 * path.len() + 2);
    for (i, u) in path.encode_utf16().enumerate() {
        m.st(p + 2 * i as u64, 2, u as u64);
    }
    m.r[1] = obj;
    m.r[2] = p;
    host.call(&mut m, "??0CSonFile@@QEAA@PEB_W@Z");
    println!("constructed, rax={:#x}", m.r[0]);
    m.r[1] = obj;
    host.call(&mut m, "?GetFileBytes@CSonFile@@QEAA_KXZ");
    println!("file bytes {}", m.r[0]);
    m.r[1] = obj;
    host.call(&mut m, "?GetTimeBase@CSonFile@@QEBANXZ");
    println!("time base {}", f64::from_bits(m.xl(0)));
}
