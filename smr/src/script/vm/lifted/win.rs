//! The operating system and C library routines translated code calls: files, time, memory,
//! text. Nothing here knows about scripts; each routine does what Windows or the C runtime does,
//! to the machine's memory. A call arrives as it would on Windows: arguments in rcx, rdx, r8, r9
//! and then on the stack at [rsp + 0x20], and the answer goes in rax (or xmm0).

use s2lift::mach::M;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

/// State of the operating-system layer: open files and the last error.
#[derive(Default)]
pub struct Win {
    files: HashMap<u64, std::fs::File>,
    next: u64,
    last_error: u32,
    errno_cell: u64,
    /// Sizes of blocks handed out by `malloc`, kept in front of the block.
    started: Option<std::time::Instant>,
}

const INVALID_HANDLE: u64 = u64::MAX;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_ACCESS_DENIED: u32 = 5;

fn wide(m: &M, p: u64) -> Vec<u16> {
    let mut v = Vec::new();
    if p == 0 {
        return v;
    }
    while m.ld(p + 2 * v.len() as u64, 2) != 0 {
        v.push(m.ld(p + 2 * v.len() as u64, 2) as u16);
    }
    v
}

fn path(m: &M, p: u64) -> std::path::PathBuf {
    let s = String::from_utf16_lossy(&wide(m, p));
    // Windows paths: the drive and the back slashes mean nothing here
    let s = s.replace('\\', "/");
    let s = match s.strip_prefix(|c: char| c.is_ascii_alphabetic()) {
        Some(r) if r.starts_with(":/") => format!("/{}", &r[2..]),
        _ => s,
    };
    std::path::PathBuf::from(s)
}

fn put_wide(m: &mut M, at: u64, s: &str) {
    for (i, u) in s.encode_utf16().enumerate() {
        m.st(at + 2 * i as u64, 2, u as u64);
    }
    m.st(at + 2 * s.encode_utf16().count() as u64, 2, 0);
}

fn ticks() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
}

impl Win {
    fn error(&mut self, e: &std::io::Error) {
        self.last_error = match e.kind() {
            std::io::ErrorKind::NotFound => ERROR_FILE_NOT_FOUND,
            std::io::ErrorKind::PermissionDenied => ERROR_ACCESS_DENIED,
            _ => 31,
        };
    }

    /// Answer a call of `name`; false if it is not one of ours.
    pub fn call(&mut self, m: &mut M, name: &str) -> bool {
        let (rcx, rdx, r8, r9) = (m.r[1], m.r[2], m.r[8], m.r[9]);
        let stack = |m: &M, k: u64| m.ld(m.r[4] + 0x20 + 8 * k, 8);
        match name {
            // ---- files ------------------------------------------------------------------
            "CreateFileW" => {
                let (access, disposition) = (rdx as u32, stack(m, 0) as u32);
                let p = path(m, rcx);
                let write = access & 0x4000_0000 != 0;
                let mut o = std::fs::OpenOptions::new();
                o.read(access & 0x8000_0000 != 0 || !write);
                match disposition {
                    1 => o.write(true).create_new(true),             // CREATE_NEW
                    2 => o.write(true).create(true).truncate(true),  // CREATE_ALWAYS
                    3 => o.write(write),                              // OPEN_EXISTING
                    4 => o.write(true).create(true),                 // OPEN_ALWAYS
                    5 => o.write(true).truncate(true),               // TRUNCATE_EXISTING
                    _ => o.write(write),
                };
                match o.open(&p) {
                    Ok(f) => {
                        self.next += 1;
                        let h = 0x1000 + self.next * 4;
                        self.files.insert(h, f);
                        m.r[0] = h;
                    }
                    Err(e) => {
                        self.error(&e);
                        m.r[0] = INVALID_HANDLE;
                    }
                }
            }
            "ReadFile" => {
                let mut buf = vec![0u8; r8 as u32 as usize];
                let n = match self.files.get_mut(&rcx) {
                    Some(f) => f.read(&mut buf).unwrap_or(0),
                    None => 0,
                };
                for (i, b) in buf[..n].iter().enumerate() {
                    m.st(rdx + i as u64, 1, *b as u64);
                }
                if r9 != 0 {
                    m.st(r9, 4, n as u64);
                }
                m.r[0] = self.files.contains_key(&rcx) as u64;
            }
            "WriteFile" => {
                let bytes: Vec<u8> = (0..r8 as u32 as u64).map(|i| m.ld(rdx + i, 1) as u8).collect();
                let ok = self.files.get_mut(&rcx).map(|f| f.write_all(&bytes).is_ok()).unwrap_or(false);
                if r9 != 0 {
                    m.st(r9, 4, if ok { bytes.len() as u64 } else { 0 });
                }
                m.r[0] = ok as u64;
            }
            "SetFilePointerEx" => {
                let to = match r9 as u32 {
                    0 => SeekFrom::Start(rdx),
                    1 => SeekFrom::Current(rdx as i64),
                    _ => SeekFrom::End(rdx as i64),
                };
                match self.files.get_mut(&rcx).map(|f| f.seek(to)) {
                    Some(Ok(p)) => {
                        if r8 != 0 {
                            m.st(r8, 8, p);
                        }
                        m.r[0] = 1;
                    }
                    _ => m.r[0] = 0,
                }
            }
            "GetFileSizeEx" => match self.files.get(&rcx).and_then(|f| f.metadata().ok()) {
                Some(md) => {
                    m.st(rdx, 8, md.len());
                    m.r[0] = 1;
                }
                None => m.r[0] = 0,
            },
            "SetEndOfFile" => {
                let ok = match self.files.get_mut(&rcx) {
                    Some(f) => f.stream_position().and_then(|p| f.set_len(p)).is_ok(),
                    None => false,
                };
                m.r[0] = ok as u64;
            }
            "FlushFileBuffers" => {
                m.r[0] = self.files.get_mut(&rcx).map(|f| f.flush().is_ok()).unwrap_or(false) as u64;
            }
            "CloseHandle" => m.r[0] = self.files.remove(&rcx).is_some() as u64 | 1,
            "DeleteFileW" => {
                let r = std::fs::remove_file(path(m, rcx));
                if let Err(e) = &r {
                    self.error(e);
                }
                m.r[0] = r.is_ok() as u64;
            }
            "MoveFileW" => {
                let r = std::fs::rename(path(m, rcx), path(m, rdx));
                if let Err(e) = &r {
                    self.error(e);
                }
                m.r[0] = r.is_ok() as u64;
            }
            "CopyFileW" => {
                let (src, dst) = (path(m, rcx), path(m, rdx));
                let r = if r8 != 0 && dst.exists() { Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)) } else { std::fs::copy(src, dst).map(|_| ()) };
                if let Err(e) = &r {
                    self.error(e);
                }
                m.r[0] = r.is_ok() as u64;
            }
            "GetFileAttributesW" => {
                m.r[0] = match std::fs::metadata(path(m, rcx)) {
                    Ok(md) => (if md.is_dir() { 0x10 } else { 0x80 }) | if md.permissions().readonly() { 1 } else { 0 },
                    Err(e) => {
                        self.error(&e);
                        0xffff_ffff
                    }
                };
            }
            "GetLastError" => m.r[0] = self.last_error as u64,
            "SetLastError" => self.last_error = rcx as u32,
            // ---- threads, events, locks: there is one thread ----------------------------------
            "InitializeCriticalSection" | "DeleteCriticalSection" | "EnterCriticalSection" | "LeaveCriticalSection" | "SetEvent"
            | "ResetEvent" | "ReleaseSemaphore" | "Sleep" | "MsgWaitForMultipleObjects" => m.r[0] = 1,
            "CreateEventA" | "CreateSemaphoreA" => {
                self.next += 1;
                m.r[0] = 0x9000 + self.next * 4;
            }
            "WaitForSingleObjectEx" => m.r[0] = 0,
            // ---- time -------------------------------------------------------------------------
            "QueryPerformanceFrequency" => {
                m.st(rcx, 8, 1_000_000_000);
                m.r[0] = 1;
            }
            "QueryPerformanceCounter" => {
                m.st(rcx, 8, ticks());
                m.r[0] = 1;
            }
            "GetTickCount" | "timeGetTime" => m.r[0] = ticks() / 1_000_000 & 0xffff_ffff,
            "GetSystemTimeAsFileTime" => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                // 100 ns intervals since 1601
                m.st(rcx, 8, now.as_nanos() as u64 / 100 + 116_444_736_000_000_000);
            }
            // ---- memory -------------------------------------------------------------------------
            "free" | "??3@YAXPEAX@Z" | "??_V@YAXPEAX@Z" => m.free(rcx),
            "malloc" | "_malloc_crt" => m.r[0] = self.malloc(m, rcx as usize),
            "calloc" | "_calloc_crt" => m.r[0] = self.malloc(m, (rcx * rdx) as usize),
            "realloc" => {
                let n = rdx as usize;
                let new = self.malloc(m, n);
                if rcx != 0 {
                    let old = m.block_size(rcx);
                    for i in 0..old.min(n) {
                        let b = m.ld(rcx + i as u64, 1);
                        m.st(new + i as u64, 1, b);
                    }
                    m.free(rcx);
                }
                m.r[0] = new;
            }
            "_errno" => {
                if self.errno_cell == 0 {
                    self.errno_cell = m.alloc(8);
                }
                m.r[0] = self.errno_cell;
            }
            // ---- text ---------------------------------------------------------------------------
            "tolower" => m.r[0] = (rcx as u8 as char).to_ascii_lowercase() as u64,
            "iswdigit" => m.r[0] = (0x30..=0x39).contains(&(rcx as u32)) as u64,
            "_ismbblead" => m.r[0] = 0,
            "strchr" => {
                let mut i = 0;
                m.r[0] = loop {
                    let c = m.ld(rcx + i, 1);
                    if c == rdx as u8 as u64 {
                        break rcx + i;
                    }
                    if c == 0 {
                        break 0;
                    }
                    i += 1;
                };
            }
            "wcschr" => {
                let mut i = 0;
                m.r[0] = loop {
                    let c = m.ld(rcx + 2 * i, 2);
                    if c == rdx & 0xffff {
                        break rcx + 2 * i;
                    }
                    if c == 0 {
                        break 0;
                    }
                    i += 1;
                };
            }
            "wcsstr" => {
                let hay = wide(m, rcx);
                let needle = wide(m, rdx);
                m.r[0] = if needle.is_empty() {
                    rcx
                } else {
                    hay.windows(needle.len()).position(|w| w == &needle[..]).map(|p| rcx + 2 * p as u64).unwrap_or(0)
                };
            }
            "wcscspn" => {
                let (a, set) = (wide(m, rcx), wide(m, rdx));
                m.r[0] = a.iter().position(|c| set.contains(c)).unwrap_or(a.len()) as u64;
            }
            "wcsncpy" => {
                let s = wide(m, rdx);
                for i in 0..r8 {
                    let c = s.get(i as usize).copied().unwrap_or(0);
                    m.st(rcx + 2 * i, 2, c as u64);
                }
                m.r[0] = rcx;
            }
            "_wcsicmp" => {
                let a: String = String::from_utf16_lossy(&wide(m, rcx)).to_lowercase();
                let b: String = String::from_utf16_lossy(&wide(m, rdx)).to_lowercase();
                m.r[0] = match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1i64 as u64,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
            }
            "strncmp" | "_strnicmp" | "_stricmp" => {
                let n = if name == "_stricmp" { u64::MAX } else { r8 };
                let mut r = 0i64;
                for i in 0..n {
                    let (mut a, mut b) = (m.ld(rcx + i, 1) as u8, m.ld(rdx + i, 1) as u8);
                    if name != "strncmp" {
                        a = a.to_ascii_lowercase();
                        b = b.to_ascii_lowercase();
                    }
                    if a != b {
                        r = if a < b { -1 } else { 1 };
                        break;
                    }
                    if a == 0 {
                        break;
                    }
                }
                m.r[0] = r as u64;
            }
            "strncpy" | "strncpy_s" | "strcpy_s" | "strcat_s" => {
                // strncpy(dst, src, n); strncpy_s(dst, size, src, n); strcpy_s(dst, size, src)
                let (dst, size, src, count) = match name {
                    "strncpy" => (rcx, r8, rdx, r8),
                    "strncpy_s" => (rcx, rdx, r8, r9),
                    _ => (rcx, rdx, r8, u64::MAX),
                };
                let mut at = if name == "strcat_s" {
                    let mut k = 0;
                    while m.ld(dst + k, 1) != 0 {
                        k += 1;
                    }
                    k
                } else {
                    0
                };
                let mut i = 0;
                while i < count && (name == "strncpy" || at + 1 < size) {
                    let c = m.ld(src + i, 1);
                    if c == 0 && name != "strncpy" {
                        break;
                    }
                    m.st(dst + at, 1, c);
                    if c == 0 {
                        // strncpy pads with zeros up to n
                        at += 1;
                        i += 1;
                        while i < count {
                            m.st(dst + at, 1, 0);
                            at += 1;
                            i += 1;
                        }
                        break;
                    }
                    at += 1;
                    i += 1;
                }
                if name != "strncpy" {
                    m.st(dst + at, 1, 0);
                }
                m.r[0] = if name == "strncpy" { dst } else { 0 };
            }
            "memcmp" => {
                let mut r = 0i64;
                for i in 0..r8 {
                    let (a, b) = (m.ld(rcx + i, 1), m.ld(rdx + i, 1));
                    if a != b {
                        r = if a < b { -1 } else { 1 };
                        break;
                    }
                }
                m.r[0] = r as u64;
            }
            "_wcstoi64" | "wcstol" => {
                let s = String::from_utf16_lossy(&wide(m, rcx));
                let t = s.trim_start();
                let radix = if name == "wcstol" { r8 as u32 as i32 } else { r8 as u32 as i32 };
                let (neg, digits) = match t.strip_prefix('-') {
                    Some(r) => (true, r),
                    None => (false, t.strip_prefix('+').unwrap_or(t)),
                };
                let radix = if radix == 0 { if digits.starts_with("0x") || digits.starts_with("0X") { 16 } else { 10 } } else { radix as u32 };
                let digits = if radix == 16 { digits.trim_start_matches("0x").trim_start_matches("0X") } else { digits };
                let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
                let v = i64::from_str_radix(&digits[..end], radix).unwrap_or(0);
                if rdx != 0 {
                    let used = s.len() - t.len() + (t.len() - digits.len()) + end;
                    m.st(rdx, 8, rcx + 2 * used as u64);
                }
                m.r[0] = (if neg { -v } else { v }) as u64;
            }
            "wcstod" | "strtod" => {
                let wide_in = name == "wcstod";
                let s: String = if wide_in {
                    String::from_utf16_lossy(&wide(m, rcx))
                } else {
                    let mut v = Vec::new();
                    while m.ld(rcx + v.len() as u64, 1) != 0 {
                        v.push(m.ld(rcx + v.len() as u64, 1) as u8);
                    }
                    String::from_utf8_lossy(&v).into_owned()
                };
                let t = s.trim_start();
                let mut end = 0;
                for i in (0..=t.len()).rev() {
                    if t.is_char_boundary(i) && t[..i].parse::<f64>().is_ok() && !t[..i].is_empty() {
                        end = i;
                        break;
                    }
                }
                let v: f64 = t[..end].parse().unwrap_or(0.0);
                if rdx != 0 {
                    let used = (s.len() - t.len() + end) as u64;
                    m.st(rdx, 8, rcx + used * if wide_in { 2 } else { 1 });
                }
                m.set_xl(0, v.to_bits());
            }
            "atoi" => {
                let mut v = Vec::new();
                while m.ld(rcx + v.len() as u64, 1) != 0 {
                    v.push(m.ld(rcx + v.len() as u64, 1) as u8);
                }
                m.r[0] = crate::script::text::parse_int(String::from_utf8_lossy(&v).trim_start()) as i32 as u32 as u64;
            }
            // ---- the process ----------------------------------------------------------------
            "GetProcessHeap" => m.r[0] = 0x1_0000,
            "DecodePointer" | "EncodePointer" => m.r[0] = rcx,
            "SendMessageW" | "PostMessageW" => m.r[0] = 0,
            "GetCurrentDirectoryW" => {
                let d = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let d = d.replace('/', "\\");
                if rcx as u32 as usize > d.encode_utf16().count() {
                    put_wide(m, rdx, &d);
                }
                m.r[0] = d.encode_utf16().count() as u64;
            }
            _ => return false,
        }
        let _ = self.started;
        true
    }

    fn malloc(&mut self, m: &mut M, n: usize) -> u64 {
        m.alloc(n)
    }
}
