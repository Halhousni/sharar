//! Text and binary files a script opens: `FileOpen(name$, 8, mode%)` and `FileOpen(name$, 9, …)`.
//!
//! A text file is held in memory and written when it is closed, with `\r\n` line ends as a
//! Windows program writes them; `Print` appends to it and `Read` takes its lines. A binary file
//! is read and written with the sizes `BReadSize` and `BWriteSize` name. What each does with a
//! size it does not expect was found by trying them in Spike2 (`tools/run_cases/g_bin_*`).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Latin-1, the way a Windows program reads an ANSI text file.
fn from_latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

fn to_latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect()
}

pub struct TextFile {
    pub path: PathBuf,
    pub writing: bool,
    out: Vec<u8>,
    lines: Vec<String>,
    next: usize,
}

impl TextFile {
    /// A new, empty file to write; it reaches the disk when closed.
    pub fn create(path: PathBuf) -> TextFile {
        TextFile { path, writing: true, out: Vec::new(), lines: Vec::new(), next: 0 }
    }

    /// An existing file to read line by line.
    pub fn open(path: PathBuf) -> io::Result<TextFile> {
        let bytes = std::fs::read(&path)?;
        let text = from_latin1(&bytes);
        let mut lines: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        // A final newline does not start another line.
        if text.ends_with('\n') || text.is_empty() {
            lines.pop();
        }
        Ok(TextFile { path, writing: false, out: Vec::new(), lines, next: 0 })
    }

    /// `Print` appends; a line feed becomes a carriage return and a line feed.
    pub fn print(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                self.out.extend_from_slice(b"\r\n");
            } else {
                self.out.extend(to_latin1(&c.to_string()));
            }
        }
    }

    pub fn read_line(&mut self) -> Option<String> {
        let l = self.lines.get(self.next).cloned();
        if l.is_some() {
            self.next += 1;
        }
        l
    }

    pub fn close(self) -> io::Result<()> {
        if self.writing {
            std::fs::write(&self.path, &self.out)?;
        }
        Ok(())
    }
}

/// A value to write or the kind of value to read.
pub enum BinVal {
    Int(i64),
    Real(f64),
    Str(String),
}

pub struct BinFile {
    file: File,
    pub big_endian: bool,
}

impl BinFile {
    /// `mode` 1 creates the file anew; otherwise it is opened to read (and, with 2, to write).
    pub fn open(path: &PathBuf, mode: i64) -> io::Result<BinFile> {
        let file = if mode & 1 != 0 {
            File::options().read(true).write(true).create(true).truncate(true).open(path)?
        } else if mode == 2 {
            File::options().read(true).write(true).open(path)?
        } else {
            File::open(path)?
        };
        Ok(BinFile { file, big_endian: false })
    }

    fn put(&mut self, mut b: Vec<u8>) -> io::Result<()> {
        if self.big_endian {
            b.reverse();
        }
        self.file.write_all(&b)
    }

    /// Write one value. `size` is `BWriteSize`'s; none means the defaults.
    pub fn write(&mut self, v: &BinVal, size: Option<i64>) -> io::Result<()> {
        match v {
            BinVal::Int(i) => {
                let n = match size {
                    Some(s @ (1 | 2 | 4 | 8)) => s as usize,
                    _ => 4,
                };
                self.put(i.to_le_bytes()[..n].to_vec())
            }
            BinVal::Real(f) => match size {
                Some(4) => self.put((*f as f32).to_le_bytes().to_vec()),
                _ => self.put(f.to_le_bytes().to_vec()),
            },
            BinVal::Str(s) => {
                let mut b = to_latin1(s);
                match size {
                    Some(n) if n > 0 => {
                        b.resize(n as usize, 0);
                    }
                    _ => b.push(0),
                }
                self.file.write_all(&b)
            }
        }
    }

    fn take(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut b = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match self.file.read(&mut b[got..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => got += k,
            }
        }
        if got < n {
            // Not enough left: nothing is read, and the position does not move.
            let _ = self.file.seek(SeekFrom::Current(-(got as i64)));
            return None;
        }
        if self.big_endian {
            b.reverse();
        }
        Some(b)
    }

    pub fn read_int(&mut self, size: Option<i64>) -> Option<i64> {
        let n = match size {
            Some(s @ (1 | 2 | 4 | 8)) => s as usize,
            _ => 4,
        };
        let b = self.take(n)?;
        let mut v: i64 = 0;
        for (i, &x) in b.iter().enumerate() {
            v |= (x as i64) << (8 * i);
        }
        // Sign-extend from the size read.
        let shift = 64 - 8 * n as u32;
        Some((v << shift) >> shift)
    }

    pub fn read_real(&mut self, size: Option<i64>) -> Option<f64> {
        if size == Some(4) {
            let b = self.take(4)?;
            return Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64);
        }
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(&b);
        Some(f64::from_le_bytes(a))
    }

    pub fn read_str(&mut self, size: Option<i64>) -> Option<String> {
        match size {
            Some(n) if n > 0 => {
                let mut b = vec![0u8; n as usize];
                let got = self.file.read(&mut b).ok()?;
                if got < n as usize {
                    let _ = self.file.seek(SeekFrom::Current(-(got as i64)));
                    return None;
                }
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                Some(from_latin1(&b[..end]))
            }
            _ => {
                let mut out = Vec::new();
                let mut one = [0u8; 1];
                loop {
                    match self.file.read(&mut one) {
                        Ok(1) if one[0] == 0 => return Some(from_latin1(&out)),
                        Ok(1) => out.push(one[0]),
                        _ => {
                            return if out.is_empty() { None } else { Some(from_latin1(&out)) };
                        }
                    }
                }
            }
        }
    }

    pub fn position(&mut self) -> i64 {
        self.file.stream_position().unwrap_or(0) as i64
    }

    /// Move the position (`mode` 0 from the start, 1 from here, 2 from the end); returns it.
    pub fn seek(&mut self, offset: i64, mode: i64) -> i64 {
        let to = match mode {
            1 => SeekFrom::Current(offset),
            2 => SeekFrom::End(offset),
            _ => SeekFrom::Start(offset.max(0) as u64),
        };
        self.file.seek(to).map(|p| p as i64).unwrap_or(-1)
    }
}
