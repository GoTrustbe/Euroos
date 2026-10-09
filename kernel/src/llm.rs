//! **On-device inference** (2026-10-09): a language model runs ON EuroOS, in a
//! sandboxed glibc process, with no network and no cloud. The engine is
//! llama.cpp's `llama-simple` built WITHOUT AVX/FMA/F16C (so it also runs
//! under `-cpu qemu64`) and without OpenMP; the model is a GGUF file. Both are
//! served from a signed EuroPack (`/pack/llama-simple`, `/pack/model.gguf`),
//! verified page by page as they are read, and the model is demand-paged
//! straight from the pack (never copied whole into RAM).
//!
//! `llm <prompt>` in the Terminal; `[llm-local]` at boot when a pack with a
//! model is attached (otherwise the test says so and is not a failure).

use alloc::string::String;
use alloc::vec::Vec;
use euromm::FrameAllocator;

pub const ENGINE: &str = "/pack/llama-simple";
pub const MODEL: &str = "/pack/model.gguf";

pub fn available() -> bool {
    crate::ring3::europack_has(ENGINE) && crate::ring3::europack_has(MODEL)
}

/// Generate `n` tokens for `prompt`. Returns (text, exit code, ticks).
pub fn generate(falloc: &mut FrameAllocator, prompt: &str, n: u32) -> (String, u64, u64) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let nstr = alloc::format!("{n}");
    let argv: [&[u8]; 6] = [b"llama-simple", b"-m", MODEL.as_bytes(), b"-n", nstr.as_bytes(), prompt.as_bytes()];
    let t0 = crate::interrupts::ticks();
    // Model load + tokens take minutes under TCG (scalar SSE2): give the run 6 min.
    let prev_deadline = crate::ring3::GLIBC_DEADLINE_TICKS.swap(36_000, core::sync::atomic::Ordering::Relaxed);
    crate::ring3::GLIBC_ARENA_MIB.store(192, core::sync::atomic::Ordering::Relaxed);
    let (out, code) = crate::ring3::run_glibc_disk(falloc, ENGINE, crate::ring3::ldlinux_bytes(), &argv, &[b"PATH=/bin", b"LANG=C", b"HOME=/home/euro"], caps);
    crate::ring3::GLIBC_ARENA_MIB.store(96, core::sync::atomic::Ordering::Relaxed);
    crate::ring3::GLIBC_DEADLINE_TICKS.store(prev_deadline, core::sync::atomic::Ordering::Relaxed);
    (out, code, crate::interrupts::ticks().saturating_sub(t0))
}

/// The generated text: llama-simple prints the prompt followed by the tokens on
/// stdout; its statistics go to stderr (not captured here).
fn answer_of(out: &str, prompt: &str) -> String {
    let line = out.lines().find(|l| l.starts_with(prompt)).unwrap_or("");
    line.get(prompt.len()..).unwrap_or("").trim().into()
}

pub fn shell(falloc: &mut FrameAllocator, line: &str) -> Vec<String> {
    let prompt = line.trim().split_once(char::is_whitespace).map(|(_, p)| p.trim()).unwrap_or("");
    if !available() {
        return alloc::vec![String::from("llm: no model pack attached (serve /pack/llama-simple and /pack/model.gguf from a signed EuroPack)")];
    }
    if prompt.is_empty() {
        return alloc::vec![String::from("usage: llm <prompt>   (on-device, sandboxed, no network)")];
    }
    let (out, code, ticks) = generate(falloc, prompt, 16);
    let mut v = alloc::vec![alloc::format!("llm (on-device, {} ticks): {}", ticks, answer_of(&out, prompt))];
    if code != 0 {
        v.push(alloc::format!("llm: engine exit {code}"));
        v.extend(out.lines().rev().take(3).map(String::from));
    }
    v
}

/// `[llm-local]`: 8 tokens from the on-device model, no network.
pub fn selftest(falloc: &mut FrameAllocator) {
    if !available() {
        crate::serial_println!("[llm-local] on-device inference: no model pack attached (PACK=llm-pack.img serves /pack/llama-simple + /pack/model.gguf): skipped, not a failure");
        return;
    }
    let prompt = "The capital of Belgium is";
    let (out, code, ticks) = generate(falloc, prompt, 4);
    let ans = answer_of(&out, prompt);
    let words = ans.split_whitespace().count();
    // Under TCG a token takes tens of seconds; one generated word proves the
    // whole path (verified pack → demand-paged model → sandboxed engine → text).
    let ok = code == 0 && words >= 1;
    crate::serial_println!(
        "[llm-local] on-device inference (llama.cpp, no AVX, 135M Q8 model demand-paged from a signed EuroPack, sandboxed glibc process, no network): exit={code} · {ticks} ticks ({} s under TCG) · answer={ans:?} ({words} words of 4 asked) → {}",
        ticks / 100,
        if ok { "OK ✓ (a language model runs on EuroOS itself)" } else { "FAILED" }
    );
    if !ok {
        for l in out.lines().rev().take(6) { crate::serial_println!("[llm-local]   {l}"); }
    }
}
