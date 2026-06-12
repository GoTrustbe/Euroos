//! Boot-zelftest voor **EuroJS** (AB-B5): de JavaScript-engine van EuroWeb.
//! Voert echte JS uit in de kernel — recursie, closures, arrays, console.log.
//! Kern: [`eurojs`].

use crate::serial_println;
use eurojs::{as_num, eval, run_capture};

pub fn selftest() {
    // Recursie: factorial.
    let fact = eval("function f(n){ if(n<=1) return 1; return n*f(n-1); } f(6)")
        .ok()
        .and_then(|v| as_num(&v))
        .map(|n| n == 720.0)
        .unwrap_or(false);

    // Closure.
    let closure = eval("function adder(x){return y=>x+y;} adder(40)(2)")
        .ok()
        .and_then(|v| as_num(&v))
        .map(|n| n == 42.0)
        .unwrap_or(false);

    // Array + lus + objecten in één programma.
    let prog = eval(
        "function sumEven(a){let t=0;for(let i=0;i<a.length;i++){if(a[i]%2===0)t=t+a[i];}return t;} sumEven([1,2,3,4,5,6,7,8,9,10])",
    )
    .ok()
    .and_then(|v| as_num(&v))
    .map(|n| n == 30.0)
    .unwrap_or(false);

    // Math + console.log-uitvoer.
    let (_r, out) = run_capture("console.log('EuroJS', Math.pow(2,10));");
    let console = out == alloc::vec![alloc::string::String::from("EuroJS 1024")];

    let ok = fact && closure && prog && console;
    serial_println!(
        "[js] EuroJS: factorial(6)=720={}, closure=42={}, array+lus+object(sumEven)=30={}, console.log+Math={} {}",
        fact, closure, prog, console,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
