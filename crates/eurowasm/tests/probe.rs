// Probe: can eurowasm run a Rust-compiled wasm32 module? Reports the first failure.
use eurowasm::{HostImports, Instance, Module, Val, WasmError};
struct H(Vec<(String, Vec<Val>)>);
impl HostImports for H {
    fn call(&mut self, m: &str, n: &str, args: &[Val], _mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        self.0.push((format!("{m}.{n}"), args.to_vec()));
        Ok(vec![0])
    }
}
#[test]
fn probe() {
    let Ok(path) = std::env::var("EURO_WASM") else { return };
    let bytes = std::fs::read(path).unwrap();
    let m = match Module::parse(&bytes) { Ok(m) => m, Err(e) => { println!("PARSE: {e:?}"); return } };
    let mut inst = Instance::new(&m);
    let mut h = H(Vec::new());
    println!("init -> {:?}", inst.invoke("init", &[], &mut h));
    println!("frame -> {:?}", inst.invoke("frame", &[], &mut h));
    println!("host calls: {}", h.0.len());
    for (n, a) in h.0.iter().take(6) { println!("  {n} {a:?}"); }
}
