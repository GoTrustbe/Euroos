//! EuroJS — the JavaScript engine of EuroWeb (Sprint AB-B5).
//!
//! A **tree-walking interpreter** — deliberately no JIT, because that would add a large
//! attack surface. Supports a real JS subset: numbers, strings,
//! booleans, `null`/`undefined`, `let`/`var`/`const`, all common operators,
//! `if`/`while`/`for`, functions (declarations, expressions, arrows) with **closures**,
//! arrays and objects with methods, `Math.*`, and `console.log`. Per tab an
//! interpreter in the browser gets a limited capability set (see EUROBROWSER-PLAN);
//! this crate is the pure, host-tested compute core. `no_std`, no `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod ast;
mod interp;
mod lexer;
mod parser;

use alloc::string::String;
use alloc::vec::Vec;

pub use interp::{Interp, Value};

/// Parse + execute JS source code; returns the value of the last expression.
pub fn eval(src: &str) -> Result<Value, String> {
    let toks = lexer::lex(src)?;
    let prog = parser::Parser::new(toks).parse_program()?;
    Interp::new().run(&prog)
}

/// Execute JS and return (result, console.log output).
pub fn run_capture(src: &str) -> (Result<Value, String>, Vec<String>) {
    let toks = match lexer::lex(src) {
        Ok(t) => t,
        Err(e) => return (Err(e), Vec::new()),
    };
    let prog = match parser::Parser::new(toks).parse_program() {
        Ok(p) => p,
        Err(e) => return (Err(e), Vec::new()),
    };
    let mut it = Interp::new();
    let r = it.run(&prog);
    (r, it.output)
}

/// Execute a page script and return (result, console.log output,
/// `document.write` output) — for the EuroWeb integration: the writes are
/// appended as text to the rendered page.
pub fn run_page(src: &str) -> (Result<Value, String>, Vec<String>, Vec<String>) {
    let toks = match lexer::lex(src) {
        Ok(t) => t,
        Err(e) => return (Err(e), Vec::new(), Vec::new()),
    };
    let prog = match parser::Parser::new(toks).parse_program() {
        Ok(p) => p,
        Err(e) => return (Err(e), Vec::new(), Vec::new()),
    };
    let mut it = Interp::new();
    let r = it.run(&prog);
    (r, it.output, it.writes)
}

/// Helper: extract a numeric result (for tests/integration).
pub fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn document_write_and_console_captured() {
        let (_r, logs, writes) =
            run_page("console.log('hi'); document.write('Sum: ' + (6*7)); document.writeln('!');");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0], "hi");
        assert_eq!(writes.join(""), "Sum: 42!\n");
    }
}

/// Helper: a string result.
pub fn as_str(v: &Value) -> Option<alloc::string::String> {
    match v {
        Value::Str(s) => Some((**s).clone()),
        _ => None,
    }
}

// ── small no_std math core for Math.* (no libm) ──

pub(crate) fn flr(x: f64) -> f64 {
    let t = x as i64 as f64;
    if x < 0.0 && t != x {
        t - 1.0
    } else {
        t
    }
}

pub(crate) fn sqrt(x: f64) -> f64 {
    if x < 0.0 || x.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    let mut g = x;
    let mut i = 0;
    while i < 60 {
        let ng = 0.5 * (g + x / g);
        let d = ng - g;
        if (if d < 0.0 { -d } else { d }) <= 1e-15 * ng {
            return ng;
        }
        g = ng;
        i += 1;
    }
    g
}

pub(crate) fn powi(base: f64, exp: f64) -> f64 {
    // Integer exponent → repeated multiplication; otherwise approximation.
    if exp == flr(exp) && (if exp < 0.0 { -exp } else { exp }) < 1024.0 {
        let mut n = exp as i64;
        let neg = n < 0;
        if neg {
            n = -n;
        }
        let mut b = base;
        let mut r = 1.0;
        while n > 0 {
            if n & 1 == 1 {
                r *= b;
            }
            b *= b;
            n >>= 1;
        }
        return if neg { 1.0 / r } else { r };
    }
    f64::NAN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(src: &str) -> f64 {
        as_num(&eval(src).unwrap()).unwrap()
    }
    fn string(src: &str) -> String {
        as_str(&eval(src).unwrap()).unwrap()
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(num("1 + 2 * 3"), 7.0);
        assert_eq!(num("(1 + 2) * 3"), 9.0);
        assert_eq!(num("10 % 3"), 1.0);
        assert_eq!(num("2 + 3 < 6 ? 100 : 200"), 100.0);
    }

    #[test]
    fn variables_and_assignment() {
        assert_eq!(num("let x = 5; x * x"), 25.0);
        assert_eq!(num("let a = 1; a = a + 9; a"), 10.0);
        assert_eq!(num("let x = 3; x++; x++; x"), 5.0);
    }

    #[test]
    fn strings_concat_and_methods() {
        assert_eq!(string("'Euro' + 'OS'"), "EuroOS");
        assert_eq!(string("'euroos'.toUpperCase()"), "EUROOS");
        assert_eq!(num("'EuroOS'.length"), 6.0);
        assert!(matches!(eval("'EuroOS'.includes('OS')").unwrap(), Value::Bool(true)));
    }

    #[test]
    fn control_flow() {
        assert_eq!(num("let s = 0; for (let i = 1; i <= 5; i++) { s = s + i; } s"), 15.0);
        assert_eq!(num("let n = 0; while (n < 10) { n = n + 2; } n"), 10.0);
        assert_eq!(num("let x = 7; if (x > 5) { x = 1; } else { x = 0; } x"), 1.0);
    }

    #[test]
    fn functions_and_recursion() {
        let src = "function fact(n) { if (n <= 1) return 1; return n * fact(n - 1); } fact(5)";
        assert_eq!(num(src), 120.0);
        let fib = "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2);} fib(10)";
        assert_eq!(num(fib), 55.0);
    }

    #[test]
    fn closures() {
        let src = "function adder(x){ return function(y){ return x + y; }; } let add5 = adder(5); add5(3)";
        assert_eq!(num(src), 8.0);
    }

    #[test]
    fn arrow_functions() {
        assert_eq!(num("let sq = x => x * x; sq(9)"), 81.0);
        assert_eq!(num("let add = (a, b) => a + b; add(4, 38)"), 42.0);
    }

    // ---- Robustness / stability: untrusted scripts ----
    //
    // Page JS runs in the kernel. A script MUST NOT take down the OS:
    // it must abort cleanly with Err (no hang, no stack overflow),
    // while legitimate programs keep working.

    #[test]
    fn robust_infinite_while_is_bounded() {
        // Must not hang forever: the step budget aborts with Err.
        let r = eval("while (true) { }");
        assert!(r.is_err(), "infinite while must abort (step budget)");
    }

    #[test]
    fn robust_infinite_for_is_bounded() {
        let r = eval("for (;;) { let x = 1; }");
        assert!(r.is_err(), "infinite for must abort (step budget)");
    }

    /// Run JS on a generous stack so the TEST itself never overflows; here we
    /// test the depth-LIMIT logic (does it return Err cleanly?), not the
    /// stack size of the test thread. In the kernel the 16 KiB guarded
    /// task stack is the hard guardrail; this limit is the soft, clean layer.
    fn eval_on_big_stack(src: &'static str) -> Result<Value, String> {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || eval(src).map(|_| ()).map_err(|e| e))
            .unwrap()
            .join()
            .unwrap()
            .map(|_| Value::Undefined)
    }

    #[test]
    fn robust_infinite_recursion_no_overflow() {
        // Infinite recursion would blow the kernel stack → must return Err cleanly.
        let r = eval_on_big_stack("function f(){ return f(); } f()");
        assert!(r.is_err(), "infinite recursion must abort (depth limit)");
        let mutual = "function a(){return b();} function b(){return a();} a()";
        assert!(eval_on_big_stack(mutual).is_err(), "mutual recursion must abort");
    }

    #[test]
    fn robust_legit_deep_recursion_still_works() {
        // A real, bounded recursion (depth 150 < limit) must just work —
        // the limit must not break legitimate use.
        let src = "function sum(n){ if(n==0) return 0; return n + sum(n-1); } sum(150)";
        let r = eval_on_big_stack(src);
        assert!(r.is_ok(), "legitimate recursion of depth 150 must succeed");
    }

    #[test]
    fn robust_legit_long_loop_still_works() {
        // A long-but-finite loop (within budget) must run through correctly.
        let src = "let s = 0; for (let i = 0; i < 100000; i++) { s = s + 1; } s";
        assert_eq!(num(src), 100000.0);
    }

    #[test]
    fn arrays() {
        assert_eq!(num("let a = [1,2,3]; a.push(4); a.length"), 4.0);
        assert_eq!(num("let a = [10,20,30]; a[1]"), 20.0);
        assert_eq!(num("let a = [1,2,3]; let s=0; for(let i=0;i<a.length;i++){s=s+a[i];} s"), 6.0);
        assert_eq!(string("[1,2,3].join('-')"), "1-2-3");
        assert_eq!(num("[5,6,7].indexOf(6)"), 1.0);
    }

    #[test]
    fn objects() {
        assert_eq!(num("let o = {x: 10, y: 20}; o.x + o.y"), 30.0);
        assert_eq!(string("let o = {name: 'Euro'}; o.name"), "Euro");
        assert_eq!(num("let o = {}; o.a = 5; o['b'] = 7; o.a + o.b"), 12.0);
    }

    #[test]
    fn object_methods_and_this_free() {
        let src = "let counter = { n: 0, inc: function(){ return 1; } }; counter.inc() + counter.n";
        assert_eq!(num(src), 1.0);
    }

    #[test]
    fn math_builtins() {
        assert_eq!(num("Math.floor(3.7)"), 3.0);
        assert_eq!(num("Math.max(2, 9, 4)"), 9.0);
        assert_eq!(num("Math.sqrt(144)"), 12.0);
        assert_eq!(num("Math.pow(2, 10)"), 1024.0);
        assert_eq!(num("Math.abs(0 - 8)"), 8.0);
    }

    #[test]
    fn equality_semantics() {
        assert!(matches!(eval("1 == '1'").unwrap(), Value::Bool(true)));
        assert!(matches!(eval("1 === '1'").unwrap(), Value::Bool(false)));
        assert!(matches!(eval("null == undefined").unwrap(), Value::Bool(true)));
        assert!(matches!(eval("null === undefined").unwrap(), Value::Bool(false)));
    }

    #[test]
    fn logical_short_circuit() {
        assert_eq!(num("0 || 42"), 42.0);
        assert_eq!(num("7 && 9"), 9.0);
        assert!(matches!(eval("false && undefinedThing").unwrap(), Value::Bool(false)));
    }

    #[test]
    fn console_log_capture() {
        let (_, out) = run_capture("console.log('Hello', 'EuroOS'); console.log(1 + 2);");
        assert_eq!(out, alloc::vec!["Hello EuroOS".to_string(), "3".to_string()]);
    }

    #[test]
    fn realistic_program() {
        // Build a list, filter even numbers, sum them via a function.
        let src = "
            function sumEven(arr) {
                let total = 0;
                for (let i = 0; i < arr.length; i++) {
                    if (arr[i] % 2 === 0) { total = total + arr[i]; }
                }
                return total;
            }
            sumEven([1,2,3,4,5,6,7,8,9,10])
        ";
        assert_eq!(num(src), 30.0); // 2+4+6+8+10
    }

    #[test]
    fn errors_are_reported() {
        assert!(eval("let x = ;").is_err()); // syntax error
        assert!(eval("undefinedVar + 1").is_err()); // not defined
    }
}
