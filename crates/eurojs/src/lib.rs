//! EuroJS — de JavaScript-engine van EuroWeb (Sprint AB-B5).
//!
//! Een **tree-walking interpreter** — bewust geen JIT, want dat zou een groot
//! aanvalsoppervlak toevoegen. Ondersteunt een echte JS-subset: getallen, strings,
//! booleans, `null`/`undefined`, `let`/`var`/`const`, alle gangbare operatoren,
//! `if`/`while`/`for`, functies (declaraties, expressies, arrows) met **closures**,
//! arrays en objecten met methoden, `Math.*`, en `console.log`. Per-tab krijgt een
//! interpreter in de browser een beperkte capability-set (zie EUROBROWSER-PLAN);
//! deze crate is de pure, host-geteste rekenkern. `no_std`, geen `unsafe`.

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

/// Parse + voer JS-broncode uit; geeft de waarde van de laatste expressie.
pub fn eval(src: &str) -> Result<Value, String> {
    let toks = lexer::lex(src)?;
    let prog = parser::Parser::new(toks).parse_program()?;
    Interp::new().run(&prog)
}

/// Voer JS uit en geef (resultaat, console.log-uitvoer) terug.
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

/// Voer een paginascript uit en geef (resultaat, console.log-uitvoer,
/// `document.write`-uitvoer) terug — voor de EuroWeb-integratie: de writes worden
/// als tekst aan de gerenderde pagina toegevoegd.
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

/// Hulp: een numeriek resultaat eruit halen (voor tests/integratie).
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
            run_page("console.log('hoi'); document.write('Som: ' + (6*7)); document.writeln('!');");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0], "hoi");
        assert_eq!(writes.join(""), "Som: 42!\n");
    }
}

/// Hulp: een stringresultaat.
pub fn as_str(v: &Value) -> Option<alloc::string::String> {
    match v {
        Value::Str(s) => Some((**s).clone()),
        _ => None,
    }
}

// ── kleine no_std-mathkern voor Math.* (geen libm) ──

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
    // Geheeltallige exponent → herhaald vermenigvuldigen; anders benadering.
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

    // ---- Robuustheid / stabiliteit: niet-vertrouwde scripts ----
    //
    // Pagina-JS draait in de kernel. Een script MAG de OS niet platleggen:
    // het moet netjes met Err afbreken (geen hang, geen stack-overflow),
    // terwijl legitieme programma's blijven werken.

    #[test]
    fn robust_infinite_while_is_bounded() {
        // Mag niet eindeloos hangen: het stap-budget breekt af met Err.
        let r = eval("while (true) { }");
        assert!(r.is_err(), "oneindige while moet afbreken (stap-budget)");
    }

    #[test]
    fn robust_infinite_for_is_bounded() {
        let r = eval("for (;;) { let x = 1; }");
        assert!(r.is_err(), "oneindige for moet afbreken (stap-budget)");
    }

    /// Voer JS uit op een ruime stack zodat de TEST zelf nooit overflowt; we
    /// testen hier de diepte-GRENS-logica (geeft die netjes Err?), niet de
    /// stackgrootte van de test-thread. In de kernel is de 16 KiB guarded
    /// taakstack de harde vangrail; deze grens is de zachte, nette laag.
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
        // Oneindige recursie zou de kernel-stack opblazen → moet netjes Err geven.
        let r = eval_on_big_stack("function f(){ return f(); } f()");
        assert!(r.is_err(), "oneindige recursie moet afbreken (diepte-grens)");
        let mutual = "function a(){return b();} function b(){return a();} a()";
        assert!(eval_on_big_stack(mutual).is_err(), "wederzijdse recursie moet afbreken");
    }

    #[test]
    fn robust_legit_deep_recursion_still_works() {
        // Een echte, begrensde recursie (diepte 150 < grens) moet gewoon werken —
        // de grens mag legitiem gebruik niet breken.
        let src = "function sum(n){ if(n==0) return 0; return n + sum(n-1); } sum(150)";
        let r = eval_on_big_stack(src);
        assert!(r.is_ok(), "legitieme recursie van diepte 150 moet slagen");
    }

    #[test]
    fn robust_legit_long_loop_still_works() {
        // Een lange-maar-eindige lus (binnen budget) moet correct doorlopen.
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
        let (_, out) = run_capture("console.log('Hallo', 'EuroOS'); console.log(1 + 2);");
        assert_eq!(out, alloc::vec!["Hallo EuroOS".to_string(), "3".to_string()]);
    }

    #[test]
    fn realistic_program() {
        // Bouw een lijst, filter even getallen, tel ze op via een functie.
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
        assert!(eval("let x = ;").is_err()); // syntaxfout
        assert!(eval("undefinedVar + 1").is_err()); // niet gedefinieerd
    }
}
