//! The machine model does what the processor does: each integer and flag-setting operation is
//! run on this computer with inline assembly and on the model, with the same operands, and the
//! result and flags compared.

#![cfg(target_arch = "x86_64")]

use s2lift::mach::{Img, M};
use std::arch::asm;

static IMG: Img = Img { blob: &[], ranges: &[], tls: &[], tls_zero: 0, tls_slot: 0 };
static IMGS: &[&Img] = &[&IMG];

/// The arithmetic flags after an operation, as the processor leaves them: CF, PF, ZF, SF, OF.
fn flags_bits(f: u64) -> [bool; 5] {
    [f & 1 != 0, f & 4 != 0, f & 0x40 != 0, f & 0x80 != 0, f & 0x800 != 0]
}

fn model_flags(m: &M) -> [bool; 5] {
    [m.cf, m.pf, m.zf, m.sf, m.of]
}

macro_rules! alu {
    ($name:ident, $asm:literal, $method:ident, $ty:ty, $size:expr) => {
        fn $name(a: u64, b: u64, cin: bool) -> (u64, [bool; 5], u64, [bool; 5]) {
            let (mut x, y) = (a as $ty, b as $ty);
            let flags: u64;
            unsafe {
                asm!(
                    "stc",
                    "cmc",
                    "test {c:e}, {c:e}",
                    "jz 2f",
                    "stc",
                    "2:",
                    $asm,
                    "pushfq",
                    "pop {f}",
                    x = inout(reg) x,
                    y = in(reg) y,
                    c = in(reg) cin as u64,
                    f = out(reg) flags,
                );
            }
            let mut m = M::new(IMGS);
            m.cf = cin;
            let r = m.$method(a as $ty as u64, b as $ty as u64, $size) as $ty;
            (x as u64, flags_bits(flags), r as u64, model_flags(&m))
        }
    };
}

alu!(add32, "add {x:e}, {y:e}", add, u32, 4);
alu!(sub32, "sub {x:e}, {y:e}", sub, u32, 4);
alu!(add64, "add {x}, {y}", add, u64, 8);
alu!(sub64, "sub {x}, {y}", sub, u64, 8);
alu!(adc32, "adc {x:e}, {y:e}", adc, u32, 4);
alu!(sbb32, "sbb {x:e}, {y:e}", sbb, u32, 4);
alu!(adc64, "adc {x}, {y}", adc, u64, 8);
alu!(sbb64, "sbb {x}, {y}", sbb, u64, 8);
alu!(and32, "and {x:e}, {y:e}", and, u32, 4);
alu!(or64, "or {x}, {y}", or, u64, 8);
alu!(xor32, "xor {x:e}, {y:e}", xor, u32, 4);

fn interesting() -> Vec<u64> {
    let mut v = vec![0, 1, 2, 3, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff,
        0x1_0000_0000, 0x7fff_ffff_ffff_ffff, 0x8000_0000_0000_0000, u64::MAX, u64::MAX - 1, 0x1234_5678_9abc_def0];
    let mut s = 0x2545f4914f6cdd1du64;
    for _ in 0..40 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.push(s);
        v.push(s & 0xffff_ffff);
    }
    v
}

#[test]
fn arithmetic_and_flags_match_the_processor() {
    let vals = interesting();
    let mut bad = Vec::new();
    for &a in &vals {
        for &b in &vals {
            for (name, f) in [("add32", add32 as fn(u64, u64, bool) -> _), ("sub32", sub32), ("add64", add64), ("sub64", sub64), ("and32", and32), ("or64", or64), ("xor32", xor32)] {
                let (hw, hf, mr, mf) = f(a, b, false);
                if hw != mr || hf != mf {
                    bad.push(format!("{name}({a:#x}, {b:#x}) hw {hw:#x} {hf:?} model {mr:#x} {mf:?}"));
                }
            }
            for cin in [false, true] {
                for (name, f) in [("adc32", adc32 as fn(u64, u64, bool) -> _), ("sbb32", sbb32), ("adc64", adc64), ("sbb64", sbb64)] {
                    let (hw, hf, mr, mf) = f(a, b, cin);
                    if hw != mr || hf != mf {
                        bad.push(format!("{name}({a:#x}, {b:#x}, cin {cin}) hw {hw:#x} {hf:?} model {mr:#x} {mf:?}"));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} differ, e.g.\n{}", bad.len(), bad.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}

fn shift_hw(op: &str, a: u64, n: u8, wide: bool) -> (u64, [bool; 5]) {
    let flags: u64;
    let mut x = a;
    macro_rules! run {
        ($ins:literal) => {
            unsafe {
                asm!("xor {t:e}, {t:e}", "add {t:e}, {t:e}", $ins, "pushfq", "pop {f}",
                     x = inout(reg) x, in("cl") n, f = out(reg) flags, t = out(reg) _)
            }
        };
    }
    match (op, wide) {
        ("shl", true) => run!("shl {x}, cl"),
        ("shr", true) => run!("shr {x}, cl"),
        ("sar", true) => run!("sar {x}, cl"),
        ("shl", false) => run!("shl {x:e}, cl"),
        ("shr", false) => run!("shr {x:e}, cl"),
        _ => run!("sar {x:e}, cl"),
    }
    (if wide { x } else { x & 0xffff_ffff }, flags_bits(flags))
}

#[test]
fn shifts_match_the_processor() {
    let mut bad = Vec::new();
    for &a in &interesting() {
        for n in [0u8, 1, 2, 7, 31, 32, 33, 63, 64, 65] {
            for op in ["shl", "shr", "sar"] {
                for wide in [false, true] {
                    let (hw, hf) = shift_hw(op, a, n, wide);
                    let mut m = M::new(IMGS);
                    // the flags before: those `add 0, 0` leaves, i.e. ZF and PF set
                    m.zf = true;
                    m.pf = true;
                    let size = if wide { 8 } else { 4 };
                    let r = match op {
                        "shl" => m.shl(a, n as u64, size),
                        "shr" => m.shr(a, n as u64, size),
                        _ => m.sar(a, n as u64, size),
                    };
                    let mf = [m.cf, m.pf, m.zf, m.sf, m.of];
                    // OF is defined only for a count of 1; the others agree whenever the count is not 0
                    let mut hf2 = hf;
                    let mut mf2 = mf;
                    if (n & if wide { 63 } else { 31 }) != 1 {
                        hf2[4] = false;
                        mf2[4] = false;
                    }
                    if hw != r || hf2 != mf2 {
                        bad.push(format!("{op}{} {a:#x} by {n}: hw {hw:#x} {hf:?} model {r:#x} {mf:?}", if wide { 64 } else { 32 }));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} differ, e.g.\n{}", bad.len(), bad.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}

fn comisd_hw(a: f64, b: f64) -> [bool; 5] {
    let flags: u64;
    unsafe {
        asm!("comisd {a}, {b}", "pushfq", "pop {f}", a = in(xmm_reg) a, b = in(xmm_reg) b, f = out(reg) flags);
    }
    flags_bits(flags)
}

#[test]
fn double_comparison_flags_match_the_processor() {
    let vals = [0.0, -0.0, 1.0, -1.0, 2.5, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 5e-324, f64::MAX];
    let mut bad = Vec::new();
    for &a in &vals {
        for &b in &vals {
            let hw = comisd_hw(a, b);
            let mut m = M::new(IMGS);
            m.comisd(a.to_bits(), b.to_bits());
            let mf = [m.cf, m.pf, m.zf, m.sf, m.of];
            if hw != mf {
                bad.push(format!("comisd {a} {b}: hw {hw:?} model {mf:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn conversions_match_the_processor() {
    let vals = [0.0, -0.0, 0.5, 1.5, 2.5, -0.5, -1.5, -2.5, 2147483647.0, 2147483647.5, 2147483648.0, -2147483648.0, -2147483648.5,
        -2147483649.0, 9.3e18, -9.3e18, 1e300, f64::NAN, f64::INFINITY, 4294967296.5];
    let mut bad = Vec::new();
    for &x in &vals {
        let (hw32, hw64): (u32, u64);
        let (rn32, rt32): (u32, u32);
        unsafe {
            let (mut a, mut b): (u32, u64) = (0, 0);
            asm!("cvttsd2si {a:e}, {x}", "cvttsd2si {b}, {x}", x = in(xmm_reg) x, a = out(reg) a, b = out(reg) b);
            hw32 = a;
            hw64 = b;
            let mut r = [0u32; 4];
            asm!("cvtpd2dq xmm0, {x}", "movdqu [{p}], xmm0", x = in(xmm_reg) x, p = in(reg) r.as_mut_ptr(), out("xmm0") _);
            rn32 = r[0];
            asm!("cvttpd2dq xmm0, {x}", "movdqu [{p}], xmm0", x = in(xmm_reg) x, p = in(reg) r.as_mut_ptr(), out("xmm0") _);
            rt32 = r[0];
        }
        let mut m = M::new(IMGS);
        let (m32, m64) = (m.cvttsd2si(x.to_bits(), 4) as u32, m.cvttsd2si(x.to_bits(), 8));
        m.cvtpd2dq(0, x.to_bits() as u128, false);
        let mn32 = m.x[0] as u32;
        m.cvtpd2dq(0, x.to_bits() as u128, true);
        let mt32 = m.x[0] as u32;
        if (hw32, hw64, rn32, rt32) != (m32, m64, mn32, mt32) {
            bad.push(format!("{x}: hw {hw32:#x} {hw64:#x} {rn32:#x} {rt32:#x} model {m32:#x} {m64:#x} {mn32:#x} {mt32:#x}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn multiplication_negation_and_counting_match_the_processor() {
    let mut bad = Vec::new();
    for &a in &interesting() {
        for &b in &interesting().iter().take(24).copied().collect::<Vec<_>>() {
            // imul r64, r64
            let (hw, flags): (u64, u64);
            unsafe {
                let mut x = a;
                let f: u64;
                asm!("imul {x}, {y}", "pushfq", "pop {f}", x = inout(reg) x, y = in(reg) b, f = out(reg) f);
                hw = x;
                flags = f;
            }
            let mut m = M::new(IMGS);
            let r = m.imul(a, b, 8);
            // ZF, SF and PF are undefined after imul; CF = OF is what is defined
            if hw != r || (flags & 1 != 0) != m.cf || (flags & 0x800 != 0) != m.of {
                bad.push(format!("imul64 {a:#x} {b:#x}: hw {hw:#x} cf {} of {} model {r:#x} cf {} of {}", flags & 1, flags & 0x800 != 0, m.cf, m.of));
            }
        }
        // neg, inc, dec on 32 bits
        for (name, hw_f) in [("neg", 0), ("inc", 1), ("dec", 2)] {
            let (hw, flags): (u32, u64);
            unsafe {
                let mut x = a as u32;
                let f: u64;
                match hw_f {
                    0 => asm!("neg {x:e}", "pushfq", "pop {f}", x = inout(reg) x, f = out(reg) f),
                    1 => asm!("inc {x:e}", "pushfq", "pop {f}", x = inout(reg) x, f = out(reg) f),
                    _ => asm!("dec {x:e}", "pushfq", "pop {f}", x = inout(reg) x, f = out(reg) f),
                }
                hw = x;
                flags = f;
            }
            let mut m = M::new(IMGS);
            m.cf = false;
            let r = match name {
                "neg" => m.neg(a, 4),
                "inc" => m.inc(a, 4),
                _ => m.dec(a, 4),
            } as u32;
            let mut hwf = flags_bits(flags);
            let mf = [m.cf, m.pf, m.zf, m.sf, m.of];
            // inc/dec leave CF as it was (here: clear, since the asm starts from `xor`-free flags we do not control)
            if name != "neg" {
                hwf[0] = false;
            }
            let mut mf = mf;
            if name != "neg" {
                mf[0] = false;
            }
            if hw != r || hwf != mf {
                bad.push(format!("{name} {a:#x}: hw {hw:#x} {hwf:?} model {r:#x} {mf:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{} differ, e.g.\n{}", bad.len(), bad.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}
