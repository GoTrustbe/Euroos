//! EuroJS tree-walking interpreter (geen JIT → klein aanvalsoppervlak).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::ast::{BinOp, Expr, Stmt};

/// Een runtime-waarde.
#[derive(Clone)]
pub enum Value {
    Num(f64),
    Str(Rc<String>),
    Bool(bool),
    Null,
    Undefined,
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<BTreeMap<String, Value>>>),
    Func(Rc<FuncDef>),
}

pub struct FuncDef {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub env: Env,
}

type Env = Rc<RefCell<Scope>>;

pub struct Scope {
    vars: BTreeMap<String, Value>,
    parent: Option<Env>,
}

fn new_scope(parent: Option<Env>) -> Env {
    Rc::new(RefCell::new(Scope { vars: BTreeMap::new(), parent }))
}

fn env_get(env: &Env, name: &str) -> Option<Value> {
    let s = env.borrow();
    if let Some(v) = s.vars.get(name) {
        Some(v.clone())
    } else if let Some(p) = &s.parent {
        env_get(p, name)
    } else {
        None
    }
}

fn env_set(env: &Env, name: &str, val: Value) -> bool {
    let mut s = env.borrow_mut();
    if s.vars.contains_key(name) {
        s.vars.insert(name.to_string(), val);
        true
    } else if let Some(p) = s.parent.clone() {
        drop(s);
        env_set(&p, name, val)
    } else {
        false
    }
}

fn env_define(env: &Env, name: &str, val: Value) {
    env.borrow_mut().vars.insert(name.to_string(), val);
}

enum Flow {
    Normal(Value),
    Return(Value),
}

/// De interpreter; `output` verzamelt `console.log`-uitvoer.
///
/// Stabiliteit: deze interpreter draait NIET-VERTROUWDE pagina-scripts in de
/// kernel. Twee harde grenzen voorkomen dat een script de OS platlegt:
/// een globaal *stap-budget* (tegen oneindige/zware lussen) en een
/// *aanroep-diepte*-grens (tegen oneindige recursie → stack-overflow).
pub struct Interp {
    pub output: Vec<String>,
    global: Env,
    steps: u64,
    depth: usize,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    /// Maximaal aantal uitgevoerde statements/lus-iteraties vóór afbreken.
    const STEP_LIMIT: u64 = 50_000_000;
    /// Maximale geneste functie-aanroep-diepte (anti stack-overflow). Een
    /// tree-walking interpreter verbruikt meerdere Rust-frames per JS-aanroep;
    /// de kernel-taakstack is slechts 16 KiB (met guard-pagina als harde
    /// vangrail). Deze zachtere grens breekt oneindige recursie netjes met een
    /// JS-fout af i.p.v. de taak te laten killen door de guard.
    const DEPTH_LIMIT: usize = 256;

    pub fn new() -> Self {
        Interp { output: Vec::new(), global: new_scope(None), steps: 0, depth: 0 }
    }

    /// Tel één uitvoeringsstap; breek af als het budget op is.
    #[inline]
    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > Self::STEP_LIMIT {
            return Err("uitvoeringsbudget overschreden (mogelijke oneindige lus)".to_string());
        }
        Ok(())
    }

    /// Voer een programma uit; geeft de waarde van de laatste expressie.
    pub fn run(&mut self, prog: &[Stmt]) -> Result<Value, String> {
        let env = self.global.clone();
        let mut last = Value::Undefined;
        // Hijs functie-declaraties (zodat ze vóór hun definitie aanroepbaar zijn).
        for s in prog {
            if let Stmt::FuncDecl(name, params, body) = s {
                let f = Value::Func(Rc::new(FuncDef { params: params.clone(), body: body.clone(), env: env.clone() }));
                env_define(&env, name, f);
            }
        }
        for s in prog {
            match self.exec(s, &env)? {
                Flow::Normal(v) => last = v,
                Flow::Return(v) => return Ok(v),
            }
        }
        Ok(last)
    }

    fn exec_block(&mut self, stmts: &[Stmt], env: &Env) -> Result<Flow, String> {
        let mut last = Value::Undefined;
        for s in stmts {
            match self.exec(s, env)? {
                Flow::Normal(v) => last = v,
                r @ Flow::Return(_) => return Ok(r),
            }
        }
        Ok(Flow::Normal(last))
    }

    fn exec(&mut self, s: &Stmt, env: &Env) -> Result<Flow, String> {
        self.tick()?;
        match s {
            Stmt::Let(name, init) => {
                let v = match init {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                env_define(env, name, v);
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::Expr(e) => Ok(Flow::Normal(self.eval(e, env)?)),
            Stmt::FuncDecl(name, params, body) => {
                let f = Value::Func(Rc::new(FuncDef { params: params.clone(), body: body.clone(), env: env.clone() }));
                env_define(env, name, f);
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::Block(stmts) => {
                let inner = new_scope(Some(env.clone()));
                self.exec_block(stmts, &inner)
            }
            Stmt::If(cond, then, els) => {
                if truthy(&self.eval(cond, env)?) {
                    let inner = new_scope(Some(env.clone()));
                    self.exec_block(then, &inner)
                } else {
                    let inner = new_scope(Some(env.clone()));
                    self.exec_block(els, &inner)
                }
            }
            Stmt::While(cond, body) => {
                while truthy(&self.eval(cond, env)?) {
                    // Elke iteratie telt mee voor het globale stap-budget,
                    // ook bij een leeg lichaam — zo is `while(true){}` begrensd.
                    self.tick()?;
                    let inner = new_scope(Some(env.clone()));
                    if let Flow::Return(v) = self.exec_block(body, &inner)? {
                        return Ok(Flow::Return(v));
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::For(init, cond, step, body) => {
                let loop_env = new_scope(Some(env.clone()));
                if let Some(s) = init {
                    self.exec(s, &loop_env)?;
                }
                loop {
                    self.tick()?;
                    let go = match cond {
                        Some(c) => truthy(&self.eval(c, &loop_env)?),
                        None => true,
                    };
                    if !go {
                        break;
                    }
                    let inner = new_scope(Some(loop_env.clone()));
                    if let Flow::Return(v) = self.exec_block(body, &inner)? {
                        return Ok(Flow::Return(v));
                    }
                    if let Some(st) = step {
                        self.eval(st, &loop_env)?;
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                Ok(Flow::Return(v))
            }
        }
    }

    fn eval(&mut self, e: &Expr, env: &Env) -> Result<Value, String> {
        match e {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(Rc::new(s.clone()))),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Undefined => Ok(Value::Undefined),
            Expr::Ident(name) => env_get(env, name).ok_or_else(|| alloc::format!("{name} is niet gedefinieerd")),
            Expr::Array(items) => {
                let mut v = Vec::new();
                for it in items {
                    v.push(self.eval(it, env)?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(v))))
            }
            Expr::Object(props) => {
                let mut m = BTreeMap::new();
                for (k, ve) in props {
                    let val = self.eval(ve, env)?;
                    m.insert(k.clone(), val);
                }
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }
            Expr::Neg(inner) => Ok(Value::Num(-to_num(&self.eval(inner, env)?))),
            Expr::Not(inner) => Ok(Value::Bool(!truthy(&self.eval(inner, env)?))),
            Expr::And(a, b) => {
                let l = self.eval(a, env)?;
                if truthy(&l) {
                    self.eval(b, env)
                } else {
                    Ok(l)
                }
            }
            Expr::Or(a, b) => {
                let l = self.eval(a, env)?;
                if truthy(&l) {
                    Ok(l)
                } else {
                    self.eval(b, env)
                }
            }
            Expr::Cond(c, t, f) => {
                if truthy(&self.eval(c, env)?) {
                    self.eval(t, env)
                } else {
                    self.eval(f, env)
                }
            }
            Expr::Bin(op, a, b) => {
                let l = self.eval(a, env)?;
                let r = self.eval(b, env)?;
                Ok(binop(op, &l, &r))
            }
            Expr::Func(params, body) => {
                Ok(Value::Func(Rc::new(FuncDef { params: params.clone(), body: body.clone(), env: env.clone() })))
            }
            Expr::Update(inc, target) => {
                let cur = to_num(&self.eval(target, env)?);
                let nv = Value::Num(if *inc { cur + 1.0 } else { cur - 1.0 });
                self.assign(target, nv.clone(), env)?;
                Ok(nv)
            }
            Expr::Assign(target, val) => {
                let v = self.eval(val, env)?;
                self.assign(target, v.clone(), env)?;
                Ok(v)
            }
            Expr::Member(obj, prop) => {
                let o = self.eval(obj, env)?;
                self.get_member(&o, prop)
            }
            Expr::Index(obj, idx) => {
                let o = self.eval(obj, env)?;
                let i = self.eval(idx, env)?;
                self.get_index(&o, &i)
            }
            Expr::Call(callee, args) => self.eval_call(callee, args, env),
        }
    }

    fn assign(&mut self, target: &Expr, val: Value, env: &Env) -> Result<(), String> {
        match target {
            Expr::Ident(name) => {
                if !env_set(env, name, val.clone()) {
                    env_define(&self.global, name, val); // impliciete globale
                }
                Ok(())
            }
            Expr::Member(obj, prop) => {
                let o = self.eval(obj, env)?;
                if let Value::Object(m) = o {
                    m.borrow_mut().insert(prop.clone(), val);
                    Ok(())
                } else {
                    Err("kan eigenschap niet zetten op niet-object".to_string())
                }
            }
            Expr::Index(obj, idx) => {
                let o = self.eval(obj, env)?;
                let i = self.eval(idx, env)?;
                match o {
                    Value::Array(a) => {
                        let n = to_num(&i) as i64;
                        let mut arr = a.borrow_mut();
                        if n >= 0 {
                            let n = n as usize;
                            if n >= arr.len() {
                                arr.resize(n + 1, Value::Undefined);
                            }
                            arr[n] = val;
                        }
                        Ok(())
                    }
                    Value::Object(m) => {
                        m.borrow_mut().insert(to_string(&i), val);
                        Ok(())
                    }
                    _ => Err("kan index niet zetten".to_string()),
                }
            }
            _ => Err("ongeldig toewijzingsdoel".to_string()),
        }
    }

    fn get_member(&mut self, o: &Value, prop: &str) -> Result<Value, String> {
        match o {
            Value::Array(a) => match prop {
                "length" => Ok(Value::Num(a.borrow().len() as f64)),
                _ => Ok(Value::Undefined),
            },
            Value::Str(s) => match prop {
                "length" => Ok(Value::Num(s.chars().count() as f64)),
                _ => Ok(Value::Undefined),
            },
            Value::Object(m) => Ok(m.borrow().get(prop).cloned().unwrap_or(Value::Undefined)),
            _ => Ok(Value::Undefined),
        }
    }

    fn get_index(&mut self, o: &Value, i: &Value) -> Result<Value, String> {
        match o {
            Value::Array(a) => {
                let n = to_num(i) as i64;
                let arr = a.borrow();
                if n >= 0 && (n as usize) < arr.len() {
                    Ok(arr[n as usize].clone())
                } else {
                    Ok(Value::Undefined)
                }
            }
            Value::Object(m) => Ok(m.borrow().get(&to_string(i)).cloned().unwrap_or(Value::Undefined)),
            Value::Str(s) => {
                let n = to_num(i) as i64;
                if n >= 0 {
                    if let Some(ch) = s.chars().nth(n as usize) {
                        return Ok(Value::Str(Rc::new(ch.to_string())));
                    }
                }
                Ok(Value::Undefined)
            }
            _ => Ok(Value::Undefined),
        }
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr], env: &Env) -> Result<Value, String> {
        // Methode-/built-in-aanroepen: callee is een Member.
        if let Expr::Member(obj_e, method) = callee {
            // console.log(...)
            if let Expr::Ident(name) = obj_e.as_ref() {
                if name == "console" && method == "log" {
                    let mut parts = Vec::new();
                    for a in args {
                        let v = self.eval(a, env)?;
                        parts.push(display(&v));
                    }
                    self.output.push(parts.join(" "));
                    return Ok(Value::Undefined);
                }
                if name == "Math" {
                    let mut av = Vec::new();
                    for a in args {
                        av.push(to_num(&self.eval(a, env)?));
                    }
                    return Ok(Value::Num(math_call(method, &av)));
                }
            }
            let obj = self.eval(obj_e, env)?;
            let mut av = Vec::new();
            for a in args {
                av.push(self.eval(a, env)?);
            }
            return self.call_method(&obj, method, av);
        }
        // Gewone functie-aanroep.
        let f = self.eval(callee, env)?;
        let mut av = Vec::new();
        for a in args {
            av.push(self.eval(a, env)?);
        }
        self.call(&f, av)
    }

    fn call_method(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match obj {
            Value::Array(a) => match method {
                "push" => {
                    let mut arr = a.borrow_mut();
                    for v in args {
                        arr.push(v);
                    }
                    Ok(Value::Num(arr.len() as f64))
                }
                "pop" => Ok(a.borrow_mut().pop().unwrap_or(Value::Undefined)),
                "join" => {
                    let sep = args.first().map(to_string).unwrap_or_else(|| ",".to_string());
                    let arr = a.borrow();
                    Ok(Value::Str(Rc::new(arr.iter().map(display).collect::<Vec<_>>().join(&sep))))
                }
                "indexOf" => {
                    let target = args.first().cloned().unwrap_or(Value::Undefined);
                    let arr = a.borrow();
                    let pos = arr.iter().position(|v| loose_eq(v, &target));
                    Ok(Value::Num(pos.map(|p| p as f64).unwrap_or(-1.0)))
                }
                _ => Err(alloc::format!("array heeft geen methode '{method}'")),
            },
            Value::Str(s) => match method {
                "toUpperCase" => Ok(Value::Str(Rc::new(s.to_uppercase()))),
                "toLowerCase" => Ok(Value::Str(Rc::new(s.to_lowercase()))),
                "charAt" => {
                    let n = args.first().map(to_num).unwrap_or(0.0) as usize;
                    Ok(Value::Str(Rc::new(s.chars().nth(n).map(|c| c.to_string()).unwrap_or_default())))
                }
                "includes" => {
                    let needle = args.first().map(to_string).unwrap_or_default();
                    Ok(Value::Bool(s.contains(&needle)))
                }
                _ => Err(alloc::format!("string heeft geen methode '{method}'")),
            },
            Value::Object(m) => {
                let f = m.borrow().get(method).cloned();
                match f {
                    Some(f) => self.call(&f, args),
                    None => Err(alloc::format!("object heeft geen methode '{method}'")),
                }
            }
            _ => Err(alloc::format!("kan '{method}' niet aanroepen")),
        }
    }

    fn call(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, String> {
        match f {
            Value::Func(def) => {
                // Aanroep-diepte begrenzen: oneindige recursie zou anders de
                // kernel-stack opblazen. We tellen op, voeren uit, tellen af.
                self.depth += 1;
                if self.depth > Self::DEPTH_LIMIT {
                    self.depth -= 1;
                    return Err("aanroep-diepte overschreden (oneindige recursie?)".to_string());
                }
                let call_env = new_scope(Some(def.env.clone()));
                for (i, p) in def.params.iter().enumerate() {
                    env_define(&call_env, p, args.get(i).cloned().unwrap_or(Value::Undefined));
                }
                let r = self.exec_block(&def.body, &call_env);
                self.depth -= 1;
                match r? {
                    Flow::Return(v) => Ok(v),
                    Flow::Normal(_) => Ok(Value::Undefined),
                }
            }
            _ => Err("waarde is niet aanroepbaar".to_string()),
        }
    }
}

// ── operatoren & coercie ──

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0 && !n.is_nan(),
        Value::Str(s) => !s.is_empty(),
        Value::Null | Value::Undefined => false,
        _ => true,
    }
}

fn to_num(v: &Value) -> f64 {
    match v {
        Value::Num(n) => *n,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Str(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

fn to_string(v: &Value) -> String {
    display(v)
}

/// JS-achtige stringweergave.
fn display(v: &Value) -> String {
    match v {
        Value::Num(n) => {
            // Geheel getal binnen i64-bereik → zonder decimalen tonen (JS-stijl).
            if n.is_finite() && *n == (*n as i64) as f64 {
                alloc::format!("{}", *n as i64)
            } else {
                alloc::format!("{n}")
            }
        }
        Value::Str(s) => (**s).clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Undefined => "undefined".to_string(),
        Value::Array(a) => a.borrow().iter().map(display).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
        Value::Func(_) => "function".to_string(),
    }
}

fn binop(op: &BinOp, l: &Value, r: &Value) -> Value {
    match op {
        BinOp::Add => {
            // String-concatenatie als één van beide een string is.
            if matches!(l, Value::Str(_)) || matches!(r, Value::Str(_)) {
                Value::Str(Rc::new(alloc::format!("{}{}", display(l), display(r))))
            } else {
                Value::Num(to_num(l) + to_num(r))
            }
        }
        BinOp::Sub => Value::Num(to_num(l) - to_num(r)),
        BinOp::Mul => Value::Num(to_num(l) * to_num(r)),
        BinOp::Div => Value::Num(to_num(l) / to_num(r)),
        BinOp::Mod => Value::Num(to_num(l) % to_num(r)),
        BinOp::Eq => Value::Bool(loose_eq(l, r)),
        BinOp::Ne => Value::Bool(!loose_eq(l, r)),
        BinOp::StrictEq => Value::Bool(strict_eq(l, r)),
        BinOp::StrictNe => Value::Bool(!strict_eq(l, r)),
        BinOp::Lt => cmp(l, r, |o| o < 0),
        BinOp::Gt => cmp(l, r, |o| o > 0),
        BinOp::Le => cmp(l, r, |o| o <= 0),
        BinOp::Ge => cmp(l, r, |o| o >= 0),
    }
}

fn cmp(l: &Value, r: &Value, f: fn(i32) -> bool) -> Value {
    // String-vergelijking als beide strings zijn, anders numeriek.
    if let (Value::Str(a), Value::Str(b)) = (l, r) {
        let o = match a.cmp(b) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        };
        return Value::Bool(f(o));
    }
    let (a, b) = (to_num(l), to_num(r));
    if a.is_nan() || b.is_nan() {
        return Value::Bool(false);
    }
    let o = if a < b {
        -1
    } else if a > b {
        1
    } else {
        0
    };
    Value::Bool(f(o))
}

fn strict_eq(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Num(a), Value::Num(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        (Value::Undefined, Value::Undefined) => true,
        (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
        (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

fn loose_eq(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,
        (Value::Str(_), Value::Num(_)) | (Value::Num(_), Value::Str(_)) => to_num(l) == to_num(r),
        (Value::Bool(_), _) | (_, Value::Bool(_)) => to_num(l) == to_num(r),
        _ => strict_eq(l, r),
    }
}

fn math_call(name: &str, args: &[f64]) -> f64 {
    let a = args.first().copied().unwrap_or(f64::NAN);
    match name {
        "floor" => crate::flr(a),
        "ceil" => -crate::flr(-a),
        "round" => crate::flr(a + 0.5),
        "abs" => {
            if a < 0.0 {
                -a
            } else {
                a
            }
        }
        "sqrt" => crate::sqrt(a),
        "max" => args.iter().copied().fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a }),
        "min" => args.iter().copied().fold(f64::INFINITY, |a, b| if b < a { b } else { a }),
        "pow" => crate::powi(a, args.get(1).copied().unwrap_or(0.0)),
        _ => f64::NAN,
    }
}
