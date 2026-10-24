//! The operating system and C library routines translated code calls: files, time, memory,
//! text. Nothing here knows about scripts; each routine does what Windows or the C runtime does,
//! to the machine's memory. A call arrives as it would on Windows: arguments in rcx, rdx, r8, r9
//! and then on the stack at [rsp + 0x20], and the answer goes in rax (or xmm0).

use s2lift::mach::M;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

/// A C `FILE`: the file, and whether a read has hit the end.
struct Stdio {
    file: std::fs::File,
    eof: bool,
}

/// State of the operating-system layer: open files and the last error.
#[derive(Default)]
pub struct Win {
    stdio: HashMap<u64, Stdio>,
    /// Directory listings in progress: handle to the names still to give.
    finds: HashMap<u64, std::collections::VecDeque<(String, std::fs::Metadata)>>,
    registry: HashMap<String, Vec<u8>>,
    clipboard: Option<String>,
    environment: HashMap<String, String>,
    processes: HashMap<u64, std::process::Child>,
    static_tm: u64,
    tls: HashMap<u64, u64>,
    cwd_override: Option<std::path::PathBuf>,
    files: HashMap<u64, std::fs::File>,
    next: u64,
    last_error: u32,
    errno_cell: u64,
    /// Private copies of files the code opened for writing: the original is left alone until
    /// the script saves (an incomplete emulation must not damage a recording).
    shadow: HashMap<std::path::PathBuf, std::path::PathBuf>,
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
                let mut p = path(m, rcx);
                let write = access & 0x4000_0000 != 0;
                if write && p.exists() && disposition != 2 {
                    p = self.shadow_of(p);
                } else if let Some(c) = self.shadow.get(&p) {
                    p = c.clone();
                }
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
            // ---- C stdio ----------------------------------------------------------------------
            "_wfopen" => {
                let (p, mode) = (path(m, rcx), String::from_utf16_lossy(&wide(m, rdx)));
                let mut o = std::fs::OpenOptions::new();
                match mode.trim_end_matches(|c| c == 'b' || c == 't') {
                    "r" => o.read(true),
                    "r+" => o.read(true).write(true),
                    "w" => o.write(true).create(true).truncate(true),
                    "w+" => o.read(true).write(true).create(true).truncate(true),
                    "a" => o.append(true).create(true),
                    _ => o.read(true),
                };
                match o.open(&p) {
                    Ok(f) => {
                        self.next += 1;
                        let h = 0x70000 + self.next * 16;
                        self.stdio.insert(h, Stdio { file: f, eof: false });
                        m.r[0] = h;
                    }
                    Err(e) => {
                        self.error(&e);
                        m.r[0] = 0;
                    }
                }
            }
            "fclose" => m.r[0] = if self.stdio.remove(&rcx).is_some() { 0 } else { u64::MAX },
            "fflush" => {
                if let Some(f) = self.stdio.get_mut(&rcx) {
                    let _ = f.file.flush();
                }
                m.r[0] = 0;
            }
            "setvbuf" => m.r[0] = 0,
            "feof" => m.r[0] = self.stdio.get(&rcx).map(|f| f.eof as u64).unwrap_or(1),
            "fread" => {
                // fread(buf, size, count, FILE*)
                let (size, count) = (rdx, r8);
                let mut buf = vec![0u8; (size * count) as usize];
                let n = match self.stdio.get_mut(&r9) {
                    Some(f) => {
                        let mut got = 0;
                        while got < buf.len() {
                            match f.file.read(&mut buf[got..]) {
                                Ok(0) | Err(_) => {
                                    f.eof = true;
                                    break;
                                }
                                Ok(k) => got += k,
                            }
                        }
                        got
                    }
                    None => 0,
                };
                for (i, b) in buf[..n].iter().enumerate() {
                    m.st(rcx + i as u64, 1, *b as u64);
                }
                m.r[0] = if size == 0 { 0 } else { n as u64 / size };
            }
            "fwrite" => {
                let bytes: Vec<u8> = (0..rdx * r8).map(|i| m.ld(rcx + i, 1) as u8).collect();
                let ok = self.stdio.get_mut(&r9).map(|f| f.file.write_all(&bytes).is_ok()).unwrap_or(false);
                m.r[0] = if ok { r8 } else { 0 };
            }
            "fseek" | "_fseeki64" => {
                let to = match r8 as u32 {
                    0 => SeekFrom::Start(rdx),
                    1 => SeekFrom::Current(rdx as i64),
                    _ => SeekFrom::End(rdx as i64),
                };
                m.r[0] = match self.stdio.get_mut(&rcx) {
                    Some(f) => {
                        f.eof = false;
                        if f.file.seek(to).is_ok() { 0 } else { u64::MAX }
                    }
                    None => u64::MAX,
                };
            }
            "_ftelli64" => m.r[0] = self.stdio.get_mut(&rcx).and_then(|f| f.file.stream_position().ok()).unwrap_or(u64::MAX),
            "_wfullpath" => {
                // (buffer, path, size)
                let p = String::from_utf16_lossy(&wide(m, rdx));
                let full = if p.starts_with('/') || p.chars().nth(1) == Some(':') {
                    p
                } else {
                    format!("{}/{p}", std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default())
                };
                let full = full.replace('/', "\\");
                let buf = if rcx == 0 { m.alloc(2 * full.len() + 2) } else { rcx };
                put_wide(m, buf, &full);
                m.r[0] = buf;
            }
            "SetCurrentDirectoryW" => {
                let ok = std::env::set_current_dir(path(m, rcx)).is_ok();
                m.r[0] = ok as u64;
            }
            "CreateDirectoryW" => {
                let r = std::fs::create_dir(path(m, rcx));
                if let Err(e) = &r {
                    self.error(e);
                }
                m.r[0] = r.is_ok() as u64;
            }
            "FindFirstFileW" => {
                // (pattern, WIN32_FIND_DATAW*)
                let pat = path(m, rcx);
                let dir = pat.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                let mask = pat.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let mut found: Vec<(String, std::fs::Metadata)> = match std::fs::read_dir(if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { &dir }) {
                    Ok(rd) => rd.flatten().filter_map(|e| e.metadata().ok().map(|md| (e.file_name().to_string_lossy().into_owned(), md))).filter(|(n, _)| glob(&mask, n)).collect(),
                    Err(_) => Vec::new(),
                };
                found.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                let mut q: std::collections::VecDeque<_> = found.into();
                match q.pop_front() {
                    Some(first) => {
                        fill_find_data(m, rdx, &first);
                        self.next += 1;
                        let h = 0x80000 + self.next * 16;
                        self.finds.insert(h, q);
                        m.r[0] = h;
                    }
                    None => {
                        self.last_error = ERROR_FILE_NOT_FOUND;
                        m.r[0] = INVALID_HANDLE;
                    }
                }
            }
            "FindNextFileW" => {
                m.r[0] = match self.finds.get_mut(&rcx).and_then(|q| q.pop_front()) {
                    Some(e) => {
                        fill_find_data(m, rdx, &e);
                        1
                    }
                    None => {
                        self.last_error = 18; // ERROR_NO_MORE_FILES
                        0
                    }
                };
            }
            "FindClose" => m.r[0] = self.finds.remove(&rcx).is_some() as u64,
            "GetLastError" => m.r[0] = self.last_error as u64,
            "SetLastError" => self.last_error = rcx as u32,
            // ---- threads, events, locks: there is one thread ----------------------------------
            "_lock" | "_unlock" | "_onexit" | "__dllonexit" => m.r[0] = rcx,
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
            "GetCurrentThreadId" => m.r[0] = 1,
            "GetCurrentProcessId" => m.r[0] = 4242,
            "TlsAlloc" | "FlsAlloc" => {
                self.next += 1;
                m.r[0] = self.next + 1;
            }
            "TlsGetValue" | "FlsGetValue" => m.r[0] = self.tls.get(&rcx).copied().unwrap_or(0),
            "TlsSetValue" | "FlsSetValue" => {
                self.tls.insert(rcx, rdx);
                m.r[0] = 1;
            }
            "TlsFree" | "FlsFree" => m.r[0] = 1,
            "HeapAlloc" | "HeapReAlloc" if name == "HeapAlloc" => m.r[0] = m.alloc(r8 as usize),
            "HeapFree" => {
                m.free(r8);
                m.r[0] = 1;
            }
            "HeapSize" => m.r[0] = m.block_size(r8) as u64,
            "HeapCreate" => m.r[0] = 0x2_0000,
            "HeapDestroy" => m.r[0] = 1,
            "GetModuleHandleW" | "GetModuleHandleA" => m.r[0] = 0x1_8000_0000,
            "GetProcAddress" => m.r[0] = 0,
            "GetCommandLineA" | "GetCommandLineW" => {
                let p = m.alloc(16);
                m.r[0] = p;
            }
            "GetVersion" => m.r[0] = 0x0000_0601, // Windows 7
            "GetStartupInfoW" | "GetStartupInfoA" => {}
            "RtlCaptureContext" | "RtlLookupFunctionEntry" | "RtlVirtualUnwind" | "SetUnhandledExceptionFilter" | "UnhandledExceptionFilter" => m.r[0] = 0,
            "HeapLock" | "HeapUnlock" | "HeapValidate" | "_heapmin" | "IsDebuggerPresent" | "IsProcessorFeaturePresent" => {
                m.r[0] = (name != "IsDebuggerPresent" && name != "IsProcessorFeaturePresent") as u64
            }
            "HeapWalk" => m.r[0] = 0,
            "_heapset" => m.r[0] = (-2i64) as u64,
            "_clearfp" | "_statusfp" => m.r[0] = 0,
            "_finite" => m.r[0] = f64::from_bits(m.xl(0)).is_finite() as u64,
            "_fpclass" => {
                let x = f64::from_bits(m.xl(0));
                // _FPCLASS_SNAN 1, QNAN 2, NINF 4, NN 8, ND 0x10, NZ 0x20, PZ 0x40, PD 0x80, PN 0x100, PINF 0x200
                m.r[0] = if x.is_nan() { 2 } else if x == f64::NEG_INFINITY { 4 } else if x == f64::INFINITY { 0x200 }
                    else if x == 0.0 { if x.is_sign_negative() { 0x20 } else { 0x40 } }
                    else if x.is_normal() { if x < 0.0 { 8 } else { 0x100 } } else if x < 0.0 { 0x10 } else { 0x80 };
            }
            "GetDesktopWindow" => m.r[0] = 0x2222,
            "GetWindowRect" => {
                for (i, v) in [0u64, 0, 1920, 1080].iter().enumerate() {
                    m.st(rdx + 4 * i as u64, 4, *v);
                }
                m.r[0] = 1;
            }
            "IsWindow" => m.r[0] = 1,
            "DestroyWindow" | "RegisterClassW" | "CoUninitialize" | "PlaySoundW" => m.r[0] = 1,
            "CreateWindowExW" | "GetWindowLongPtrW" | "SetWindowLongPtrW" | "DefWindowProcW" => m.r[0] = 0,
            "CoInitialize" => m.r[0] = 0,
            "CoCreateInstance" => m.r[0] = 0x8000_4005, // E_FAIL: there is no speech here
            "GetEnvironmentVariableW" => {
                let n = String::from_utf16_lossy(&wide(m, rcx));
                let v = self.environment.get(&n).cloned().or_else(|| std::env::var(&n).ok());
                m.r[0] = match v {
                    Some(v) => {
                        let units = v.encode_utf16().count() as u64;
                        if r8 > units {
                            put_wide(m, rdx, &v);
                        }
                        if r8 > units { units } else { units + 1 }
                    }
                    None => {
                        self.last_error = 203;
                        0
                    }
                };
            }
            "SetEnvironmentVariableW" => {
                let n = String::from_utf16_lossy(&wide(m, rcx));
                if rdx == 0 {
                    self.environment.remove(&n);
                } else {
                    self.environment.insert(n, String::from_utf16_lossy(&wide(m, rdx)));
                }
                m.r[0] = 1;
            }
            "GetEnvironmentStringsW" => {
                let mut all: Vec<u16> = Vec::new();
                for (k, v) in std::env::vars().chain(self.environment.clone()) {
                    all.extend(format!("{k}={v}").encode_utf16());
                    all.push(0);
                }
                all.push(0);
                let p = m.alloc(all.len() * 2);
                for (i, u) in all.iter().enumerate() {
                    m.st(p + 2 * i as u64, 2, *u as u64);
                }
                m.r[0] = p;
            }
            "FreeEnvironmentStringsW" => m.r[0] = 1,
            "RegQueryValueExW" | "RegSetValueExW" | "RegDeleteValueW" | "RegCloseKey" => {
                m.r[0] = match name {
                    "RegCloseKey" | "RegSetValueExW" | "RegDeleteValueW" => 0,
                    _ => 2, // ERROR_FILE_NOT_FOUND: no stored setting
                }
            }
            "OpenClipboard" | "EmptyClipboard" | "CloseClipboard" => m.r[0] = 1,
            "GlobalAlloc" => m.r[0] = m.alloc(rdx as usize + 16),
            "GlobalLock" => m.r[0] = rcx,
            "GlobalUnlock" => m.r[0] = 1,
            "GlobalFree" => {
                m.free(rcx);
                m.r[0] = 0;
            }
            "SetClipboardData" => {
                self.clipboard = Some(String::from_utf16_lossy(&wide(m, rdx)));
                m.r[0] = rdx;
            }
            "GetClipboardData" => {
                m.r[0] = match &self.clipboard.clone() {
                    Some(t) => {
                        let p = m.alloc(2 * t.len() + 2);
                        put_wide(m, p, t);
                        p
                    }
                    None => 0,
                }
            }
            "_gmtime64" | "_localtime64" => {
                if self.static_tm == 0 {
                    self.static_tm = m.alloc(64);
                }
                let t = m.ld(rcx, 8) as i64;
                let (tm, _) = super::localtime(t, name == "_gmtime64");
                for (i, v) in tm.iter().enumerate() {
                    m.st(self.static_tm + 4 * i as u64, 4, *v as u32 as u64);
                }
                m.r[0] = self.static_tm;
            }
            "GetLocalTime" => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                let (tm, _) = super::localtime(now.as_secs() as i64, false);
                // SYSTEMTIME: year month dayofweek day hour minute second milliseconds
                let f = [tm[5] + 1900, tm[4] + 1, tm[6], tm[3], tm[2], tm[1], tm[0], now.subsec_millis() as i32];
                for (i, v) in f.iter().enumerate() {
                    m.st(rcx + 2 * i as u64, 2, *v as u64);
                }
            }
            "_mktime64" => {
                // (struct tm *): local time to Unix time
                let f: Vec<i32> = (0..9).map(|i| m.ld(rcx + 4 * i, 4) as u32 as i32).collect();
                m.r[0] = super::mktime(&f) as u64;
            }
            "strerror" => {
                let p = m.alloc(32);
                for (i, b) in b"error\0".iter().enumerate() {
                    m.st(p + i as u64, 1, *b as u64);
                }
                m.r[0] = p;
            }
            "MultiByteToWideChar" => {
                // (codepage, flags, src, srclen, dst, dstlen)
                let (src, len) = (r8, r9 as u32 as i32);
                let n = if len < 0 { let mut k = 0; while m.ld(src + k, 1) != 0 { k += 1; } k as usize + 1 } else { len as usize };
                let bytes: Vec<u8> = (0..n as u64).map(|i| m.ld(src + i, 1) as u8).collect();
                let units: Vec<u16> = if rcx == 65001 { String::from_utf8_lossy(&bytes).encode_utf16().collect() } else { bytes.iter().map(|&b| b as u16).collect() };
                let dst = m.ld(m.r[4] + 0x20, 8);
                let cap = m.ld(m.r[4] + 0x28, 8) as u32 as usize;
                if cap >= units.len() && dst != 0 {
                    for (i, u) in units.iter().enumerate() {
                        m.st(dst + 2 * i as u64, 2, *u as u64);
                    }
                }
                m.r[0] = if cap == 0 || cap >= units.len() { units.len() as u64 } else { 0 };
            }
            "WideCharToMultiByte" => {
                // (codepage, flags, src, srclen, dst, dstlen, defchar, usedefault)
                let (src, len) = (r8, r9 as u32 as i32);
                let n = if len < 0 { let mut k = 0; while m.ld(src + 2 * k, 2) != 0 { k += 1; } k as usize + 1 } else { len as usize };
                let units: Vec<u16> = (0..n as u64).map(|i| m.ld(src + 2 * i, 2) as u16).collect();
                let bytes: Vec<u8> = if rcx == 65001 {
                    String::from_utf16_lossy(&units).into_bytes()
                } else {
                    units.iter().map(|&u| if u < 256 { u as u8 } else { b'?' }).collect()
                };
                let dst = m.ld(m.r[4] + 0x20, 8);
                let cap = m.ld(m.r[4] + 0x28, 8) as u32 as usize;
                if cap >= bytes.len() && dst != 0 {
                    for (i, b) in bytes.iter().enumerate() {
                        m.st(dst + i as u64, 1, *b as u64);
                    }
                }
                m.r[0] = if cap == 0 || cap >= bytes.len() { bytes.len() as u64 } else { 0 };
            }
            "CreateProcessW" => {
                // (app, commandline, ..., flags, env, cwd, startupinfo, processinfo): run the command line
                let cmdline = String::from_utf16_lossy(&wide(m, if rcx != 0 { rcx } else { rdx }));
                let (info, cwd) = (m.ld(m.r[4] + 0x20 + 8 * 4, 8), m.ld(m.r[4] + 0x20 + 8 * 2, 8));
                let words = split_command_line(&cmdline);
                let Some((prog, rest)) = words.split_first() else { m.r[0] = 0; return true };
                let mut c = std::process::Command::new(prog.replace('\\', "/"));
                c.args(rest);
                if cwd != 0 {
                    c.current_dir(path(m, cwd));
                }
                match c.spawn() {
                    Ok(child) => {
                        self.next += 1;
                        let h = 0xA0000 + self.next * 16;
                        let pid = child.id() as u64;
                        self.processes.insert(h, child);
                        if info != 0 {
                            m.st(info, 8, h);
                            m.st(info + 8, 8, 0);
                            m.st(info + 16, 4, pid);
                            m.st(info + 20, 4, 0);
                        }
                        m.r[0] = 1;
                    }
                    Err(e) => {
                        self.error(&e);
                        m.r[0] = 0;
                    }
                }
            }
            "GetExitCodeProcess" => {
                let code = match self.processes.get_mut(&rcx) {
                    Some(c) => match c.try_wait() {
                        Ok(Some(st)) => st.code().unwrap_or(1) as u64,
                        _ => 259, // STILL_ACTIVE
                    },
                    None => 0,
                };
                m.st(rdx, 4, code);
                m.r[0] = 1;
            }
            "TerminateProcess" => {
                if let Some(c) = self.processes.get_mut(&rcx) {
                    let _ = c.kill();
                }
                m.r[0] = 1;
            }
            "__pctype_func" | "___lc_codepage_func" | "___mb_cur_max_func" | "___lc_locale_name_func" | "___lc_collate_cp_func" | "setlocale" => m.r[0] = 0,
            "DecodePointer" | "EncodePointer" => m.r[0] = rcx,
            "PostMessageW" => m.r[0] = 0,
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

    /// The private copy of `original`, made on first use.
    fn shadow_of(&mut self, original: std::path::PathBuf) -> std::path::PathBuf {
        if let Some(c) = self.shadow.get(&original) {
            return c.clone();
        }
        let dir = std::env::temp_dir().join(format!("sharar-shadow-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let copy = dir.join(format!("{}-{}", self.shadow.len(), original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        if std::fs::copy(&original, &copy).is_ok() {
            self.shadow.insert(original, copy.clone());
            copy
        } else {
            original
        }
    }

    fn malloc(&mut self, m: &mut M, n: usize) -> u64 {
        m.alloc(n)
    }
}

/// `*` and `?` matching without regard to case, for the names a directory search finds.
fn glob(pat: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pat.to_lowercase().chars().collect(), name.to_lowercase().chars().collect());
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|k| go(&p[1..], &n[k..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    go(&p, &n)
}

/// Write a `WIN32_FIND_DATAW` for one directory entry.
fn fill_find_data(m: &mut M, at: u64, e: &(String, std::fs::Metadata)) {
    let attr: u64 = if e.1.is_dir() { 0x10 } else { 0x80 };
    m.st(at, 4, attr);
    // times (creation, access, write) as 100 ns since 1601
    let t = e.1.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64 / 100 + 116_444_736_000_000_000).unwrap_or(0);
    for k in 0..3 {
        m.st(at + 4 + 8 * k, 8, t);
    }
    m.st(at + 28, 4, e.1.len() >> 32);
    m.st(at + 32, 4, e.1.len() & 0xffff_ffff);
    put_wide(m, at + 44, &e.0);
}

fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut cur, mut quoted, mut any) = (String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}
