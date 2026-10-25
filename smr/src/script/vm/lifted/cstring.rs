//! MFC's `CString`, for translated commands that build and take apart text.
//!
//! A `CString` is one pointer to its characters, with a header of 24 bytes in front of them (the
//! string manager, the length, the allocated length, a reference count). Translated code reads
//! the length straight out of the header, so the header is kept as MFC keeps it; every method
//! that changes the text makes a new block (nothing is shared, so nothing is counted).

use s2lift::mach::M;

const HEADER: u64 = 0x18;

/// The nil strings (one per character width) that empty `CString`s point at.
#[derive(Default)]
pub struct State {
    nil: [u64; 2],
    manager: u64,
}

fn width(wide: bool) -> u64 {
    if wide {
        2
    } else {
        1
    }
}

impl State {
    fn nil(&mut self, m: &mut M, wide: bool) -> u64 {
        let k = wide as usize;
        if self.nil[k] == 0 {
            let p = m.alloc(HEADER as usize + 8) + HEADER;
            m.st(p - 8, 4, 0xffff_ffff); // the reference count of a nil string
            self.nil[k] = p;
        }
        self.nil[k]
    }

    fn manager(&mut self, m: &mut M) -> u64 {
        if self.manager == 0 {
            self.manager = m.alloc(64);
        }
        self.manager
    }

    /// The characters of the `CString` at `obj`.
    pub fn read(&self, m: &M, obj: u64, wide: bool) -> Vec<u32> {
        let p = m.ld(obj, 8);
        if p == 0 {
            return Vec::new();
        }
        let len = m.ld(p - 0x10, 4) as u32 as usize;
        (0..len).map(|i| m.ld(p + i as u64 * width(wide), width(wide) as u32) as u32).collect()
    }

    /// Make the `CString` at `obj` hold `chars`.
    pub fn write(&mut self, m: &mut M, obj: u64, chars: &[u32], wide: bool) {
        if chars.is_empty() {
            let p = self.nil(m, wide);
            m.st(obj, 8, p);
            return;
        }
        let w = width(wide);
        let mgr = self.manager(m);
        let p = m.alloc(HEADER as usize + (chars.len() + 1) * w as usize) + HEADER;
        m.st(p - 0x18, 8, mgr);
        m.st(p - 0x10, 4, chars.len() as u64);
        m.st(p - 0x0c, 4, chars.len() as u64);
        m.st(p - 0x08, 4, 1);
        for (i, c) in chars.iter().enumerate() {
            m.st(p + i as u64 * w, w as u32, *c as u64);
        }
        m.st(obj, 8, p);
    }
}

/// A NUL-terminated string in the machine.
fn cstr(m: &M, p: u64, wide: bool) -> Vec<u32> {
    let mut v = Vec::new();
    if p == 0 {
        return v;
    }
    let w = width(wide);
    loop {
        let c = m.ld(p + v.len() as u64 * w, w as u32) as u32;
        if c == 0 {
            return v;
        }
        v.push(c);
    }
}

fn to_units(s: &str) -> Vec<u32> {
    s.encode_utf16().map(|u| u as u32).collect()
}

pub fn from_units(v: &[u32]) -> String {
    String::from_utf16_lossy(&v.iter().map(|&u| u as u16).collect::<Vec<_>>())
}

const WIDE: &str = "?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@";
const NARROW: &str = "?$CStringT@DV?$StrTraitMFC_DLL@DV?$ChTraitsCRT@D@ATL@@@@@ATL@@";

/// Answer a call of an MFC string method. `args` are the registers a Win64 call passes (rcx, rdx,
/// r8, r9) and the machine gives the stack. Returns false if `name` is not one of ours.
pub fn call(st: &mut State, m: &mut M, name: &str) -> bool {
    let (rcx, rdx, r8, r9) = (m.r[1], m.r[2], m.r[8], m.r[9]);
    let stack = |m: &M, k: u64| m.ld(m.r[4] + 0x20 + 8 * k, 8);
    // Which class and which member: the name is `<op>` then the class then the signature.
    let (wide, rest) = if let Some(i) = name.find(WIDE) {
        (true, (&name[..i], &name[i + WIDE.len()..]))
    } else if let Some(i) = name.find(NARROW) {
        (false, (&name[..i], &name[i + NARROW.len()..]))
    } else if name.contains("CSimpleStringT@_W$00@ATL@@") {
        (true, (name.split("CSimpleStringT").next().unwrap(), name.split("ATL@@").nth(1).unwrap_or("")))
    } else if name.contains("CSimpleStringT@D$00@ATL@@") {
        (false, (name.split("CSimpleStringT").next().unwrap(), name.split("ATL@@").nth(1).unwrap_or("")))
    } else {
        return false;
    };
    let (op, sig) = rest;
    let op = op.trim_end_matches(|c| c == '?' || c == '$');
    let cs = |m: &M, s: &State, p: u64| s.read(m, p, wide);
    match (op, sig) {
        // construct
        ("??0", s) if s.starts_with("QEAA@XZ") => {
            let p = st.nil(m, wide);
            m.st(rcx, 8, p);
            m.r[0] = rcx;
        }
        ("??0", s) if s.starts_with("QEAA@PEAUIAtlStringMgr") => {
            let p = st.nil(m, wide);
            m.st(rcx, 8, p);
            m.r[0] = rcx;
        }
        ("??0", s) if s.starts_with("QEAA@AEBV01@@Z") => {
            let v = cs(m, st, rdx);
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("??0", s) if s.starts_with("QEAA@_WH@Z") || s.starts_with("QEAA@DH@Z") => {
            // a character repeated
            let ch = if wide { rdx as u32 & 0xffff } else { rdx as u32 & 0xff };
            let v = vec![ch; (r8 as u32 as i32).max(0) as usize];
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("??0", s) if s.starts_with("QEAA@PEB_W@Z") => {
            let v = cstr(m, rdx, true);
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("??0", s) if s.starts_with("QEAA@PEBD@Z") => {
            let v = cstr(m, rdx, false);
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("??1", _) => {}
        // assign
        ("??4", s) if s.contains("AEBV01@@Z") => {
            let v = cs(m, st, rdx);
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("??4", s) if s.contains("PEB_W@Z") => {
            let v = cstr(m, rdx, true);
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("??4", s) if s.contains("PEBD@Z") => {
            let v = cstr(m, rdx, false);
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        // append
        ("??Y", s) => {
            let mut v = cs(m, st, rcx);
            if s.contains("PEB_W@Z") {
                v.extend(cstr(m, rdx, true));
            } else if s.contains("PEBD@Z") {
                v.extend(cstr(m, rdx, false));
            } else if s.contains("_W@Z") || s.contains("D@Z") {
                v.push(rdx as u32 & if wide { 0xffff } else { 0xff });
            } else {
                // another string by reference
                v.extend(cs(m, st, rdx));
            }
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        // operator PCXSTR
        ("??B", _) => m.r[0] = m.ld(rcx, 8),
        // operator[]
        ("??A", _) => {
            let v = cs(m, st, rcx);
            m.r[0] = v.get(rdx as u32 as i32 as usize).copied().unwrap_or(0) as u64;
        }
        ("?Empty@", _) => {
            let p = st.nil(m, wide);
            m.st(rcx, 8, p);
        }
        // a buffer to write into: the string is copied to a block with room, ReleaseBuffer
        // reads the text back out
        ("?GetBuffer@", s) | ("?GetBufferSetLength@", s) => {
            let mut v = cs(m, st, rcx);
            let want = if s.contains("H@Z") { rdx as u32 as i32 as usize } else { v.len() };
            if op == "?GetBufferSetLength@" {
                v.resize(want, 0);
            }
            let cap = want.max(v.len());
            let w = width(wide);
            let mgr = st.manager(m);
            let p = m.alloc(HEADER as usize + (cap + 1) * w as usize) + HEADER;
            m.st(p - 0x18, 8, mgr);
            m.st(p - 0x10, 4, v.len() as u64);
            m.st(p - 0x0c, 4, cap as u64);
            m.st(p - 0x08, 4, 1);
            for (i, c) in v.iter().enumerate() {
                m.st(p + i as u64 * w, w as u32, *c as u64);
            }
            m.st(rcx, 8, p);
            m.r[0] = p;
        }
        ("?ReleaseBuffer@", _) => {
            let p = m.ld(rcx, 8);
            let w = width(wide);
            let n = rdx as u32 as i32;
            let len = if n < 0 { cstr(m, p, wide).len() } else { n as usize };
            let v: Vec<u32> = (0..len).map(|i| m.ld(p + i as u64 * w, w as u32) as u32).collect();
            st.write(m, rcx, &v, wide);
        }
        ("?ReverseFind@", _) => {
            let v = cs(m, st, rcx);
            let ch = rdx as u32 & if wide { 0xffff } else { 0xff };
            m.r[0] = v.iter().rposition(|&c| c == ch).map(|i| i as u64).unwrap_or(u64::MAX);
        }
        ("?LoadStringW@", _) => {
            // a message from the application's string table (SONVIEW.exe's resources)
            match s2lift::strings_gen::STRINGS.binary_search_by_key(&(rdx as u32), |e| e.0) {
                Ok(i) => {
                    let v: Vec<u32> = s2lift::strings_gen::STRINGS[i].1.encode_utf16().map(|u| u as u32).collect();
                    st.write(m, rcx, &v, wide);
                    m.r[0] = 1;
                }
                Err(_) => m.r[0] = 0,
            }
        }
        ("?Format@", _) | ("?AppendFormat@", _) => {
            let fmt = cstr(m, rdx, wide);
            let mut next = 0u64;
            let mut arg = |m: &M| -> u64 {
                let v = match next {
                    0 => r8,
                    1 => r9,
                    k => stack(m, k - 2),
                };
                next += 1;
                v
            };
            let out = format_any(m, &fmt, &mut arg, !wide);
            if std::env::var_os("S2LIFT_SERVICES").is_some() {
                eprintln!("  format {:?} -> {:?}", from_units(&fmt), from_units(&out));
            }
            let mut v = if op == "?AppendFormat@" { cs(m, st, rcx) } else { Vec::new() };
            v.extend(out);
            st.write(m, rcx, &v, wide);
        }
        ("?Left@", _) => {
            let v = cs(m, st, rcx);
            let n = (r8 as u32 as i32).clamp(0, v.len() as i32) as usize;
            st.write(m, rdx, &v[..n], wide);
            m.r[0] = rdx;
        }
        ("?Right@", _) => {
            let v = cs(m, st, rcx);
            let n = (r8 as u32 as i32).clamp(0, v.len() as i32) as usize;
            st.write(m, rdx, &v[v.len() - n..], wide);
            m.r[0] = rdx;
        }
        ("?Mid@", s) => {
            let v = cs(m, st, rcx);
            let start = (r8 as u32 as i32).clamp(0, v.len() as i32) as usize;
            let end = if s.contains("HH@Z") {
                (start + (r9 as u32 as i32).max(0) as usize).min(v.len())
            } else {
                v.len()
            };
            st.write(m, rdx, &v[start..end], wide);
            m.r[0] = rdx;
        }
        ("?Find@", s) => {
            let v = cs(m, st, rcx);
            let start = (r8 as u32 as i32).max(0) as usize;
            let hay = &v[start.min(v.len())..];
            let pos = if s.starts_with("QEBAH_W") || s.starts_with("QEBAHDH") || s.starts_with("QEBAHD") {
                hay.iter().position(|&c| c == (rdx as u32 & if wide { 0xffff } else { 0xff }))
            } else {
                let sub = cstr(m, rdx, wide);
                if sub.is_empty() { Some(0) } else { hay.windows(sub.len()).position(|w| w == &sub[..]) }
            };
            m.r[0] = pos.map(|p| (p + start) as i64).unwrap_or(-1) as u64;
        }
        ("?TrimLeft@", s) | ("?TrimRight@", s) | ("?Trim@", s) => {
            let mut v = cs(m, st, rcx);
            let set: Vec<u32> = if s.contains("PEB_W@Z") {
                cstr(m, rdx, true)
            } else if s.contains("_W@Z") || s.contains("D@Z") {
                vec![rdx as u32 & if wide { 0xffff } else { 0xff }]
            } else {
                vec![9, 10, 11, 12, 13, 32, 0x85, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000]
            };
            if op != "?TrimRight@" {
                let n = v.iter().take_while(|c| set.contains(c)).count();
                v.drain(..n);
            }
            if op != "?TrimLeft@" {
                while v.last().map_or(false, |c| set.contains(c)) {
                    v.pop();
                }
            }
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("?MakeUpper@", _) | ("?MakeLower@", _) => {
            let v: Vec<u32> = cs(m, st, rcx)
                .into_iter()
                .map(|c| match char::from_u32(c) {
                    Some(ch) if c < 128 => {
                        (if op == "?MakeUpper@" { ch.to_ascii_uppercase() } else { ch.to_ascii_lowercase() }) as u32
                    }
                    _ => c,
                })
                .collect();
            st.write(m, rcx, &v, wide);
            m.r[0] = rcx;
        }
        ("?Compare@", _) | ("?CompareNoCase@", _) => {
            let a = cs(m, st, rcx);
            let b = cstr(m, rdx, wide);
            let fold = |v: &[u32]| -> Vec<u32> {
                v.iter().map(|&c| if op == "?CompareNoCase@" && c < 128 { (c as u8 as char).to_ascii_lowercase() as u32 } else { c }).collect()
            };
            m.r[0] = match fold(&a).cmp(&fold(&b)) {
                std::cmp::Ordering::Less => -1i64 as u64,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
        }
        ("?Concatenate@", _) => {
            // (dest, src1, len1, src2, len2)
            let (l1, l2) = (r8 as u32 as usize, stack(m, 0) as u32 as usize);
            let w = width(wide);
            let mut v: Vec<u32> = (0..l1).map(|i| m.ld(rdx + i as u64 * w, w as u32) as u32).collect();
            v.extend((0..l2).map(|i| m.ld(r9 + i as u64 * w, w as u32) as u32));
            if std::env::var_os("S2LIFT_SERVICES").is_some() {
                eprintln!("  concatenate {:?} (+{l1}, +{l2})", from_units(&v));
            }
            st.write(m, rcx, &v, wide);
        }
        ("?FindOneOf@", _) => {
            let v = cs(m, st, rcx);
            let set = cstr(m, rdx, wide);
            m.r[0] = v.iter().position(|c| set.contains(c)).map(|p| p as i64).unwrap_or(-1) as u64;
        }
        ("?Delete@", _) => {
            // Delete(index, count): returns the new length
            let mut v = cs(m, st, rcx);
            let at = (rdx as u32 as i32).max(0) as usize;
            let n = (r8 as u32 as i32).max(0) as usize;
            if at < v.len() {
                v.drain(at..(at + n).min(v.len()));
            }
            st.write(m, rcx, &v, wide);
            m.r[0] = v.len() as u64;
        }
        ("?GetManager@", _) => m.r[0] = st.manager(m),
        _ => return false,
    }
    true
}

/// A wide `printf`: `%d %i %u %x %X %o %c %s (wide) %hs/%S (narrow) %f %e %g %p %%` with flags,
/// width and precision. `arg` hands out the next argument slot.
pub(super) fn format_wide(m: &M, fmt: &[u32], arg: &mut dyn FnMut(&M) -> u64) -> Vec<u32> {
    format_any(m, fmt, arg, false)
}

/// `printf` over a format held as characters; `narrow` says it is the narrow-character function,
/// where `%s` is a string of bytes and `%S` one of wide characters (it is the other way round for `wprintf`).
pub(super) fn format_any(m: &M, fmt: &[u32], arg: &mut dyn FnMut(&M) -> u64, narrow_fn: bool) -> Vec<u32> {
    let r = format_any_(m, fmt, arg, narrow_fn);
    if std::env::var_os("S2LIFT_SERVICES").is_some() {
        eprintln!("  (format {:?} gives {:?})", from_units(fmt), from_units(&r));
    }
    r
}

fn format_any_(m: &M, fmt: &[u32], arg: &mut dyn FnMut(&M) -> u64, narrow_fn: bool) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != '%' as u32 {
            out.push(c);
            continue;
        }
        let mut flags = String::new();
        while i < fmt.len() && "-+ #0".contains(char::from_u32(fmt[i]).unwrap_or(' ')) {
            flags.push(char::from_u32(fmt[i]).unwrap());
            i += 1;
        }
        let mut width = String::new();
        while i < fmt.len() && (fmt[i] as u8 as char).is_ascii_digit() {
            width.push(fmt[i] as u8 as char);
            i += 1;
        }
        if i < fmt.len() && fmt[i] == '*' as u32 {
            let w = arg(m) as u32 as i32;
            if w < 0 {
                flags.push('-');
            }
            width = w.abs().to_string();
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < fmt.len() && fmt[i] == '.' as u32 {
            i += 1;
            let mut p = String::new();
            if i < fmt.len() && fmt[i] == '*' as u32 {
                let v = arg(m) as u32 as i32;
                prec = if v < 0 { None } else { Some(v as usize) };
                i += 1;
            } else {
                while i < fmt.len() && (fmt[i] as u8 as char).is_ascii_digit() {
                    p.push(fmt[i] as u8 as char);
                    i += 1;
                }
                prec = Some(p.parse().unwrap_or(0));
            }
        }
        let mut size = String::new();
        while i < fmt.len() && "hlIL64w".contains(char::from_u32(fmt[i]).unwrap_or(' ')) {
            size.push(char::from_u32(fmt[i]).unwrap());
            i += 1;
        }
        let Some(&t) = fmt.get(i) else { break };
        i += 1;
        let t = char::from_u32(t).unwrap_or('?');
        let wide64 = size.contains("ll") || size.contains("I64") || size.contains('L');
        let text: String = match t {
            '%' => "%".into(),
            'd' | 'i' => {
                let v = arg(m);
                let n = if wide64 { v as i64 } else { v as u32 as i32 as i64 };
                let mut s = n.abs().to_string();
                if let Some(p) = prec {
                    while s.len() < p {
                        s.insert(0, '0');
                    }
                }
                if n < 0 { format!("-{s}") } else if flags.contains('+') { format!("+{s}") } else if flags.contains(' ') { format!(" {s}") } else { s }
            }
            'u' => (if wide64 { arg(m) } else { arg(m) as u32 as u64 }).to_string(),
            'x' => format!("{:x}", if wide64 { arg(m) } else { arg(m) as u32 as u64 }),
            'X' => format!("{:X}", if wide64 { arg(m) } else { arg(m) as u32 as u64 }),
            'o' => format!("{:o}", if wide64 { arg(m) } else { arg(m) as u32 as u64 }),
            'c' => char::from_u32(arg(m) as u32 & if narrow_fn { 0xff } else { 0xffff }).unwrap_or('?').to_string(),
            's' | 'S' => {
                let p = arg(m);
                let narrow = (t == 'S') != narrow_fn || size.contains('h');
                let v = if p == 0 { to_units("(null)") } else { cstr(m, p, !narrow) };
                let mut s = from_units(&v);
                if let Some(pr) = prec {
                    s = s.chars().take(pr).collect();
                }
                s
            }
            'f' | 'e' | 'E' | 'g' | 'G' => {
                let x = f64::from_bits(arg(m));
                crate::script::vm::printf::format_real(x, t, prec, &flags)
            }
            'p' => format!("{:016X}", arg(m)),
            other => format!("%{other}"),
        };
        let w: usize = width.parse().unwrap_or(0);
        let mut chars: Vec<u32> = to_units(&text);
        if chars.len() < w {
            let pad = w - chars.len();
            if flags.contains('-') {
                chars.extend(std::iter::repeat(' ' as u32).take(pad));
            } else {
                let fill = if flags.contains('0') && "diuxXofeEgG".contains(t) { '0' } else { ' ' } as u32;
                let sign = chars.first().map_or(false, |&c| c == '-' as u32 || c == '+' as u32) && fill == '0' as u32;
                let at = if sign { 1 } else { 0 };
                for _ in 0..pad {
                    chars.insert(at, fill);
                }
            }
        }
        out.extend(chars);
    }
    out
}
