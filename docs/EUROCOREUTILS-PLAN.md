# EuroOS — Coreutils Gap Analyse
*Referentie: uutils/coreutils (GNU-compatible Rust reimplementatie, 22.9k★, gebruikt door Ubuntu 26.04 LTS)*

**Doel:** EuroOS heeft vandaag ~45 shell commando's. De GNU coreutils standaard bevat 108 commando's. Dit document toont welke er al zijn, welke ontbreken, en hoe Claude Code ze kan toevoegen — in de juiste volgorde.

---

## Status overzicht

| Categorie | Aanwezig | Ontbreekt | Totaal |
|-----------|---------|-----------|--------|
| Bestandsbeheer | 9 | 8 | 17 |
| Tekst verwerking | 2 | 22 | 24 |
| Schijf & systeem | 6 | 8 | 14 |
| Checksums & encoding | 0 | 12 | 12 |
| Processen & omgeving | 5 | 8 | 13 |
| Overig | 3 | 25 | 28 |
| **Totaal** | **~25*** | **~83** | **108** |

*\* Sommige EuroOS commando's (net, euroguard, scrub, …) zijn eigen commando's buiten de coreutils standaard.*

---

## ✅ Al aanwezig in EuroOS

Gebaseerd op de gedocumenteerde ~45 shell commando's in TECHNICAL-OVERVIEW.md:

```
cat       ls        mkdir     rm        rename    rmdir
df        du (?)    ps        free      uname     uptime
ping      ping6     wget      https     net       netstat
resolve   write     dmesg     id        su        help
sprof     euroguard scrub     euroctl   eup       ctr
```

**Aanwezig maar nog niet coreutils-compatibel** (eigen implementatie, flags mogelijk afwijkend):
`cat`, `ls`, `mkdir`, `rm`, `df`, `ps`, `uname`, `uptime`, `id`

---

## ❌ Ontbreekt — gegroepeerd en geprioriteerd

### Prioriteit 1 — Dagelijks gebruik, elke developer verwacht ze

| Commando | Beschrijving | Complexiteit |
|----------|-------------|-------------|
| `cp` | Bestanden kopiëren | Laag |
| `mv` | Bestanden verplaatsen/hernoemen | Laag |
| `ln` | Harde en symbolische links | Laag |
| `touch` | Tijdstempel bijwerken / leeg bestand aanmaken | Laag |
| `stat` | Bestand/FS metadata tonen | Laag |
| `echo` | Tekst naar stdout | Laag |
| `pwd` | Huidige directory tonen | Laag |
| `head` | Eerste N regels tonen | Laag |
| `tail` | Laatste N regels tonen | Laag |
| `wc` | Regels/woorden/bytes tellen | Laag |
| `sort` | Regels sorteren | Middel |
| `uniq` | Dubbele regels rapporteren/verwijderen | Laag |
| `cut` | Kolommen/velden uitsnijden | Laag |
| `tr` | Tekens vertalen/verwijderen | Laag |
| `grep` | Patronen zoeken in tekst | Middel |
| `find` | Bestanden zoeken in directory tree | Middel |
| `date` | Datum/tijd tonen of instellen | Laag |
| `sleep` | Pauzeren | Laag |
| `true` / `false` | Exit codes 0/1 | Triviaal |
| `env` | Omgevingsvariabelen instellen/tonen | Laag |
| `printenv` | Omgevingsvariabelen tonen | Laag |
| `test` / `[` | Bestandstypen en waarden vergelijken | Middel |

### Prioriteit 2 — Scripting en automation

| Commando | Beschrijving | Complexiteit |
|----------|-------------|-------------|
| `basename` | Bestandsnaam zonder pad | Laag |
| `dirname` | Pad zonder bestandsnaam | Laag |
| `realpath` | Absoluut pad oplosssen | Laag |
| `readlink` | Symbolische link waarde | Laag |
| `mktemp` | Tijdelijk bestand/map aanmaken | Laag |
| `tee` | stdin naar stdout én bestanden | Laag |
| `xargs` | Commando's bouwen vanuit stdin | Middel |
| `seq` | Getallen reeks genereren | Laag |
| `printf` | Geformatteerde output | Middel |
| `expr` | Expressies evalueren | Middel |
| `fold` | Regels afbreken op breedte | Laag |
| `fmt` | Alinea's herformatteren | Laag |
| `join` | Bestanden samenvoegen op gemeenschappelijk veld | Middel |
| `comm` | Twee gesorteerde bestanden vergelijken | Laag |
| `split` | Bestand splitsen in delen | Laag |
| `csplit` | Bestand splitsen op context | Middel |
| `paste` | Bestanden naast elkaar plakken | Laag |
| `nl` | Regels nummeren | Laag |
| `od` | Binaire dump in octaal/hex | Middel |
| `tac` | Regels in omgekeerde volgorde | Laag |
| `rev` | Tekens per regel omdraaien | Laag |
| `shuf` | Willekeurige volgorde | Laag |

### Prioriteit 3 — Checksums en encoding

| Commando | Beschrijving | Complexiteit |
|----------|-------------|-------------|
| `md5sum` | MD5 checksum | Laag |
| `sha1sum` | SHA-1 checksum | Laag |
| `sha256sum` | SHA-256 checksum | Laag |
| `sha512sum` | SHA-512 checksum | Laag |
| `sha224sum` | SHA-224 checksum | Laag |
| `sha384sum` | SHA-384 checksum | Laag |
| `b2sum` | BLAKE2b checksum | Laag |
| `cksum` | CRC + bestandsgrootte | Laag |
| `sum` | Checksum + blokken tellen | Laag |
| `base32` | Base32 encode/decode | Laag |
| `base64` | Base64 encode/decode | Laag |
| `basenc` | Meerdere base-encodings | Laag |

### Prioriteit 4 — Systeem en hardware info

| Commando | Beschrijving | Complexiteit |
|----------|-------------|-------------|
| `nproc` | Aantal beschikbare cores | Laag |
| `arch` | Machine architectuur tonen | Triviaal |
| `hostname` | Hostname tonen/instellen | Laag |
| `numfmt` | Getallen naar leesbaar formaat | Laag |
| `truncate` | Bestandsgrootte aanpassen | Laag |
| `link` | `link()` syscall aanroepen | Triviaal |
| `unlink` | Bestand ontkoppelen | Triviaal |
| `factor` | Priemfactoren berekenen | Laag |
| `yes` | Herhaal tekst eindeloos | Triviaal |
| `ptx` | Permuted index van bestandsinhoud | Hoog |
| `pr` | Bestanden pagineren voor afdruk | Middel |
| `tsort` | Topologisch sorteren | Laag |

---

## Implementatieplan voor Claude Code

### Structuur

Alle coreutils commando's leven in het bestaande shell systeem. Elk commando is een match-arm in `kernel/src/shell.rs` (of equivalent), met een implementatie in `userspace/eurocoreutils/`:

```
userspace/
└── eurocoreutils/
    ├── mod.rs              // Registratie van alle commando's
    ├── fileops/
    │   ├── cp.rs           // cp
    │   ├── mv.rs           // mv
    │   ├── ln.rs           // ln
    │   ├── touch.rs        // touch
    │   ├── stat.rs         // stat
    │   └── truncate.rs     // truncate
    ├── text/
    │   ├── echo.rs         // echo
    │   ├── cat.rs          // cat (vervangt/verbetert bestaande)
    │   ├── head.rs         // head
    │   ├── tail.rs         // tail
    │   ├── wc.rs           // wc
    │   ├── sort.rs         // sort
    │   ├── uniq.rs         // uniq
    │   ├── cut.rs          // cut
    │   ├── tr.rs           // tr
    │   ├── grep.rs         // grep
    │   ├── tac.rs          // tac
    │   ├── rev.rs          // rev
    │   ├── shuf.rs         // shuf
    │   ├── fold.rs         // fold
    │   ├── fmt.rs          // fmt
    │   ├── join.rs         // join
    │   ├── comm.rs         // comm
    │   ├── paste.rs        // paste
    │   ├── nl.rs           // nl
    │   ├── od.rs           // od
    │   ├── split.rs        // split
    │   ├── csplit.rs       // csplit
    │   └── pr.rs           // pr
    ├── pathops/
    │   ├── pwd.rs          // pwd
    │   ├── basename.rs     // basename
    │   ├── dirname.rs      // dirname
    │   ├── realpath.rs     // realpath
    │   ├── readlink.rs     // readlink
    │   └── mktemp.rs       // mktemp
    ├── sysinfo/
    │   ├── date.rs         // date
    │   ├── nproc.rs        // nproc
    │   ├── arch.rs         // arch
    │   ├── hostname.rs     // hostname
    │   └── numfmt.rs       // numfmt
    ├── env/
    │   ├── env.rs          // env
    │   ├── printenv.rs     // printenv
    │   └── printf.rs       // printf
    ├── control/
    │   ├── sleep.rs        // sleep
    │   ├── true_false.rs   // true / false
    │   ├── test.rs         // test / [
    │   ├── expr.rs         // expr
    │   ├── seq.rs          // seq
    │   ├── yes.rs          // yes
    │   ├── tee.rs          // tee
    │   └── xargs.rs        // xargs
    ├── checksums/
    │   ├── md5sum.rs       // md5sum
    │   ├── sha1sum.rs      // sha1sum
    │   ├── sha256sum.rs    // sha256sum
    │   ├── sha512sum.rs    // sha512sum
    │   ├── sha224sum.rs    // sha224sum
    │   ├── sha384sum.rs    // sha384sum
    │   ├── b2sum.rs        // b2sum
    │   ├── cksum.rs        // cksum
    │   └── sum.rs          // sum
    ├── encoding/
    │   ├── base32.rs       // base32
    │   ├── base64.rs       // base64
    │   └── basenc.rs       // basenc
    └── misc/
        ├── factor.rs       // factor
        ├── link_unlink.rs  // link / unlink
        ├── tsort.rs        // tsort
        └── find.rs         // find
```

### Gedeelde infrastructuur

Alle commando's delen dezelfde hulpfuncties. Implementeer deze **eerst**:

```rust
// userspace/eurocoreutils/common.rs

/// Argumenten parsen: opties (beginnen met -) en positional args
pub struct Args {
    pub flags: HashSet<char>,           // -r, -v, -f
    pub options: HashMap<String, String>, // --max-depth=3
    pub positional: Vec<String>,
}

impl Args {
    pub fn parse(input: &[&str]) -> Self { ... }
    pub fn flag(&self, c: char) -> bool { self.flags.contains(&c) }
    pub fn option(&self, name: &str) -> Option<&str> { ... }
}

/// Standaard foutmelding in GNU-stijl
pub fn err(cmd: &str, path: &str, msg: &str) {
    eprintln!("{}: {}: {}", cmd, path, msg);
}

/// Glob uitbreiden: *.rs → [main.rs, lib.rs, ...]
pub fn glob_expand(pattern: &str, fs: &dyn FileSystem) -> Vec<String> { ... }
```

### Implementatiepatroon per commando

Elk commando volgt hetzelfde patroon zodat Claude Code consistent kan werken:

```rust
// userspace/eurocoreutils/text/head.rs

use crate::common::{Args, err};

/// head — print de eerste N regels van een bestand
/// GNU-compatibel: head [-n count] [-c bytes] [FILE...]
pub fn run(args: &[&str], fs: &dyn FileSystem, stdout: &mut dyn Write) -> i32 {
    let args = Args::parse(args);
    
    // Standaard: 10 regels
    let n_lines: usize = args.option("n")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    
    let files = if args.positional.is_empty() {
        vec!["-".to_string()]  // stdin
    } else {
        args.positional.clone()
    };
    
    let mut exit_code = 0;
    let show_headers = files.len() > 1;
    
    for path in &files {
        if show_headers {
            writeln!(stdout, "==> {} <==", path).ok();
        }
        
        match read_lines(path, fs) {
            Ok(lines) => {
                for line in lines.take(n_lines) {
                    writeln!(stdout, "{}", line).ok();
                }
            }
            Err(e) => {
                err("head", path, &e.to_string());
                exit_code = 1;
            }
        }
    }
    
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn head_default_10_lines() {
        let input = (1..=20).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        // ... test body
    }
    
    #[test]
    fn head_n_flag() { ... }
    
    #[test]
    fn head_multiple_files_shows_headers() { ... }
}
```

---

## Aanbevolen bouwvolgorde voor Claude Code

### Batch 1 — Triviale commando's (1 sessie)
Geen echte logica, directe kernel/syscall mapping:

```
true  false  yes  arch  pwd  echo  sleep  hostname
link  unlink  basename  dirname  nproc
```

**Waarom eerst:** minimale risico, directe testbaarheid, bouwt vertrouwen in het patroon.

### Batch 2 — Bestandsbeheer (1 sessie)
Bouwen op bestaande EuroFS operaties:

```
cp  mv  ln  touch  stat  truncate  readlink  realpath  mktemp
```

**Waarom:** dagelijks onmisbaar, EuroFS heeft alle nodige primitieven al.

### Batch 3 — Tekst I/O (1–2 sessies)
Lezen en schrijven van tekst, geen FS-writes:

```
head  tail  wc  tac  rev  fold  nl  paste  cat (uitbreiden)
```

**Waarom:** pure tekstverwerking, volledig host-testbaar zonder FS.

### Batch 4 — Tekst transformatie (1–2 sessies)
```
sort  uniq  cut  tr  shuf  comm  join  split  csplit  fmt  od
```

### Batch 5 — Zoeken en filteren (1 sessie)
```
grep  find  xargs
```
**Opmerking:** `find` is de complexste van de drie; begin met `grep`.

### Batch 6 — Checksums en encoding (1 sessie)
Crypto-primitieven bestaan al in `eurotls`:

```
sha256sum  sha512sum  sha224sum  sha384sum  sha1sum  md5sum
b2sum  cksum  sum  base64  base32  basenc
```

**Waarom samen:** allemaal dezelfde structuur (lees bestand → hash/encode → print). `eurotls` heeft SHA-256/512 al; hergebruik.

### Batch 7 — Omgeving en control (1 sessie)
```
env  printenv  printf  expr  seq  tee  test  date  numfmt  factor
```

### Batch 8 — Systeem en print (optioneel)
```
pr  tsort  ptx
```
**Laagste prioriteit:** `pr` en `ptx` zijn zeldzaam gebruikt.

---

## Test strategie

Alle coreutils zijn volledig **host-testbaar** — geen QEMU nodig. Ze werken op EuroFS primitieven die al host-testbaar zijn.

```bash
# Host tests draaien
cargo test -p eurocoreutils

# Per batch
cargo test -p eurocoreutils --test batch1_trivial
cargo test -p eurocoreutils --test batch2_fileops
cargo test -p eurocoreutils --test batch3_textio
```

### Referentie-output vergelijking
Voor GNU-compatibiliteit: elk commando heeft een test die de output vergelijkt met de verwachte GNU-output:

```rust
#[test]
fn sort_gnu_compatible() {
    // Verwachte output zoals GNU sort die zou produceren
    let expected = "aap\nbaar\nkat\n";
    let output = run_cmd(&["sort"], "kat\naap\nbaar\n");
    assert_eq!(output, expected);
}
```

---

## Wat EuroOS uniek maakt t.o.v. uutils

uutils streeft naar 100% GNU-compatibiliteit. EuroOS kan dat als baseline nemen maar voegt toe:

| Feature | GNU/uutils | EuroOS |
|---------|-----------|--------|
| `cp --verify` | Nee | Ja — Ed25519 handtekening verificatie na copy |
| `rm --immutable-check` | Nee | Ja — weigert immutable files (L1) |
| `stat --cap` | Nee | Ja — toont EuroGuard capabilities van bestand |
| `find --cap` | Nee | Ja — zoekt op capability-profiel |
| `grep --audit` | Nee | Ja — logt zoekopdracht in P3 audit log |
| Alle checksums | Alleen output | Output + optioneel `--sign` met Ed25519 |

Deze uitbreidingen zijn optioneel en achterwaarts compatibel — de standaard flags werken identiek aan GNU.

---

## Sprint aanduiding in de hoofdroadmap

Deze coreutils implementatie past in **Sprint M2 (europkg)** als tussenlaag:
- Batch 1–4 kunnen parallel lopen met Sprint R (EuroDevice)
- Batch 5–7 (grep/find/checksums) passen in Sprint M1 (EuroToolchain) als zelftest-infrastructuur
- De EuroOS-specifieke uitbreidingen (--cap, --sign) passen in Sprint X (EuroPol)

**Claude Code sprint commando:** `"implementeer eurocoreutils batch 1"` of `"voeg grep toe aan EuroOS shell"`

---

*Referentie: https://uutils.github.io/coreutils/docs/ — volledig gedocumenteerde GNU-compatibele implementatie in Rust, MIT-gelicenseerd, bruikbaar als implementatiereferentie.*
