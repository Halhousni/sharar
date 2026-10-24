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
    /// Thread-local data: the template, the zeroes after it and the slot the module uses.
    pub tls: &'static [u8],
    pub tls_zero: u32,
    pub tls_slot: u32,
}

pub const RAX: usize = 0;
pub const RCX: usize = 1;
pub const RDX: usize = 2;
pub const RSP: usize = 4;

const STACK_SMALL: usize = 32 * 1024;
const STACK_BIG: usize = 4 * 1024 * 1024;
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
    stack: Vec<u8>,
    /// The address of the thread information block, which `gs:` addresses.
    pub teb: u64,
    imgs: &'static [&'static Img],
    /// Memory handed to the code: arrays it reads and writes. Addresses start at HEAP_BASE.
    heap: Vec<u8>,
    free_blocks: std::collections::HashMap<usize, Vec<u64>>,
    /// Image ranges the code has written to (its own static data), copied on the first write.
    over: Vec<Vec<Option<Vec<u8>>>>,
    /// The last blocks entered, newest last, for the message when the code goes wrong.
    recent: [u64; 8],
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
    pub fn new(imgs: &'static [&'static Img]) -> M {
        Self::with_stack(imgs, STACK_SMALL)
    }

    /// A machine with room for the large stack frames the data file engine uses.
    pub fn big(imgs: &'static [&'static Img]) -> M {
        Self::with_stack(imgs, STACK_BIG)
    }

    pub fn with_stack(imgs: &'static [&'static Img], stack: usize) -> M {
        let mut m = M { r: [0; 16], x: [0; 16], zf: false, sf: false, cf: false, of: false, pf: false, stack: vec![0; stack], teb: 0, imgs, heap: Vec::new(), free_blocks: Default::default(), over: imgs.iter().map(|i| vec![None; i.ranges.len()]).collect(), recent: [0; 8] };
        m.r[RSP] = STACK_BASE + stack as u64 - 0x100;
        // The thread information block: self pointer, stack limits, the table of thread-local
        // blocks (one slot per module, which `Img::tls_slot` names).
        let teb = m.alloc(0x1000);
        m.teb = teb;
        m.st(teb + 0x30, 8, teb);
        m.st(teb + 0x08, 8, STACK_BASE + stack as u64);
        m.st(teb + 0x10, 8, STACK_BASE);
        let table = m.alloc(8 * 64);
        m.st(teb + 0x58, 8, table);
        for img in imgs {
            if !img.tls.is_empty() || img.tls_zero != 0 {
                let block = m.alloc(img.tls.len() + img.tls_zero as usize + 16);
                for (i, b) in img.tls.iter().enumerate() {
                    m.st(block + i as u64, 1, *b as u64);
                }
                m.st(table + 8 * img.tls_slot as u64, 8, block);
            }
        }
        m
    }

    /// Run `f` with a module's static data in view. Every module's addresses are its own, so the
    /// machine sees all of them at once and this only calls `f`.
    pub fn with_img<R>(&mut self, _img: &'static Img, f: impl FnOnce(&mut M) -> R) -> R {
        f(self)
    }

    // ---- memory ------------------------------------------------------------------------

    fn bytes(&self, a: u64, n: usize) -> &[u8] {
        if a >= STACK_BASE && a + n as u64 <= STACK_BASE + self.stack.len() as u64 {
            let o = (a - STACK_BASE) as usize;
            return &self.stack[o..o + n];
        }
        if a >= HEAP_BASE && a + n as u64 <= HEAP_BASE + self.heap.len() as u64 {
            let o = (a - HEAP_BASE) as usize;
            return &self.heap[o..o + n];
        }
        for (im, img) in self.imgs.iter().enumerate() {
            for (k, &(start, off, len)) in img.ranges.iter().enumerate() {
                if a >= start && a + n as u64 <= start + len as u64 {
                    if let Some(v) = &self.over[im][k] {
                        let o = (a - start) as usize;
                        return &v[o..o + n];
                    }
                    let o = off as usize + (a - start) as usize;
                    return &img.blob[o..o + n];
                }
            }
        }
        panic!("translated code read unmapped address {a:#x} (blocks {:x?}, registers {:x?})", self.recent, self.r);
    }

    pub fn ld(&self, a: u64, n: u32) -> u64 {
        // Reading just past the end of the heap, as code that looks one element beyond an array
        // does, finds zeros, as it does in a process whose heap has not yet grown there.
        if a >= HEAP_BASE && a < HEAP_BASE + (1 << 40) && a + n as u64 > HEAP_BASE + self.heap.len() as u64 {
            let mut v = 0u64;
            for i in 0..n as u64 {
                let at = a + i;
                if at < HEAP_BASE + self.heap.len() as u64 {
                    v |= (self.heap[(at - HEAP_BASE) as usize] as u64) << (8 * i);
                }
            }
            return v;
        }
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
            for (im, img) in self.imgs.iter().enumerate() {
                for (k, &(start, off, len)) in img.ranges.iter().enumerate() {
                    if a >= start && a + n as u64 <= start + len as u64 {
                        let v = self.over[im][k].get_or_insert_with(|| img.blob[off as usize..(off + len) as usize].to_vec());
                        let o = (a - start) as usize;
                        for i in 0..n as usize {
                            v[o + i] = (val >> (8 * i)) as u8;
                        }
                        return;
                    }
                }
            }
        }
        assert!(
            a >= STACK_BASE && a + n as u64 <= STACK_BASE + self.stack.len() as u64,
            "translated code wrote outside its memory at {a:#x} (blocks {:x?}, registers {:x?})",
            self.recent,
            self.r
        );
        let o = (a - STACK_BASE) as usize;
        for i in 0..n as usize {
            self.stack[o + i] = (v >> (8 * i)) as u8;
        }
    }

    /// Start a new call on the stack: forget what the last one left.
    pub fn reset_stack(&mut self) {
        self.r[RSP] = STACK_BASE + self.stack.len() as u64 - 0x100;
    }

    /// Give the code `n` zeroed bytes (16-aligned) and return their address.
    pub fn alloc(&mut self, n: usize) -> u64 {
        // Blocks carry their size in the 16 bytes before them, and `free` keeps them by size so
        // that a program that frees what it allocates does not grow the heap.
        let size = (n.max(16) + 15) & !15;
        if let Some(p) = self.free_blocks.get_mut(&size).and_then(|v| v.pop()) {
            let o = (p - HEAP_BASE) as usize;
            for b in &mut self.heap[o..o + size] {
                *b = 0;
            }
            return p;
        }
        let at = (self.heap.len() + 15) & !15;
        self.heap.resize(at + 16 + size, 0);
        let p = HEAP_BASE + at as u64 + 16;
        self.heap[at..at + 8].copy_from_slice(&(size as u64).to_le_bytes());
        p
    }

    /// Return a block from `alloc` for reuse. Anything else (null, a block not from the heap) is
    /// ignored.
    pub fn free(&mut self, p: u64) {
        if p < HEAP_BASE + 16 || p >= HEAP_BASE + self.heap.len() as u64 {
            return;
        }
        let o = (p - HEAP_BASE) as usize;
        let size = u64::from_le_bytes(self.heap[o - 16..o - 8].try_into().unwrap()) as usize;
        self.free_blocks.entry(size).or_default().push(p);
    }

    /// The size a block was asked for, rounded up to 16.
    pub fn block_size(&self, p: u64) -> usize {
        let o = (p - HEAP_BASE) as usize;
        u64::from_le_bytes(self.heap[o - 16..o - 8].try_into().unwrap()) as usize
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

    /// With S2LIFT_CALLS set, print each routine entered and the registers it was given.
    pub fn trace_enter(&self, at: u64) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ON.get_or_init(|| std::env::var_os("S2LIFT_CALLS").is_some()) {
            eprintln!(
                "enter {at:#x} rcx={:#x} rdx={:#x} r8={:#x} r9={:#x} rsp={:#x}",
                self.r[1], self.r[2], self.r[8], self.r[9], self.r[4]
            );
        }
    }

    /// With S2LIFT_BLOCKS set, print each basic block entered.
    pub fn block(&mut self, at: u64) {
        self.recent.rotate_left(1);
        self.recent[7] = at;
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ON.get_or_init(|| std::env::var_os("S2LIFT_BLOCKS").is_some()) {
            eprintln!("  block {at:#x} rbx={:#x} rsp={:#x}", self.r[3], self.r[4]);
        }
    }

    pub fn fail(&self, what: &str) -> ! {
        panic!("translated code reached {what} (blocks {:x?})", self.recent)
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

    pub fn adc(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let c = self.cf as u64;
        let (a, b) = (a & mask(size), b & mask(size));
        let full = a as u128 + b as u128 + c as u128;
        let r = full as u64 & mask(size);
        self.cf = full >> (size * 8) != 0;
        self.of = sign(a, size) == sign(b, size) && sign(r, size) != sign(a, size);
        self.zf = r == 0;
        self.sf = sign(r, size);
        self.pf = Self::parity(r);
        r
    }

    pub fn sbb(&mut self, a: u64, b: u64, size: u32) -> u64 {
        let c = self.cf as u64;
        let (a, b) = (a & mask(size), b & mask(size));
        let r = a.wrapping_sub(b).wrapping_sub(c) & mask(size);
        self.cf = (a as u128) < b as u128 + c as u128;
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

    /// `idiv`/`div r/m` of 4 or 8 bytes: rdx:rax by the operand, quotient in rax, remainder in rdx.
    /// A zero divisor, or a quotient that does not fit, is the hardware's divide error.
    pub fn idiv(&mut self, src: u64, size: u32) {
        if size == 4 {
            let n = ((self.r[RDX] as u32 as u64) << 32 | self.r[RAX] as u32 as u64) as i64;
            let d = src as u32 as i32 as i64;
            if d == 0 || n / d > i32::MAX as i64 || n / d < i32::MIN as i64 {
                panic!("divide error");
            }
            self.r[RAX] = (n / d) as i32 as u32 as u64;
            self.r[RDX] = (n % d) as i32 as u32 as u64;
        } else {
            let n = ((self.r[RDX] as u128) << 64 | self.r[RAX] as u128) as i128;
            let d = src as i64 as i128;
            if d == 0 || n / d > i64::MAX as i128 || n / d < i64::MIN as i128 {
                panic!("divide error");
            }
            self.r[RAX] = (n / d) as i64 as u64;
            self.r[RDX] = (n % d) as i64 as u64;
        }
    }

    pub fn div(&mut self, src: u64, size: u32) {
        if size == 4 {
            let n = (self.r[RDX] as u32 as u64) << 32 | self.r[RAX] as u32 as u64;
            let d = src as u32 as u64;
            if d == 0 || n / d > u32::MAX as u64 {
                panic!("divide error");
            }
            self.r[RAX] = n / d;
            self.r[RDX] = n % d;
        } else {
            let n = (self.r[RDX] as u128) << 64 | self.r[RAX] as u128;
            let d = src as u128;
            if d == 0 || n / d > u64::MAX as u128 {
                panic!("divide error");
            }
            self.r[RAX] = (n / d) as u64;
            self.r[RDX] = (n % d) as u64;
        }
    }

    pub fn imul1_64(&mut self, src: u64) {
        let full = self.r[RAX] as i64 as i128 * src as i64 as i128;
        self.r[RAX] = full as u64;
        self.r[RDX] = (full >> 64) as u64;
        let over = full != full as i64 as i128;
        self.cf = over;
        self.of = over;
    }

    pub fn mul32(&mut self, src: u64) {
        let full = (self.r[RAX] as u32 as u64) * (src as u32 as u64);
        self.r[RAX] = full & 0xffff_ffff;
        self.r[RDX] = full >> 32;
        self.cf = self.r[RDX] != 0;
        self.of = self.cf;
    }

    fn f(v: u64) -> f32 {
        f32::from_bits(v as u32)
    }

    fn set_ss(&mut self, i: usize, v: f32) {
        self.x[i] = self.x[i] & !0xffff_ffffu128 | v.to_bits() as u128;
    }

    pub fn addss(&mut self, i: usize, s: u64) {
        let v = Self::f(self.xl(i)) + Self::f(s);
        self.set_ss(i, v);
    }
    pub fn subss(&mut self, i: usize, s: u64) {
        let v = Self::f(self.xl(i)) - Self::f(s);
        self.set_ss(i, v);
    }
    pub fn mulss(&mut self, i: usize, s: u64) {
        let v = Self::f(self.xl(i)) * Self::f(s);
        self.set_ss(i, v);
    }
    pub fn divss(&mut self, i: usize, s: u64) {
        let v = Self::f(self.xl(i)) / Self::f(s);
        self.set_ss(i, v);
    }
    pub fn sqrtss(&mut self, i: usize, s: u64) {
        self.set_ss(i, Self::f(s).sqrt());
    }

    pub fn comiss(&mut self, a: u64, b: u64) {
        self.comisd((Self::f(a) as f64).to_bits(), (Self::f(b) as f64).to_bits());
    }

    pub fn cvtsi2ss(&mut self, i: usize, v: u64, size: u32) {
        let f = if size == 8 { v as i64 as f32 } else { v as u32 as i32 as f32 };
        self.set_ss(i, f);
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
