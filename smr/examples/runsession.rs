//! Run a script with a Session host: `runsession file.s2s [out_dir]`; the guest's `data` and
//! `out` folders are mapped to testdata/Example_Data_Files and out_dir.
use smr::script::session::Session;
use smr::script::vm::Vm;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let src = std::fs::read_to_string(&a[1]).unwrap();
    let c = smr::script::compile_with_commands(&src).unwrap();
    let mut s = Session::new();
    s.map_path(r"C:\Users\Hilal\Documents\Spike8\data\", "testdata/Example_Data_Files");
    let out = a.get(2).cloned().unwrap_or("/tmp/sharar_out".into());
    std::fs::create_dir_all(&out).unwrap();
    s.map_path(r"C:\Users\Hilal\Documents\Spike8\out\", out);
    let mut vm = Vm::new(c, true).unwrap().with_host(Box::new(s));
    let stop = vm.run();
    print!("{}", vm.log);
    println!("stopped: {stop:?}");
    if let Some(res) = vm.array("res") {
        println!("res = {:?}", &res[..40.min(res.len())]);
    }
}
