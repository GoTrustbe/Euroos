//! Boot self-test for **EuroJS** (AB-B5): the JavaScript engine of EuroWeb.
//! Runs real JS in the kernel — recursion, closures, arrays, console.log.
//! Core: [`eurojs`].

use crate::serial_println;
use eurojs::{as_num, eval, run_capture};

pub fn selftest() {
    // Recursion: factorial.
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

    // Array + loop + objects in one program.
    let prog = eval(
        "function sumEven(a){let t=0;for(let i=0;i<a.length;i++){if(a[i]%2===0)t=t+a[i];}return t;} sumEven([1,2,3,4,5,6,7,8,9,10])",
    )
    .ok()
    .and_then(|v| as_num(&v))
    .map(|n| n == 30.0)
    .unwrap_or(false);

    // Math + console.log output.
    let (_r, out) = run_capture("console.log('EuroJS', Math.pow(2,10));");
    let console = out == alloc::vec![alloc::string::String::from("EuroJS 1024")];

    let ok = fact && closure && prog && console;
    serial_println!(
        "[js] EuroJS: factorial(6)=720={}, closure=42={}, array+loop+object(sumEven)=30={}, console.log+Math={} {}",
        fact, closure, prog, console,
        if ok { "✓" } else { "✗ FAIL" }
    );
}
