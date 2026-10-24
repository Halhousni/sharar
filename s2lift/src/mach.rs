//! The registers, flags and memory that the translated maths routines (`msvcrt_gen.rs`) act on.
//!
//! `tools/lift_libm.py` turns each instruction of msvcr110's `sin`, `exp`, `pow` and the rest into
//! one call on [`M`]; this file is what those calls mean. Only what those routines use is here:
//! scalar and packed-bit SSE2 on the XMM registers, 64/32-bit integer arithmetic, and a stack.
//! Nothing here rounds or reorders anything, which is the point: the answers are the DLL's, to
//! the last bit.

/// The read-only part of a DLL that translated code reads: its constant tables, as ranges of
/// addresses (start, offset in `blob`, length).
pub struct Img {
    pub blob: &'static [u8],
    pub ranges: &'static [(u64, u32, u32)],
}

pub const RAX: usize = 0;
pub const RCX: usize = 1;
pub const RDX: usize = 2;
pub const RSP: usize = 4;

const STACK: usize = 16384;
const STACK_BASE: u64 = 0x7fff_0000_0000;
const HEAP_BASE: u64 = 0x6000_0000_0000;

pub struct M {
    pub r: [u64; 16],
    pub x: [u128; 16],
    pub zf: bool,
    pub sf: bool,
    pub cf: bool,
    pub of: bool,
    pub pf: bool,
    stack: [u8; STACK],
    img: &'static Img,
    /// Memory handed to the code: arrays it reads and writes. Addresses start at HEAP_BASE.
    heap: Vec<u8>,
    /// Image ranges the code has written to (its own static data), copied on the first write.
    over: Vec<Option<Vec<u8>>>,
}

fn mask(size: u32) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

fn sign(v: u64, size: u32) -> bool {
    v >> (size * 8 - 1) & 1 != 0
}

fn sext(v: u64, size: u32) -> i64 {
    let s = 64 - size * 8;
    ((v << s) as i64) >> s
}

impl M {
    pub fn new(img: &'static Img) -> M {
        let mut m = M { r: [0; 16], x: [0; 16], zf: false, sf: false, cf: false, of: false, pf: false, stack: [0; STACK], img, heap: Vec::new(), over: vec![None; img.ranges.len()] };
        m.r[RSP] = STACK_BASE + STACK as u64 - 0x100;
        m
    }

    // ---- memory ------------------------------------------------------------------------

    fn bytes(&self, a: u64, n: usize) -> &[u8] {
        if a >= STACK_BASE && a + n as u64 <= STACK_BASE + STACK as u64 {
            let o = (a - STACK_BASE) as usize;
            return &self.stack[o..o + n];
        }
        if a >= HEAP_BASE && a + n as u64 <= HEAP_BASE + self.heap.len() as u64 {
            let o = (a - HEAP_BASE) as usize;
            return &self.heap[o..o + n];
        }
        for (k, &(start, off, len)) in self.img.ranges.iter().enumerate() {
            if a >= start && a + n as u64 <= start + len as u64 {
                if let Some(v) = &self.over[k] {
                    let o = (a - start) as usize;
                    return &v[o..o + n];
                }
                let o = off as usize + (a - start) as usize;
                return &self.img.blob[o..o + n];
            }
        }
        panic!("msvcrt: read of unmapped address {a:#x}");
    }

    pub fn ld(&self, a: u64, n: u32) -> u64 {
        let b = self.bytes(a, n as usize);
        let mut v = 0u64;
        for (i, &c) in b.iter().enumerate() {
            v |= (c as u64) << (8 * i);
        }
        v
    }

    pub fn ld128(&self, a: u64) -> u128 {
        let b = self.bytes(a, 16);
        let mut v = 0u128;
        for (i, &c) in b.iter().enumerate() {
            v |= (c as u128) << (8 * i);
        }
        v
    }

    pub fn st(&mut self, a: u64, n: u32, val: u64) {
        let v = val;
        if a >= HEAP_BASE && a + n as u64 <= HEAP_BASE + self.heap.len() as u64 {
            let o = (a - HEAP_BASE) as usize;
            for i in 0..n as usize {
                self.heap[o + i] = (v >> (8 * i)) as u8;
            }
            return;
        }
        if a < STACK_BASE {
            for (k, &(start, off, len)) in self.img.ranges.iter().enumerate() {
                if a >= start && a + n as u64 <= start + len as u64 {
                    let v = self.over[k].get_or_insert_with(|| self.img.blob[off as usize..(off + len) as usize].to_vec());
                    let o = (a - start) as usize;
                    for i in 0..n as usize {
                        v[o + i] = (val >> (8 * i)) as u8;
                    }
                    return;
                }
            }
        }
        assert!(a >= STACK_BASE && a + n as u64 <= STACK_BASE + STACK as u64, "translated code wrote outside its memory at {a:#x}");
        let o = (a - STACK_BASE) as usize;
        for i in 0..n as usize {
            self.stack[o + i] = (v >> (8 * i)) as u8;
        }
    }

    /// Give the code `n` zeroed bytes (16-aligned) and return their address.
    pub fn alloc(&mut self, n: usize) -> u64 {
        let at = (self.heap.len() + 15) & !15;
        self.heap.resize(at + n, 0);
        HEAP_BASE + at as u64
    }

    pub fn put_f64s(&mut self, a: u64, v: &[f64]) {
        for (i, x) in v.iter().enumerate() {
            self.st(a + 8 * i as u64, 8, x.to_bits());
        }
    }

    pub fn get_f64s(&self, a: u64, n: usize) -> Vec<f64> {
        (0..n).map(|i| f64::from_bits(self.ld(a + 8 * i as u64, 8))).collect()
    }

    /// A path the translation does not follow (error reporting by C++ exceptions and streams).
    /// `memset(rcx, edx, r8)`.
    pub fn memset(&mut self) {
        let (d, c, n) = (self.r[RCX], self.r[RDX] as u8, self.r[8]);
        for i in 0..n {
            self.st(d + i, 1, c as u64);
        }
        self.r[RAX] = d;
    }

    /// `memcpy`/`memmove(rcx, rdx, r8)`.
    pub fn memcpy(&mut self) {
        let (d, s, n) = (self.r[RCX], self.r[RDX], self.r[8]);
        let bytes: Vec<u8> = (0..n).map(|i| self.ld(s + i, 1) as u8).collect();
        for (i, b) in bytes.iter().enumerate() {
            self.st(d + i as u64, 1, *b as u64);
        }
        self.r[RAX] = d;
    }

    pub fn fail(&self, what: &str) -> ! {
        panic!("translated code reached {what}")
    }

    pub fn st128(&mut self, a: u64, v: u128) {
        self.st(a, 8, v as u64);
        self.st(a + 8, 8, (v >> 64) as u64);
    }

    pub fn ea(&self, base: i32, index: i32, scale: u64, disp: i64) -> u64 {
        let mut a = disp as u64;
        if base >= 0 {
            a = a.wrapping_add(self.r[base as usize]);
        }
        if index >= 0 {
            a = a.wrapping_add(self.r[index as usize].wrapping_mul(scale));
        }
        a
    }

    // ---- registers ---------------------------------------------------------------------

    /// Read general register `i` at `size` bytes; `high` selects ah/ch/dh/bh.
    pub fn gr(&self, i: usize, size: u32, high: bool) -> u64 {
        if high {
            return (self.r[i] >> 8) & 0xff;
        }
        self.r[i] & mask(size)
    }

    pub fn sr(&mut self, i: usize, size: u32, high: bool, v: u64) {
        if high {
            self.r[i] = self.r[i] & !0xff00 | (v & 0xff) << 8;
            return;
        }
        self.r[i] = match size {
            8 => v,
            4 => v & 0xffff_ffff,
            _ => self.r[i] & !mask(size) | v & mask(size),
        };
    }

    pub fn xl(&self, i: usize) -> u64 {
        self.x[i] as u64
    }

    pub fn set_xl(&mut self, i: usize, v: u64) {
        self.x[i] = self.x[i] & !(u64::MAX as u128) | v as u128;
    }

    /// `movsd xmm, m64` and `movq`: loads the low half and clears the high.
    pub fn set_xl_zero(&mut self, i: usize, v: u64) {
        self.x[i] = v as u128;
    }

    fn d(v: u64) -> f64 {
        f64::from_bits(v)
    }

    // ---- scalar double arithmetic ------------------------------------------------------

    pub fn addsd(&mut self, i: usize, s: u64) {
        let v = Self::d(self.xl(i)) + Self::d(s);
        self.set_xl(i, v.to_bits());
    }
    pub fn subsd(&mut self, i: usize, s: u64) {
        let v = Self::d(self.xl(i)) - Self::d(s);
        self.set_xl(i, v.to_bits());
    }
    pub fn mulsd(&mut self, i: usize, s: u64) {
        let v = Self::d(self.xl(i)) * Self::d(s);
        self.set_xl(i, v.to_bits());
    }
    pub fn divsd(&mut self, i: usize, s: u64) {
        let v = Self::d(self.xl(i)) / Self::d(s);
        self.set_xl(i, v.to_bits());
    }
    pub fn sqrtsd(&mut self, i: usize, s: u64) {
        self.set_xl(i, Self::d(s).sqrt().to_bits());
    }

    /// `comisd`/`ucomisd`: unordered sets ZF, PF and CF.
    pub fn comisd(&mut self, a: u64, b: u64) {
        let (a, b) = (Self::d(a), Self::d(b));
        self.of = false;
        self.sf = false;
        if a.is_nan() || b.is_nan() {
            self.zf = true;
            self.pf = true;
            self.cf = true;
        } else {
            self.pf = false;
            self.zf = a == b;
            self.cf = a < b;
        }
    }

    pub fn cvttsd2si(&self, s: u64, size: u32) -> u64 {
        let v = Self::d(s);
        if size == 8 {
            if v.is_nan() || v >= 9.223372036854775808e18 || v < -9.223372036854775808e18 {
                0x8000_0000_0000_0000
            } else {
                v as i64 as u64
            }
        } else if v.is_nan() || v >= 2147483648.0 || v <= -2147483649.0 {
            0x8000_0000
        } else {
            v as i32 as u32 as u64
        }
    }

    pub fn cvtsi2sd(&mut self, i: usize, v: u64, size: u32) {
        let f = if size == 8 { v as i64 as f64 } else { v as u32 as i32 as f64 };
        self.set_xl(i, f.to_bits());
    }

    /// `cvtdq2pd xmm, xmm/m64`: two signed 32-bit integers become two doubles.
    pub fn cvtdq2pd(&mut self, i: usize, s: u64) {
        let lo = s as u32 as i32 as f64;
        let hi = (s >> 32) as u32 as i32 as f64;
        self.x[i] = lo.to_bits() as u128 | (hi.to_bits() as u128) << 64;
    }

    /// `cvtpd2dq` (rounding to nearest even) and `cvttpd2dq` (truncating): two doubles become two
    /// 32-bit integers in the low half, the high half cleared.
    pub fn cvtpd2dq(&mut self, i: usize, s: u128, trunc: bool) {
        let conv = |bits: u64| -> u32 {
            let v = Self::d(bits);
            let r = if trunc { v.trunc() } else { v.round_ties_even() };
            if v.is_nan() || r >= 2147483648.0 || r < -2147483648.0 {
                0x8000_0000
            } else {
                r as i32 as u32
            }
        };
        let lo = conv(s as u64);
        let hi = conv((s >> 64) as u64);
        self.x[i] = lo as u128 | (hi as u128) << 32;
    }

    // ---- integer arithmetic and flags --------------------------------------------------

    fn parity(v: u64) -> bool {
        (v as u8).count_ones() % 2 == 0
    }

    fn logic_flags(&mut self, r: u64, size: u32) {
        self.cf = false;
        self.of = false;
        self.zf = r & mask(size) == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
    }

    pub fn add(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let r = a.wrapping_add(b) & mask(size);
        self.cf = r < a & mask(size);
        self.of = sign(a, size) == sign(b, size) && sign(r, size) != sign(a, size);
        self.zf = r == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
        r
    }

    pub fn sub(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let (a, b) = (a & mask(size), b & mask(size));
        let r = a.wrapping_sub(b) & mask(size);
        self.cf = a < b;
        self.of = sign(a, size) != sign(b, size) && sign(r, size) != sign(a, size);
        self.zf = r == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
        r
    }

    pub fn and(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let r = a & b & mask(size);
        self.logic_flags(r, size);
        r
    }
    pub fn or(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let r = (a | b) & mask(size);
        self.logic_flags(r, size);
        r
    }
    pub fn xor(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let r = (a ^ b) & mask(size);
        self.logic_flags(r, size);
        r
    }

    /// `inc` and `dec` leave the carry flag alone.
    pub fn inc(&mut self, a: u64, size: u32) -> u64 {
        let cf = self.cf;
        let r = self.add(a, 1, size);
        self.cf = cf;
        r
    }
    pub fn dec(&mut self, a: u64, size: u32) -> u64 {
        let cf = self.cf;
        let r = self.sub(a, 1, size);
        self.cf = cf;
        r
    }
    pub fn neg(&mut self, a: u64, size: u32) -> u64 {
        self.sub(0, a, size)
    }

    pub fn shl(&mut self, a: u64, n: u64, size: u32) -> u64 {
        let n = (n & if size == 8 { 63 } else { 31 }) as u32;
        if n == 0 {
            return a & mask(size);
        }
        let a = a & mask(size);
        let r = if n >= size * 8 { 0 } else { (a << n) & mask(size) };
        self.cf = n <= size * 8 && (a >> (size * 8 - n)) & 1 != 0;
        self.zf = r == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
        self.of = sign(r, size) != self.cf;
        r
    }

    pub fn shr(&mut self, a: u64, n: u64, size: u32) -> u64 {
        let n = (n & if size == 8 { 63 } else { 31 }) as u32;
        let a = a & mask(size);
        if n == 0 {
            return a;
        }
        let r = a >> n;
        self.cf = (a >> (n - 1)) & 1 != 0;
        self.zf = r == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
        self.of = sign(a, size);
        r
    }

    pub fn sar(&mut self, a: u64, n: u64, size: u32) -> u64 {
        let n = (n & if size == 8 { 63 } else { 31 }) as u32;
        let a = a & mask(size);
        if n == 0 {
            return a;
        }
        let r = (sext(a, size) >> n) as u64 & mask(size);
        self.cf = (sext(a, size) >> (n - 1)) & 1 != 0;
        self.zf = r == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
        self.of = false;
        r
    }

    /// Two-operand and three-operand `imul`: the low half of the signed product; CF and OF say
    /// whether it was cut.
    pub fn imul(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let full = sext(a, size) as i128 * sext(b, size) as i128;
        let r = full as u64 & mask(size);
        let over = full != sext(r, size) as i128;
        self.cf = over;
        self.of = over;
        r
    }

    /// One-operand `imul r/m32`: edx:eax = eax * src.
    pub fn imul1_32(&mut self, src: u64) {
        let full = sext(self.r[RAX], 4) * sext(src, 4);
        self.r[RAX] = full as u64 & 0xffff_ffff;
        self.r[RDX] = (full >> 32) as u64 & 0xffff_ffff;
        let over = full != full as i32 as i64;
        self.cf = over;
        self.of = over;
    }

    /// `mul r/m64`: rdx:rax = rax * src, unsigned.
    pub fn mul64(&mut self, src: u64) {
        let full = self.r[RAX] as u128 * src as u128;
        self.r[RAX] = full as u64;
        self.r[RDX] = (full >> 64) as u64;
        self.cf = self.r[RDX] != 0;
        self.of = self.cf;
    }

    /// `movmskpd`: the sign bits of the two doubles.
    pub fn movmskpd(&self, i: usize) -> u64 {
        (self.x[i] >> 63 & 1) as u64 | (self.x[i] >> 127 & 1) as u64 * 2
    }

    pub fn cc(&self, c: &str) -> bool {
        match c {
            "e" => self.zf,
            "ne" => !self.zf,
            "a" => !self.cf && !self.zf,
            "ae" => !self.cf,
            "b" => self.cf,
            "be" => self.cf || self.zf,
            "g" => !self.zf && self.sf == self.of,
            "ge" => self.sf == self.of,
            "l" => self.sf != self.of,
            "le" => self.zf || self.sf != self.of,
            "s" => self.sf,
            "ns" => !self.sf,
            "o" => self.of,
            "no" => !self.of,
            "p" => self.pf,
            "np" => !self.pf,
            _ => unreachable!("condition {c}"),
        }
    }

    pub fn push(&mut self, v: u64) {
        self.r[RSP] = self.r[RSP].wrapping_sub(8);
        let a = self.r[RSP];
        self.st(a, 8, v);
    }

    pub fn pop(&mut self) -> u64 {
        let a = self.r[RSP];
        let v = self.ld(a, 8);
        self.r[RSP] = a.wrapping_add(8);
        v
    }
}

pub fn sx(v: u64, size: u32) -> u64 {
    sext(v, size) as u64
}
