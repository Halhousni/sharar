//! Commands that run Spike2's own code: `s2lift::commands` holds each command's `Execute`
//! translated from `runtime.s2l`, with the routines of compiler.dll and SpikeUtl.dll it calls.
//! What is left for the host is `CRunInfo`, the script's side of a call: its arguments, its
//! result and the arrays it was given (this file), and the windows and data files (host.rs).
//!
//! A script array is copied into the machine's memory in the layout `CArrObj` expects, the
//! translated code works on it there, and the result is copied back when the command ends.

use super::{Arg, Cell, Ref, RunError, Vm, V};
mod cstring;
mod win;
use crate::script::compile::Ty;
use s2lift::{commands, mach::M, Svc};
use std::panic::{catch_unwind, panic_any, AssertUnwindSafe};

/// What translated code throws with `SetArgError` and the like: Spike2's error code.
struct Thrown(i32);

/// Raised when translated code reaches something not translated (a service the host lacks).
pub const ERR_LIFTED: i32 = -7002;

/// A script array copied into the machine.
#[derive(Clone)]
struct Bound {
    cell: usize,
    addr: u64,
    ty: Ty,
    dims: Vec<usize>,
}

impl Bound {
    fn clone_shape(&self) -> (u64, Vec<usize>) {
        (self.addr, self.dims.clone())
    }
}

fn elem_size(ty: Ty) -> Option<usize> {
    match ty {
        Ty::Int | Ty::Float => Some(8),
        Ty::Str => None,
    }
}

struct Run<'v, 'w> {
    vm: &'v mut Vm,
    args: &'v [Arg],
    result: Option<V>,
    bound: Vec<Bound>,
    /// The last service the translated code asked for that the host does not have.
    missing: Option<String>,
    world: &'w mut World,
    /// String arguments the code was given to change: argument number and the holder it edits.
    holders: Vec<(i32, u64)>,
}

impl<'v, 'w> Run<'v, 'w> {
    fn arg(&self, idx: i32) -> &Arg {
        if idx < 1 || idx as usize > self.args.len() {
            panic_any(Thrown(-0x1b75));
        }
        &self.args[idx as usize - 1]
    }

    /// Put script array `idx` into the machine as a `CArrObj` at `obj`.
    fn fill_in_array(&mut self, m: &mut M, idx: i32, obj: u64) {
        let Some(c) = self.vm.a_array(self.arg(idx)) else { panic_any(Thrown(-0x1b7e)) };
        let Cell::Array { ty, dims, data } = &self.vm.cells[c] else { unreachable!() };
        let (ty, dims) = (*ty, dims.clone());
        let Some(es) = elem_size(ty) else { panic_any(Thrown(-0x1b7e)) };
        // One copy per array: a command may fill the same argument in twice.
        let addr = match self.bound.iter().find(|b| b.cell == c) {
            Some(b) => b.addr,
            None => {
                let addr = m.alloc((data.len() * es).max(8));
                for (i, v) in data.iter().enumerate() {
                    let bits = match v {
                        V::Int(n) => *n as u64,
                        V::Flt(f) => f.to_bits(),
                        V::Str(_) => 0,
                    };
                    m.st(addr + (i * es) as u64, 8, bits);
                }
                self.bound.push(Bound { cell: c, addr, ty, dims: dims.clone() });
                addr
            }
        };
        self.write_obj(m, obj, ty, &dims, addr);
    }

    /// The fields of a `CArrObj` that the translated code reads: the type description (type,
    /// number of dimensions, size of each), the stride of each dimension in bytes, the data.
    fn write_obj(&self, m: &mut M, obj: u64, ty: Ty, dims: &[usize], addr: u64) {
        let vt = match ty {
            Ty::Int => 1,
            Ty::Float => 2,
            Ty::Str => 3,
        };
        m.st(obj + 0x08, 8, 0);
        m.st(obj + 0x10, 4, vt);
        m.st(obj + 0x14, 2, dims.len() as u64);
        for k in 0..5 {
            m.st(obj + 0x18 + 4 * k as u64, 4, dims.get(k).copied().unwrap_or(0) as u64);
        }
        let mut stride = 8usize;
        for k in (0..dims.len()).rev() {
            m.st(obj + 0x2c + 4 * k as u64, 4, stride as u64);
            stride *= dims[k];
        }
        m.st(obj + 0x40, 8, addr);
        m.st(obj + 0x48, 8, 0);
        m.st(obj + 0x50, 4, 0xffff_ffff);
        m.st(obj + 0x88, 2, 0);
    }

    fn copy_back(&mut self, m: &M) {
        for (idx, holder) in std::mem::take(&mut self.holders) {
            let text = cstring::from_units(&self.world.strs.read(m, holder, true));
            if let Arg::Ref(r) = self.args[idx as usize - 1] {
                let _ = self.vm.set(r, V::Str(text));
            }
        }
        for b in &self.bound {
            if let Cell::Array { data, dims, .. } = &mut self.vm.cells[b.cell] {
                let n: usize = b.dims.iter().product();
                *dims = b.dims.clone();
                *data = (0..n)
                    .map(|i| {
                        let bits = m.ld(b.addr + 8 * i as u64, 8);
                        match b.ty {
                            Ty::Int => V::Int(bits as i64),
                            _ => V::Flt(f64::from_bits(bits)),
                        }
                    })
                    .collect();
            }
        }
    }
}

/// Addresses handed out as the methods of objects the host builds: slot k of the fake vtable.
const FAKE_CODE: u64 = 0x4100_0000_0000;

impl<'v, 'w> Svc for Run<'v, 'w> {
    fn call(&mut self, m: &mut M, target: u64) -> bool {
        match target.wrapping_sub(FAKE_CODE) {
            // CWinThread::GetMainWnd (slot 31)
            31 => {
                m.r[0] = self.world.main_wnd;
                m.pop();
                true
            }
            // a routine of an object whose constructor was skipped: it succeeds
            t if (0x3000..0x3200).contains(&t) => {
                m.r[0] = 1;
                m.pop();
                true
            }
            // CWinApp::GetProfileInt(section, entry, default): nothing is stored, so the default
            0x108 => {
                m.r[0] = m.r[9] & 0xffff_ffff;
                m.pop();
                true
            }
            // the owner of a channel view: its data file (slot 0x480), seconds per tick (0x510)
            0x480 => {
                m.r[0] = self.world.measure_file;
                m.pop();
                true
            }
            // the time of the last item in the file, in ticks: the data file's own answer (its slot 0x58)
            0x468 => {
                let file = self.world.measure_file;
                let target = m.ld(m.ld(file, 8) + 0x58, 8);
                m.r[1] = file;
                commands::call_at(m, self, target);
                true
            }
            // ticks (rdx) to seconds
            0x510 => {
                m.set_xl(0, (m.r[2] as i64 as f64 * self.world.measure_tf).to_bits());
                m.pop();
                true
            }
            // seconds (xmm1) to ticks, rounded away from zero in the middle
            0x4f8 => {
                let x = f64::from_bits(m.xl(1)) / self.world.measure_tf;
                let big = 0x7000_0000_0000_0000i64;
                m.r[0] = if x > big as f64 {
                    big
                } else if x >= 0.0 {
                    (x + 0.5) as i64
                } else if x >= -(big as f64) {
                    (x - 0.5) as i64
                } else {
                    -big
                } as u64;
                m.pop();
                true
            }
            _ => false,
        }
    }

    fn import(&mut self, m: &mut M, id: u32) {
        let name = commands::IMPORT_NAMES[id as usize];
        if std::env::var_os("S2LIFT_SERVICES").is_some() {
            eprintln!("service {name} rcx={:#x} rdx={:#x} r8={:#x} r9={:#x}", m.r[1], m.r[2], m.r[8], m.r[9]);
        }
        let rdx = m.r[2];
        let r8 = m.r[8];
        let r9 = m.r[9];
        let int_arg = |v: u64| v as u32 as i32;
        let num = |r: Result<f64, RunError>| match r {
            Ok(v) => v,
            Err(e) => panic_any(Thrown(e.0)),
        };
        match name {
            "?GetFloat@CRunInfo@@QEBANH@Z" => {
                let v = num(self.vm.a_flt(self.arg(int_arg(rdx))));
                m.set_xl(0, v.to_bits());
            }
            "?GetInt@CRunInfo@@QEBA_JH@Z" => m.r[0] = self.int_arg(int_arg(rdx)) as u64,
            "?GetIntErr@CRunInfo@@QEBA_JH_J0@Z" => {
                let v = self.int_arg(int_arg(rdx));
                if !(r8 as i64 <= v && v <= m.r[9] as i64) {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_INT));
                }
                m.r[0] = v as u64;
            }
            "?GetIntLim@CRunInfo@@QEBA_JH_J0@Z" => {
                let v = self.int_arg(int_arg(rdx));
                m.r[0] = v.clamp(r8 as i64, (m.r[9] as i64).max(r8 as i64)) as u64;
            }
            "?GetI32@CRunInfo@@QEBAHH@Z" => {
                let v = self.int_arg(int_arg(rdx));
                if v < i32::MIN as i64 || v > i32::MAX as i64 {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_INT));
                }
                m.r[0] = v as i32 as u32 as u64;
            }
            "?GetI32Err@CRunInfo@@QEBAHHHH@Z" => {
                let v = self.int_arg(int_arg(rdx));
                if !(int_arg(r8) as i64 <= v && v <= int_arg(m.r[9]) as i64) {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_INT));
                }
                m.r[0] = v as i32 as u32 as u64;
            }
            "?GetI32Lim@CRunInfo@@QEBAHHHH@Z" => {
                let v = self.int_arg(int_arg(rdx));
                let (lo, hi) = (int_arg(r8) as i64, int_arg(m.r[9]) as i64);
                // as the DLL does it: below lo gives lo; otherwise above hi gives hi
                let r = if lo <= v { if hi < v { hi } else { v } } else { lo };
                m.r[0] = r as i32 as u32 as u64;
            }
            "?GetFloatErr@CRunInfo@@QEBANHNN@Z" => {
                let v = num(self.vm.a_flt(self.arg(int_arg(rdx))));
                if !(f64::from_bits(m.xl(2)) <= v && v <= f64::from_bits(m.xl(3))) {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_REAL));
                }
                m.set_xl(0, v.to_bits());
            }
            "?GetFloatLim@CRunInfo@@QEBANHNN@Z" => {
                let v = num(self.vm.a_flt(self.arg(int_arg(rdx))));
                let (lo, hi) = (f64::from_bits(m.xl(2)), f64::from_bits(m.xl(3)));
                let r = if lo <= v && v <= hi { v } else if lo <= v { hi } else { lo };
                m.set_xl(0, r.to_bits());
            }
            "?GetArgType@CRunInfo@@QEBAXHAEAVTypeDesc@@@Z" => self.arg_type(m, int_arg(rdx), r8),
            "?Resize@CRunInfo@@QEBAXHAEAVCArrObj@@H@Z" => self.resize(m, int_arg(rdx), r8, int_arg(m.r[9])),
            "?SetFloat@CRunInfo@@QEBAXHN@Z" => {
                let v = f64::from_bits(m.xl(2));
                self.set_arg(int_arg(rdx), V::Flt(v));
            }
            "?SetCRef@CRunInfo@@QEBAXHK@Z" => {
                // a colour into three arguments: red, green, blue
                let c = r8 as u32;
                let i = int_arg(rdx);
                self.set_arg(i, V::Flt((c & 0xff) as f64 / 255.0));
                self.set_arg(i + 1, V::Flt(((c >> 8) & 0xff) as f64 / 255.0));
                self.set_arg(i + 2, V::Flt(((c >> 16) & 0xff) as f64 / 255.0));
            }
            "?SendMessageToDescendants@CWnd@@SAXPEAUHWND__@@I_K_JHH@Z" => {}
            // painting and arranging windows: there is no screen, so nothing to do
            n if n.contains("@CWnd@@")
                && ["?Invalidate@", "?InvalidateRect@", "?UpdateWindow@", "?ShowWindow@", "?SetWindowPos@", "?MoveWindow@", "?SetFocus@", "?ClientToScreen@", "?ScreenToClient@", "?SetCapture@", "?ReleaseCapture@", "?RedrawWindow@", "?EnableWindow@", "?BringWindowToTop@", "?SetWindowTextW@", "?ValidateRect@"].iter().any(|p| n.starts_with(p)) => {
                m.r[0] = 1;
            }
            // device contexts, pens, dialogs: objects that are only built and dropped here
            n if (n.starts_with("??0") || n.starts_with("??1"))
                && ["CClientDC@@", "CFileDialog@@", "CWaitCursor@@", "CPen@@", "CBrush@@", "CFont@@", "CDC@@", "CBitmap@@", "CPaintDC@@"].iter().any(|c| n.contains(c)) => {
                let obj = m.r[1];
                if n.starts_with("??0") && obj != 0 {
                    // its routines all succeed
                    if self.world.stub_vtable == 0 {
                        let vt = m.alloc(8 * 0x200);
                        for k in 0..0x200u64 {
                            m.st(vt + 8 * k, 8, FAKE_CODE + 0x3000 + k);
                        }
                        self.world.stub_vtable = vt;
                    }
                    m.st(obj, 8, self.world.stub_vtable);
                }
                m.r[0] = obj;
            }
            "_ecvt" | "_fcvt" => {
                // (double value, int digits, int *decpt, int *sign): the digits of the value, rounded
                let v = f64::from_bits(m.xl(0));
                let nd = (rdx as u32 as i32).clamp(0, 40) as usize;
                let neg = v.is_sign_negative();
                let (digits, decpt) = if v == 0.0 || !v.is_finite() {
                    ("0".repeat(nd.max(1)), 0i32)
                } else if name == "_ecvt" {
                    let t = format!("{:.*e}", nd.saturating_sub(1), v.abs());
                    let (mant, exp) = t.split_once('e').unwrap();
                    (mant.replace('.', ""), exp.parse::<i32>().unwrap() + 1)
                } else {
                    // digits after the point
                    let t = format!("{:.*}", nd, v.abs());
                    let (ip, fp) = t.split_once('.').unwrap_or((t.as_str(), ""));
                    let all = format!("{ip}{fp}");
                    let lead = all.len() - all.trim_start_matches('0').len();
                    if all.trim_start_matches('0').is_empty() {
                        ("0".to_string(), 0)
                    } else {
                        (all.trim_start_matches('0').to_string(), ip.len() as i32 - lead as i32)
                    }
                };
                if self.world.cvt_buf == 0 {
                    self.world.cvt_buf = m.alloc(64);
                }
                for (i, b) in digits.bytes().enumerate() {
                    m.st(self.world.cvt_buf + i as u64, 1, b as u64);
                }
                m.st(self.world.cvt_buf + digits.len() as u64, 1, 0);
                if r8 != 0 {
                    m.st(r8, 4, decpt as u32 as u64);
                }
                if r9 != 0 {
                    m.st(r9, 4, neg as u64);
                }
                m.r[0] = self.world.cvt_buf;
            }
            "GetTextMetricsW" => {
                // a font of 16 rows and 7-wide letters
                for k in 0..16u64 {
                    m.st(rdx + 4 * k, 4, 0);
                }
                m.st(rdx, 4, 16);
                m.st(rdx + 4, 4, 13);
                m.st(rdx + 8, 4, 3);
                m.st(rdx + 20, 4, 7);
                m.st(rdx + 24, 4, 14);
                m.r[0] = 1;
            }
            "GetTextExtentPoint32W" | "GetTextExtentPointW" => {
                let n = r9 as u32 as u64;
                m.st(m.r[4] + 0x28, 4, 0);
                let out = m.ld(m.r[4] + 0x20, 8);
                if out != 0 {
                    m.st(out, 4, 7 * n);
                    m.st(out + 4, 4, 16);
                }
                m.r[0] = 1;
            }
            "SelectObject" | "DeleteObject" | "GetStockObject" | "CreateFontIndirectW" | "CreatePen" | "CreateSolidBrush" | "SetBkMode" | "SetTextColor" | "SetBkColor" | "SetMapMode" | "GetDeviceCaps" | "SaveDC" | "RestoreDC" | "IntersectClipRect" | "SelectClipRgn" | "CreateCompatibleDC" | "DeleteDC" | "SetROP2" | "MoveToEx" | "LineTo" | "Rectangle" | "FillRect" | "TextOutW" | "ExtTextOutW" | "DrawTextW" | "SetTextAlign" | "GetCurrentObject" | "PtVisible" | "RectVisible" => {
                m.r[0] = 1;
            }
            "GetObjectType" | "GetObjectW" | "GetClipBox" | "GetCursorPos" | "GetKeyState" | "GetAsyncKeyState" | "GetSystemMetrics" | "GetWindowLongPtrW" | "SetWindowLongPtrW" | "GetBkColor" | "GetTextColor" | "GetBkMode" | "GetROP2" | "GetMapMode" | "GetViewportOrgEx" | "SetViewportOrgEx" | "OffsetViewportOrgEx" | "GetDCOrgEx" => {
                m.r[0] = 0;
            }
            "GetSysColor" => m.r[0] = 0x808080,
            n if n.starts_with('?') && !n.starts_with("??") && ["@CGdiObject@@", "@CDC@@", "@CPen@@", "@CBrush@@", "@CFont@@", "@CBitmap@@", "@CRgn@@", "@CClientDC@@", "@CMetaFileDC@@", "@CPaintDC@@", "@CMenu@@", "@CToolTipCtrl@@"].iter().any(|c| n.contains(c)) => {
                // drawing: nothing is drawn
                m.r[0] = 1;
            }
            n if n.starts_with("?GetThisClass@") => m.r[0] = 0x7777_0000,
            "?IsKindOf@CObject@@QEBAHPEBUCRuntimeClass@@@Z" => m.r[0] = 0,
            "GetParent" => m.r[0] = if m.r[1] != 0 { 0x30001 } else { 0 },
            "GetFocus" | "GetCapture" | "GetDlgItem" | "GetActiveWindow" | "GetForegroundWindow" | "GetDC" | "ReleaseDC" | "GetWindow" | "GetAncestor" => {
                m.r[0] = 0;
            }
            "IsIconic" | "IsZoomed" | "GdiplusStartup" | "?BeginWaitCursor@CCmdTarget@@QEAAXXZ" | "?EndWaitCursor@CCmdTarget@@QEAAXXZ" | "?GetParentFrame@CWnd@@QEBAPEAVCFrameWnd@@XZ" => {
                m.r[0] = 0;
            }
            "InvalidateRect" | "UpdateWindow" | "RedrawWindow" | "ShowWindow" | "SetFocus" | "EnableWindow" | "BringWindowToTop" | "MoveWindow" | "SetCapture" | "ReleaseCapture" | "SetCursor" | "ClientToScreen" | "ScreenToClient" | "ValidateRect" | "SetWindowPos" => {
                m.r[0] = 1;
            }
            // creating a window always succeeds (there is no screen): the objects are only kept
            "?Create@CWnd@@UEAAHPEB_W0KAEBUtagRECT@@PEAV1@IPEAUCCreateContext@@@Z" | "LoadCursorW" | "LoadIconW" | "GetClientRect" | "GetWindowRect" => {
                if name == "LoadCursorW" || name == "LoadIconW" {
                    m.r[0] = 0x1234;
                } else if name.contains("Rect") {
                    for k in 0..4 {
                        m.st(rdx + 4 * k, 4, 0);
                    }
                    m.r[0] = 1;
                } else {
                    m.r[0] = 1;
                }
            }
            "?AfxRegisterWndClass@@YAPEB_WIPEAUHICON__@@PEAUHBRUSH__@@0@Z" => {
                let p = m.alloc(16);
                m.st(p, 2, 'W' as u64);
                m.r[0] = p;
            }
            "?SetInt@CRunInfo@@QEBAXH_J@Z" => self.set_arg(int_arg(rdx), V::Int(r8 as i64)),
            "?StoreIntResult@CRunInfo@@QEBAX_J@Z" => self.result = Some(V::Int(rdx as i64)),
            "?StoreFloatResult@CRunInfo@@QEBAXN@Z" => self.result = Some(V::Flt(f64::from_bits(m.xl(1)))),
            "?SetArgError@CRunInfo@@QEBAXFH@Z" => panic_any(Thrown(rdx as u16 as i16 as i32)),
            "?FillInArray@CRunInfo@@QEBAXHAEAVCArrObj@@@Z" | "?FillInArray0@CRunInfo@@QEBAXHAEAVCArrObj@@@Z" => {
                self.fill_in_array(m, int_arg(rdx), r8)
            }
            "?GetCString@CRunInfo@@QEBAXHAEAV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@@Z" => {
                let text = self.str_arg(int_arg(rdx));
                let units: Vec<u32> = text.encode_utf16().map(|u| u as u32).collect();
                self.world.strs.write(m, r8, &units, true);
            }
            "?GetConstStringHolder@CRunInfo@@QEBAAEBV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@H@Z" => {
                let text = self.str_arg(int_arg(rdx));
                let units: Vec<u32> = text.encode_utf16().map(|u| u as u32).collect();
                let holder = m.alloc(8);
                self.world.strs.write(m, holder, &units, true);
                m.r[0] = holder;
            }
            "?GetStringHolder@CRunInfo@@QEBAPEAV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@H_N@Z" => {
                let idx = int_arg(rdx);
                let text = self.str_arg(idx);
                let units: Vec<u32> = text.encode_utf16().map(|u| u as u32).collect();
                let holder = m.alloc(8);
                self.world.strs.write(m, holder, &units, true);
                self.holders.push((idx, holder));
                m.r[0] = holder;
            }
            "?GetConstString@CRunInfo@@QEBAPEB_WH@Z" => {
                let text = self.str_arg(int_arg(rdx));
                let units: Vec<u16> = text.encode_utf16().collect();
                let p = m.alloc(units.len() * 2 + 2);
                for (i, u) in units.iter().enumerate() {
                    m.st(p + 2 * i as u64, 2, *u as u64);
                }
                m.r[0] = p;
            }
            "?SetCString@CRunInfo@@QEBAXHAEBV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@@Z" => {
                let text = cstring::from_units(&self.world.strs.read(m, r8, true));
                self.set_arg(int_arg(rdx), V::Str(text));
            }
            "?StoreStringResult@CRunInfo@@QEBAXAEBV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@@Z" => {
                self.result = Some(V::Str(cstring::from_units(&self.world.strs.read(m, rdx, true))));
            }
            "?StoreStringResult@CRunInfo@@QEBAXPEB_W@Z" => {
                let mut units = Vec::new();
                while m.ld(rdx + 2 * units.len() as u64, 2) != 0 {
                    units.push(m.ld(rdx + 2 * units.len() as u64, 2) as u16);
                }
                self.result = Some(V::Str(String::from_utf16_lossy(&units)));
            }
            other if cstring::call(&mut self.world.strs, m, other) => {}
            other if self.world.win.call(m, other) => {}
            // The C++ runtime and the C library: memory is the machine's heap, which is never
            // freed during a command.
            "??2@YAPEAX_K@Z" | "??_U@YAPEAX_K@Z" => m.r[0] = m.alloc(m.r[1] as usize),
            "??3@YAXPEAX@Z" | "??_V@YAXPEAX@Z" | "free" => m.free(m.r[1]),
            "?_Orphan_all@_Container_base0@std@@QEAAXXZ" => {}
            "?_Xbad_alloc@std@@YAXXZ" | "?_Xlength_error@std@@YAXPEBD@Z" | "?_Xout_of_range@std@@YAXPEBD@Z" => {
                panic_any(Thrown(-7003));
            }
            "?GetCurrentViewAndType@CRunInfo@@QEBAPEAUHWND__@@AEAVCObjType@@@Z" => {
                let h = self.vm.current_view();
                let info = self.vm.host.as_ref().and_then(|host| host.view_info(h));
                match info {
                    Some((kind, _)) => {
                        m.st(rdx, 1, internal_view_type(kind));
                        m.r[0] = hwnd_of(h);
                    }
                    None => {
                        m.st(rdx, 1, 0);
                        m.r[0] = 0;
                    }
                }
            }
            "SendMessageW" => {
                let msg = rdx as u32;
                m.r[0] = match msg {
                    0x14a7 => self.sonfile_of(m, view_of(rcx_of(m))),
                    0x15d1 => self.measure(m, view_of(rcx_of(m)), r9),
                    0x15d3 => self.search(m, view_of(rcx_of(m)), r9),
                    0x15a6 => self.chan_value(m, view_of(rcx_of(m)), r9),
                    _ => match commands::msg_handlers(msg).first().copied() {
                        Some(f) => {
                            // the window's own handler, run with an object standing for the window
                            let hwnd = rcx_of(m);
                            let this = match self.world.wnds.get(&hwnd) {
                                Some(&t) => t,
                                None if hwnd == 0 => self.main_window(m),
                                None if self.vm.host.as_ref().map(|host| !host.view_channels(view_of(hwnd)).is_empty()).unwrap_or(false) => self.time_window(m, view_of(hwnd)),
                                None => {
                                    let t = m.alloc(0x4000);
                                    m.st(t + 0x40, 8, hwnd);
                                    self.world.wnds.insert(hwnd, t);
                                    t
                                }
                            };
                            let (wp, lp) = (r8, r9);
                            m.r[1] = this;
                            m.r[2] = wp;
                            m.r[8] = lp;
                            m.push(0);
                            commands::call_at(m, self, f);
                            m.r[0]
                        }
                        None => {
                            if std::env::var_os("S2LIFT_SERVICES").is_some() {
                                eprintln!("  (message {msg:#x} to {:#x} not handled)", rcx_of(m));
                            }
                            0
                        }
                    },
                };
            }
            "?DoInit@@YAFPEAVCBaseCmd@@@Z" => {
                // a command object has been built: remember which command it is by its vtable
                // (several commands can share a class: the k-th object of a class is the k-th command that has it)
                let vt = m.ld(m.r[1], 8);
                let k = self.world.seen.entry(vt).or_insert(0);
                let names: Vec<&str> = commands::VTABLES.iter().filter(|(_, v)| *v == vt).map(|(n, _)| *n).collect();
                if let Some(n) = names.get(*k).or(names.last()) {
                    self.world.objects.insert(n.to_string(), m.r[1]);
                }
                *k += 1;
                m.r[0] = 0;
            }
            "?AfxGetThread@@YAPEAVCWinThread@@XZ" => {
                if self.world.thread == 0 {
                    let t = m.alloc(0x400);
                    let vt = m.alloc(0x400);
                    for k in 0..128u64 {
                        m.st(vt + 8 * k, 8, FAKE_CODE + k);
                    }
                    m.st(t, 8, vt);
                    let wnd = m.alloc(0x100);
                    m.st(wnd + 0x40, 8, 0x1111); // m_hWnd of the main window
                    self.world.main_wnd = wnd;
                    self.world.thread = t;
                }
                m.r[0] = self.world.thread;
            }
            "?AfxGetApp@@YAPEAVCWinApp@@XZ" => m.r[0] = self.app(m),
            "?AfxGetModuleState@@YAPEAVAFX_MODULE_STATE@@XZ" => {
                let key = name.len() as i64 + 1_000_000;
                let o = match self.world.sonfiles.get(&key) {
                    Some(&o) => o,
                    None => {
                        let o = m.alloc(0x400);
                        self.world.sonfiles.insert(key, o);
                        o
                    }
                };
                m.r[0] = o;
            }
            "?FromHandlePermanent@CWnd@@SAPEAV1@PEAUHWND__@@@Z" | "?FromHandle@CWnd@@SAPEAV1@PEAUHWND__@@@Z" => {
                // the window object behind a handle: a data view's window stand-in
                let hwnd = m.r[1];
                let h = view_of(hwnd);
                if hwnd == 0x30001 {
                    // the frame window around the views
                    if self.world.frame == 0 {
                        self.world.frame = m.alloc(0x1000);
                        m.st(self.world.frame + 0x40, 8, 0x30001);
                    }
                    m.r[0] = self.world.frame;
                    return;
                }
                let is_view = hwnd != 0 && self.vm.host.as_ref().map(|host| !host.view_channels(h).is_empty()).unwrap_or(false);
                m.r[0] = if is_view { self.time_window(m, h) } else { 0 };
            }
            "_CxxThrowException" => {
                if std::env::var_os("S2LIFT_SERVICES").is_some() {
                    eprintln!("  throw from blocks {:x?}", m.recent_blocks());
                }
                s2lift::mach::throw(m.r[1], m.r[2])
            }
            "?_Xregex_error@std@@YAXW4error_type@regex_constants@1@@Z" => {
                // the exception object: a runtime_error with the error code after it
                let obj = m.alloc(0x40);
                m.st(obj + 0x18, 4, m.r[1] & 0xffff_ffff);
                std::panic::resume_unwind(Box::new(s2lift::mach::CxxThrow { obj, info: 0 }))
            }
            "?CurrentScriptId@CRunInfo@@QEBAPEAUHWND__@@XZ" => m.r[0] = 0x30100,
            "?GError@@YAXF@Z" => panic_any(Thrown(m.r[1] as u16 as i16 as i32)),
            "ldexp" => {
                let x = f64::from_bits(m.xl(0));
                m.set_xl(0, scalbn(x, m.r[2] as u32 as i32).to_bits());
            }
            "frexp" => {
                let (mant, e) = frexp(f64::from_bits(m.xl(0)));
                m.st(m.r[2], 4, e as u32 as u64);
                m.set_xl(0, mant.to_bits());
            }
            "_time64" => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                if m.r[1] != 0 {
                    m.st(m.r[1], 8, now);
                }
                m.r[0] = now;
            }
            "_localtime64_s" => {
                // (struct tm *out, const __time64_t *t): local time, here the machine's zone
                let t = m.ld(rdx, 8) as i64;
                let (tm, ok) = localtime(t, false);
                for (i, v) in tm.iter().enumerate() {
                    m.st(m.r[1] + 4 * i as u64, 4, *v as u32 as u64);
                }
                m.r[0] = if ok { 0 } else { 22 };
            }
            "GetLocaleInfoW" => {
                // (locale, type, buffer, size): the date and time formats of a US English system
                let kind = (rdx as u32) & 0x0fff_ffff; // LOCALE_NOUSEROVERRIDE and the like are flags
                const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
                const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
                let text: String = match kind {
                    0x1f => "M/d/yyyy".into(),                   // LOCALE_SSHORTDATE
                    0x20 => "dddd, MMMM dd, yyyy".into(),        // LOCALE_SLONGDATE
                    0x1003 => "h:mm:ss tt".into(),               // LOCALE_STIMEFORMAT
                    0x1d => "/".into(),                           // LOCALE_SDATE
                    0x1e => ":".into(),                           // LOCALE_STIME
                    0x21 => "0".into(),                           // LOCALE_IDATE: month day year
                    0x22 => "0".into(),                           // LOCALE_ILDATE
                    0x23 => "0".into(),                           // LOCALE_ITIME: 12 hour clock
                    0x25 => "0".into(),                           // LOCALE_ITLZERO
                    0x28 => "AM".into(),                          // LOCALE_S1159
                    0x29 => "PM".into(),                          // LOCALE_S2359
                    0x2a..=0x30 => DAYS[(kind - 0x2a) as usize].into(),
                    0x31..=0x37 => DAYS[(kind - 0x31) as usize][..3].into(),
                    0x38..=0x43 => MONTHS[(kind - 0x38) as usize].into(),
                    0x44..=0x4f => MONTHS[(kind - 0x44) as usize][..3].into(),
                    _ => String::new(),
                };
                let units: Vec<u16> = text.encode_utf16().collect();
                if r9 > 0 {
                    for (i, u) in units.iter().enumerate() {
                        m.st(r8 + 2 * i as u64, 2, *u as u64);
                    }
                    m.st(r8 + 2 * units.len() as u64, 2, 0);
                }
                m.r[0] = units.len() as u64 + 1;
            }
            "_wtoi" => {
                let mut v = Vec::new();
                while m.ld(rcx_of(m) + 2 * v.len() as u64, 2) != 0 {
                    v.push(m.ld(rcx_of(m) + 2 * v.len() as u64, 2) as u16);
                }
                let s = String::from_utf16_lossy(&v);
                m.r[0] = crate::script::text::parse_int(s.trim_start()) as i32 as u32 as u64;
            }
            // any other call of the window system: there is no screen, so it does nothing
            other if !other.starts_with('?') && !other.starts_with('_') && other.chars().next().map_or(false, |c| c.is_ascii_uppercase()) && !other.contains('@') => {
                if std::env::var_os("S2LIFT_SERVICES").is_some() {
                    eprintln!("  (window-system call {other} ignored)");
                }
                m.r[0] = 0;
            }
            other if self.world.lenient => {
                if std::env::var_os("S2LIFT_SERVICES").is_some() {
                    eprintln!("  (service {other} skipped)");
                }
                // a constructor answers with the object it was given, which gets a vtable whose
                // routines all succeed
                if other.starts_with("??0") {
                    let obj = m.r[1];
                    if obj != 0 && m.ld(obj, 8) == 0 {
                        if self.world.stub_vtable == 0 {
                            let vt = m.alloc(8 * 0x200);
                            for k in 0..0x200u64 {
                                m.st(vt + 8 * k, 8, FAKE_CODE + 0x3000 + k);
                            }
                            self.world.stub_vtable = vt;
                        }
                        m.st(obj, 8, self.world.stub_vtable);
                    }
                    m.r[0] = obj;
                } else {
                    m.r[0] = 0;
                }
            }
            other => {
                self.missing = Some(other.to_string());
                panic!("the service {other} is not provided");
            }
        }
    }
}

/// A window handle that stands for view `h` (nothing like it exists here; the translated code
/// only passes it back).
fn hwnd_of(h: i64) -> u64 {
    0x30000 + 16 * h as u64
}

fn view_of(hwnd: u64) -> i64 {
    ((hwnd.wrapping_sub(0x30000)) / 16) as i64
}

/// Spike2's own number for a kind of view, from the number `ViewKind` gives for it.
fn internal_view_type(script_kind: i64) -> u64 {
    match script_kind {
        0 => 4,
        4 => 5,
        1 => 7,
        2 => 8,
        3 => 9,
        8 => 10,
        9 => 0xb,
        0xc => 0xd,
        0xb => 0xf,
        0x11 => 0x10,
        10 => 1,
        _ => 0,
    }
}

impl<'v, 'w> Run<'v, 'w> {
    /// The data file engine's object for view `h`, made (by opening the file with Spike2's own
    /// code) the first time it is asked for.
    /// The application object's stand-in: a zeroed object whose settings reader answers with the
    /// default it is given, also reachable at the global SONVIEW keeps it in.
    fn app(&mut self, m: &mut M) -> u64 {
        if self.world.app == 0 {
            let (o, vt) = (m.alloc(0x400), m.alloc(0x800));
            m.st(vt + 0x108, 8, FAKE_CODE + 0x108);
            m.st(o, 8, vt);
            m.st(o + 0x88, 8, 0x30100);
            self.world.app = o;
            m.st(0x1_403d_ade8 + (7u64 << 32), 8, o);
        }
        self.world.app
    }

    /// The main window's stand-in: zeroed, with the sampling configuration the application builds
    /// (a `CConfig` over a `CSamModeInfo` inside the window).
    fn main_window(&mut self, m: &mut M) -> u64 {
        if let Some(&t) = self.world.wnds.get(&0) {
            return t;
        }
        self.app(m);
        let t = m.alloc(0x20000);
        self.world.wnds.insert(0, t);
        let saved = m.r;
        // the window's own constructor, with services nothing provides answering zero
        {
            self.world.lenient = true;
            let sp = m.r[4];
            m.r[1] = t;
            m.push(0);
            let r = catch_unwind(AssertUnwindSafe(|| commands::call_at(m, self, 0x1_4008_6a50 + (7u64 << 32))));
            self.world.lenient = false;
            m.r[4] = sp;
            m.r = saved;
            if r.is_ok() && m.ld(t + 0xc870, 8) != 0 {
                self.register_main_window(m, t);
                return t;
            }
            if std::env::var_os("S2LIFT_SERVICES").is_some() {
                let why = match &r {
                    Err(p) => p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default(),
                    Ok(()) => "constructed no configuration".to_string(),
                };
                eprintln!("  (the main window's constructor did not finish: {why})");
            }
        }
        m.r[1] = t + 0x1d0;
        commands::call_export("??0CSamModeInfo@@QEAA@XZ", m, self);
        m.r = saved;
        // the colour tables the window's constructor fills from the defaults in the program
        let bias = 7u64 << 32;
        for i in 0..0x28u64 {
            let c = m.ld(0x1_4000_0000 + 0x289bf0 + 4 * i + bias, 4);
            m.st(0x1_4000_0000 + 0x419260 + 4 * i + bias, 4, c);
            let w = m.ld(0x1_4000_0000 + 0x289c90 + 2 * i + bias - 0x0, 2) as u16 as i16;
            let v = if w < 0 { 0xffff_ffff } else { m.ld(0x1_4000_0000 + 0x289bf0 + 4 * w as u64 + bias, 4) };
            m.st(0x1_4000_0000 + 0x4191c0 + 4 * i + bias, 4, v);
        }
        // the IIR filter bank the application keeps (and the IIRDefine it edits)
        let def = m.alloc(0xf8);
        m.r[1] = def;
        commands::call_export("??0IIRDefine@@QEAA@XZ", m, self);
        m.st(t + 0xcdd0, 8, def);
        let bank = m.alloc(0xbb0);
        m.r[1] = bank;
        commands::call_export("??0CIIRFilterBank@@QEAA@XZ", m, self);
        m.st(bank, 8, 0x1_402a_5630 + (7u64 << 32));
        m.st(t + 0xcdf0, 8, bank);
        // the application's own start-up of the sampling settings (their defaults)
        m.r[1] = t;
        m.push(0);
        commands::call_at(m, self, 0x1_4008_bc80 + (7u64 << 32));
        m.r = saved;
        let cfg = m.alloc(0x10);
        m.st(cfg, 8, 0x1_402a_7de8 + (7u64 << 32));
        m.st(cfg + 8, 8, t + 0x1d0);
        m.st(t + 0xc870, 8, cfg);
        self.register_main_window(m, t);
        t
    }

    /// Tell the shared library (SpikeUtl) which window is the application's main window.
    fn register_main_window(&mut self, m: &mut M, t: u64) {
        let saved = m.r;
        m.r[1] = t;
        commands::call_export("?CEDMainWndSet@@YAXPEAVCWnd@@@Z", m, self);
        m.r = saved;
    }

    /// The stand-in for a data view's window (SONVIEW's `CTimeWindow`): its real vtable, its document
    /// (holding the data file object), the seconds per tick, and a channel view for every channel in
    /// the file, made by SONVIEW's own routine and kept in the window's list by channel number.
    fn time_window(&mut self, m: &mut M, h: i64) -> u64 {
        if let Some(&w) = self.world.twins.get(&h) {
            return w;
        }
        let file = self.sonfile_of(m, h);
        let chans = self.vm.host.as_ref().map(|host| host.view_channels(h)).unwrap_or_default();
        let tf = chans.first().and_then(|(c, _)| self.vm.host.as_ref().and_then(|host| host.wave_info(h, *c))).map(|x| x.0).unwrap_or(1e-5);
        if self.world.measure_buf == 0 {
            self.world.measure_buf = m.alloc(4 * 8000 + 64);
        }
        m.st(0x1_403d_a7e8 + (7u64 << 32), 8, self.world.measure_buf);
        self.app(m);
        let win = m.alloc(0x2000);
        self.world.twins.insert(h, win);
        // SONVIEW's own constructor (with the services nothing provides skipped)
        {
            self.world.lenient = true;
            let sp = m.r[4];
            let saved = m.r;
            m.r[1] = win;
            m.push(0);
            let r = catch_unwind(AssertUnwindSafe(|| commands::call_at(m, self, 0x1_4012_f370 + (7u64 << 32))));
            self.world.lenient = false;
            m.r[4] = sp;
            m.r = saved;
            if r.is_err() && std::env::var_os("S2LIFT_SERVICES").is_some() {
                eprintln!("  (the window's constructor did not finish)");
            }
        }
        // the window's vtable, with the end-of-data question (slot 0x468) answered by the data file
        // itself: the window's own routine scans the channels for it
        {
            let real = 0x1_402c_56c8 + (7u64 << 32);
            let copy = m.alloc(0x800);
            for k in 0..0xe0u64 {
                let v = m.ld(real + 8 * k, 8);
                m.st(copy + 8 * k, 8, v);
            }
            m.st(copy + 0x468, 8, FAKE_CODE + 0x468);
            m.st(win, 8, copy);
        }
        m.st(win + 0x40, 8, hwnd_of(h));
        let doc = m.alloc(0x800);
        {
            self.world.lenient = true;
            let sp = m.r[4];
            let saved = m.r;
            m.r[1] = doc;
            m.push(0);
            let r = catch_unwind(AssertUnwindSafe(|| commands::call_at(m, self, 0x1_4012_3d60 + (7u64 << 32))));
            self.world.lenient = false;
            m.r[4] = sp;
            m.r = saved;
            if r.is_err() && std::env::var_os("S2LIFT_SERVICES").is_some() {
                eprintln!("  (the document's constructor did not finish)");
            }
        }
        m.st(doc + 0x270, 8, file);
        m.st(win + 0xd8, 8, doc);
        // the window's manager of cursors, which the window makes when it is created
        {
            self.world.lenient = true;
            let sp = m.r[4];
            let saved = m.r;
            m.r[1] = win;
            m.r[2] = 0;
            m.r[8] = 0;
            m.r[9] = 0;
            m.push(0);
            let r = catch_unwind(AssertUnwindSafe(|| commands::call_at(m, self, 0x1_400c_cbb0 + (7u64 << 32))));
            self.world.lenient = false;
            m.r[4] = sp;
            m.r = saved;
            let mgr = m.ld(win + 0x5b0, 8);
            if mgr != 0 {
                // it is a window of its own
                m.st(mgr + 0x40, 8, 0x30002);
            }
            if r.is_err() && std::env::var_os("S2LIFT_SERVICES").is_some() {
                let why = match &r {
                    Err(p) => p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default(),
                    Ok(()) => String::new(),
                };
                eprintln!("  (the cursor manager's constructor did not finish: {why})");
            }
        }
        m.st(win + 0x880, 8, tf.to_bits());
        self.world.measure_file = file;
        self.world.measure_tf = tf;
        let ptrs = m.alloc(8 * 40);
        let mut count = 0u64;
        self.world.lenient = true;
        for (c, kind) in chans {
            let sp = m.r[4];
            let saved = m.r;
            m.r[1] = win;
            m.r[2] = file;
            m.r[8] = (c - 1) as u64;
            m.push(0);
            let r = catch_unwind(AssertUnwindSafe(|| commands::call_at(m, self, 0x1_4013_0230 + (7u64 << 32))));
            let view = m.r[0];
            m.r[4] = sp;
            m.r = saved;
            if r.is_ok() && view != 0 {
                m.st(view + 0x140, 8, win);
                // a waveform view draws lines unless it is told otherwise
                if matches!(kind, 1 | 6 | 7 | 9) {
                    m.st(view + 0x178, 4, 0xb);
                }
                m.st(ptrs + 8 * (c - 1) as u64, 8, view);
                count += 1;
            } else if std::env::var_os("S2LIFT_SERVICES").is_some() {
                let why = match &r {
                    Err(p) => p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default(),
                    Ok(()) => "returned no view".to_string(),
                };
                eprintln!("  (no channel view for channel {c}: {why})");
            }
        }
        self.world.lenient = false;
        // the vector of channel views, indexed by channel (empty places are null)
        let _ = count;
        m.st(win + 0x240, 8, ptrs);
        m.st(win + 0x248, 8, ptrs + 8 * 32);
        m.st(win + 0x250, 8, ptrs + 8 * 40);
        win
    }

    /// The vtable of SONVIEW's class for a channel of this kind (the number `ChanKind` gives).
    fn view_vtable(kind: i64) -> Option<u64> {
        let va = match kind {
            1 => 0x1_4028_e1a8u64,
            9 => 0x1_4028_dca8,
            2..=4 => 0x1_4029_7678,
            5 => 0x1_402a_ab88,
            6 => 0x1_4028_f468,
            7 => 0x1_402b_61e8,
            8 => 0x1_402c_6af8,
            _ => return None,
        };
        Some(va + (7u64 << 32))
    }

    /// The channel view at `chan` (a place in the window's list of channels) in the data view's window stand-in, with its vtable.
    fn mock_view(&mut self, m: &mut M, h: i64, chan: i64) -> Option<(u64, u64)> {
        let win = self.time_window(m, h);
        self.world.measure_file = m.ld(m.ld(win + 0xd8, 8) + 0x270, 8);
        self.world.measure_tf = f64::from_bits(m.ld(win + 0x880, 8));
        let (begin, end) = (m.ld(win + 0x240, 8), m.ld(win + 0x248, 8));
        if chan < 0 || begin == 0 || begin + 8 * chan as u64 >= end {
            return None;
        }
        let view = m.ld(begin + 8 * chan as u64, 8);
        if view == 0 {
            return None;
        }
        Some((view, m.ld(view, 8)))
    }

    /// `ChanValue`: the value of a channel at a time, from the channel view's own routine (virtual slot
    /// 0x4c8), which interpolates according to the channel's draw mode.
    fn chan_value(&mut self, m: &mut M, h: i64, lp: u64) -> u64 {
        let chan = m.ld(lp + 8, 4) as u32 as i32 as i64;
        let t = f64::from_bits(m.ld(lp + 0x10, 8));
        let desc = m.ld(lp, 8);
        let Some((view, vtable)) = self.mock_view(m, h, chan) else { return 0 };
        let tf = self.world.measure_tf;
        let big = 0x7000_0000_0000_0000i64;
        let x = t / tf;
        let ticks = if x > big as f64 { big } else if x >= 0.0 { (x + 0.5) as i64 } else if x >= -(big as f64) { (x - 0.5) as i64 } else { -big };
        let out = m.alloc(8);
        let saved = m.r;
        let target = m.ld(vtable + 0x4c8, 8);
        m.r[1] = view;
        m.r[2] = ticks as u64;
        m.r[8] = 0;
        m.r[9] = out;
        m.r[4] -= 0x40;
        m.st(m.r[4] + 0x20, 8, if desc != 0 && (m.ld(desc, 2) as u16 as i16) != -1 { 0 } else { 0 });
        m.push(0);
        commands::call_at(m, self, target);
        let ok = m.r[0] as u8 != 0;
        let v = m.ld(out, 8);
        m.r = saved;
        if ok {
            m.st(lp + 0x18, 8, v);
        }
        ok as u64
    }

    /// `ChanSearch`: the channel view's own search routine (virtual slot 0x398) on a stand-in object that
    /// carries the real vtable.
    fn search(&mut self, m: &mut M, h: i64, lp: u64) -> u64 {
        const NO_CHAN: u64 = -1505i64 as u64;
        let chan = m.ld(lp + 4, 4) as u32 as i32 as i64;
        let Some((view, vtable)) = self.mock_view(m, h, chan) else { return NO_CHAN };
        let saved = m.r;
        let target = m.ld(vtable + 0x398, 8);
        m.r[1] = view;
        m.r[2] = lp;
        m.r[4] -= 0x40;
        m.push(0);
        commands::call_at(m, self, target);
        let found = m.r[0] as u8 != 0;
        m.r = saved;
        found as u64
    }

    /// `ChanMeasure`: SONVIEW's own measuring routine, run on stand-ins for the channel view and its owner.
    fn measure(&mut self, m: &mut M, h: i64, lp: u64) -> u64 {
        const FAIL: u64 = -1551i64 as u64;
        let chan = m.ld(lp, 4) as u32 as i32 as i64;
        let ty = m.ld(lp + 4, 4) as u32 as i32 - 0x13;
        let flag = m.ld(lp + 0x14, 4);
        let (t0, t1) = (f64::from_bits(m.ld(lp + 0x30, 8)), f64::from_bits(m.ld(lp + 0x28, 8)));
        let Some((view, vtable)) = self.mock_view(m, h, chan) else {
            if std::env::var_os("S2LIFT_SERVICES").is_some() {
                eprintln!("  (measure: no view for channel {chan})");
            }
            return FAIL;
        };
        if std::env::var_os("S2LIFT_SERVICES").is_some() {
            eprintln!("  (measure: view {view:#x} vtable {vtable:#x} chan {} kind {} mode {:#x})", m.ld(view + 0x170, 2), m.ld(view + 0x174, 4), m.ld(view + 0x178, 4));
        }
        let out = m.alloc(8);
        let tf = self.world.measure_tf;
        let (a, b) = ((t0 / tf).round() as i64, (t1 / tf).round() as i64);
        let saved = m.r;
        let target = m.ld(vtable + 0x4d0, 8);
        m.r[1] = view;
        m.r[2] = a as u64;
        m.r[8] = b as u64;
        m.r[9] = ty as u64;
        m.r[4] -= 0x50;
        m.st(m.r[4] + 0x20, 8, out);
        m.st(m.r[4] + 0x28, 8, flag);
        m.push(0);
        commands::call_at(m, self, target);
        let ok = m.r[0] as u8 != 0;
        let v = m.ld(out, 8);
        m.r = saved;
        if ok {
            m.st(lp + 0x18, 8, v);
            0
        } else {
            FAIL
        }
    }

    fn sonfile_of(&mut self, m: &mut M, h: i64) -> u64 {
        if let Some(&o) = self.world.sonfiles.get(&h) {
            return o;
        }
        let info = self.vm.host.as_ref().and_then(|host| host.view_info(h));
        if std::env::var_os("S2LIFT_SERVICES").is_some() {
            eprintln!("  sonfile for view {h}: {info:?}");
        }
        let Some((_, Some(path))) = info else { return 0 };
        let text = path.to_string_lossy().into_owned();
        let obj = m.alloc(0x50000);
        let name = m.alloc(2 * text.len() + 2);
        for (i, u) in text.encode_utf16().enumerate() {
            m.st(name + 2 * i as u64, 2, u as u64);
        }
        let saved = m.r;
        m.r[1] = obj;
        m.r[2] = name;
        // the machine is borrowed by the caller; the constructor runs on it, then its registers are restored
        commands::call_export("??0CSonFile@@QEAA@PEB_W@Z", m, self);
        m.r = saved;
        self.world.sonfiles.insert(h, obj);
        obj
    }

    fn str_arg(&self, idx: i32) -> String {
        match self.vm.a_str(self.arg(idx)) {
            Ok(s) => s,
            Err(e) => panic_any(Thrown(e.0)),
        }
    }

    fn int_arg(&self, idx: i32) -> i64 {
        match self.vm.a_int(self.arg(idx)) {
            Ok(v) => v,
            Err(e) => panic_any(Thrown(e.0)),
        }
    }

    /// `GetArgType`: the type of argument `idx`, in the 28 bytes of a `TypeDesc` at `td`.
    fn arg_type(&mut self, m: &mut M, idx: i32, td: u64) {
        let (vt, dims): (u64, Vec<usize>) = match self.arg(idx).clone() {
            Arg::Val(V::Int(_)) => (1, vec![]),
            Arg::Val(V::Flt(_)) => (2, vec![]),
            Arg::Val(V::Str(_)) => (3, vec![]),
            Arg::Ref(r) => match self.vm.cells.get(match r { Ref::Cell(c) | Ref::Elem(c, _) => c }) {
                Some(Cell::Array { ty, dims, .. }) if matches!(r, Ref::Cell(_)) => {
                    (match ty { Ty::Int => 1, Ty::Float => 2, Ty::Str => 3 }, dims.clone())
                }
                Some(Cell::Array { ty, .. }) => (match ty { Ty::Int => 1, Ty::Float => 2, Ty::Str => 3 }, vec![]),
                Some(Cell::Scalar { ty, .. }) => (match ty { Ty::Int => 1, Ty::Float => 2, Ty::Str => 3 }, vec![]),
                None => (0, vec![]),
            },
        };
        // InitTypeDesc: type 2, no dimensions, every size -1; then what the argument has.
        m.st(td, 4, vt);
        m.st(td + 4, 2, dims.len() as u64);
        for k in 0..5 {
            m.st(td + 8 + 4 * k as u64, 4, dims.get(k).map(|d| *d as u64).unwrap_or(0xffff_ffff));
        }
    }

    /// `Resize`: give the last dimension of array argument `idx` the size `n`, keeping the
    /// elements that have a place in both, and refresh the `CArrObj`.
    fn resize(&mut self, m: &mut M, idx: i32, obj: u64, n: i32) {
        let Some(c) = self.vm.a_array(self.arg(idx)) else { panic_any(Thrown(-0x1b7e)) };
        let Some(k) = self.bound.iter().position(|b| b.cell == c) else { panic_any(Thrown(-0x1b7e)) };
        let old = self.bound[k].clone_shape();
        let mut dims = old.1.clone();
        let last = dims.len() - 1;
        if dims[last] as i32 == n {
            return;
        }
        dims[last] = n.max(0) as usize;
        let total: usize = dims.iter().product();
        let addr = m.alloc((total * 8).max(8));
        // copy the elements whose indices exist in both shapes
        let old_total: usize = old.1.iter().product();
        for flat in 0..old_total {
            let mut rest = flat;
            let mut idxs = vec![0usize; old.1.len()];
            for d in (0..old.1.len()).rev() {
                idxs[d] = rest % old.1[d].max(1);
                rest /= old.1[d].max(1);
            }
            if idxs.iter().zip(&dims).all(|(i, d)| i < d) {
                let to = idxs.iter().zip(&dims).fold(0, |acc, (i, d)| acc * d + i);
                let v = m.ld(old.0 + 8 * flat as u64, 8);
                m.st(addr + 8 * to as u64, 8, v);
            }
        }
        let ty = self.bound[k].ty;
        self.bound[k].addr = addr;
        self.bound[k].dims = dims.clone();
        self.write_obj(m, obj, ty, &dims, addr);
    }

    fn set_arg(&mut self, idx: i32, v: V) {
        match *self.arg(idx) {
            Arg::Ref(r) => {
                if let Err(e) = self.vm.set(r, v) {
                    panic_any(Thrown(e.0));
                }
            }
            Arg::Val(_) => panic_any(Thrown(-0x1b74)),
        }
    }
}

/// Is `name` one of the commands translated?
pub fn has(name: &str) -> bool {
    commands::COMMANDS.contains(&name)
}

/// Is `name` one the environment asks to be run from Spike2's code even though a hand-written
/// version exists (`S2LIFT_FORCE=Name,Name` or `all`)?
pub fn forced(name: &str) -> bool {
    if std::env::var("S2LIFT_SKIP").map(|v| v.split(',').any(|n| n == name)).unwrap_or(false) {
        return false;
    }
    match std::env::var("S2LIFT_FORCE") {
        Ok(v) => v == "all" || v.split(',').any(|n| n == name),
        Err(_) => false,
    }
}

/// The translated code's world, kept from one command to the next: its memory (the data files
/// it has open live there), its strings and its open operating-system files.
pub struct World {
    m: Option<M>,
    strs: cstring::State,
    win: win::Win,
    /// The command objects built so far, by command name.
    objects: std::collections::HashMap<String, u64>,
    /// The data file engine's object for each view that has one.
    sonfiles: std::collections::HashMap<i64, u64>,
    /// Whether the command objects have been built.
    registered: bool,
    thread: u64,
    main_wnd: u64,
    /// The window object handed to message handlers, by window handle.
    wnds: std::collections::HashMap<u64, u64>,
    /// What the stand-in for a channel's owner answers while a measurement runs: the data file object and seconds per tick.
    measure_file: u64,
    measure_tf: f64,
    /// The buffer the measurement routines read samples into.
    measure_buf: u64,
    /// The stand-in for the application object (settings are read from it).
    app: u64,
    /// How many command objects of each class have been built.
    seen: std::collections::HashMap<u64, usize>,
    /// While the main window is being built, services nothing provides answer zero.
    lenient: bool,
    /// The window stand-ins of the data views, by view handle.
    twins: std::collections::HashMap<i64, u64>,
    /// A vtable of do-nothing routines for objects whose constructors were skipped.
    stub_vtable: u64,
    /// The frame window stand-in (parent of the views).
    frame: u64,
    /// The buffer `_ecvt` answers in.
    cvt_buf: u64,
}

impl Default for World {
    fn default() -> World {
        let mut m = commands::machine();
        // compiler.dll keeps a pointer to the script application's object (windows, options) in a
        // global that the application sets when it starts; the code reads window handles from it.
        let app = m.alloc(0x400);
        m.st(0x2_8008_bf20, 8, app);
        m.st(app + 0x88, 8, 0x1111); // the main window
        World { m: Some(m), strs: Default::default(), win: Default::default(), objects: Default::default(), sonfiles: Default::default(), registered: false, thread: 0, main_wnd: 0, wnds: Default::default(), measure_file: 0, measure_tf: 0.0, measure_buf: 0, app: 0, seen: Default::default(), lenient: false, twins: Default::default(), stub_vtable: 0, frame: 0, cvt_buf: 0 }
    }
}

impl Vm {
    /// Run a translated command. The run information the code reads directly is its argument
    /// count and the number of the variant the arguments matched.
    pub(super) fn run_lifted(&mut self, name: &str, variant: usize, args: &[Arg]) -> Result<Option<Option<V>>, RunError> {
        let mut world = self.lifted_world.take().unwrap_or_default();
        let mut m = world.m.take().expect("the machine is out only during a command");
        m.reset_stack();
        let info = m.alloc(64);
        m.st(info, 4, args.len() as u64);
        m.st(info + 4, 4, variant.max(1) as u64);
        m.r[2] = info;
        let mut run = Run { vm: self, args, result: None, bound: Vec::new(), missing: None, world: &mut *world, holders: Vec::new() };
        let quiet = std::panic::take_hook();
        if std::env::var_os("S2LIFT_TRACE").is_none() {
            std::panic::set_hook(Box::new(|_| {}));
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            // The command objects, built (all at once) by Spike2's own code the first time.
            if !run.world.registered {
                run.world.registered = true;
                // each module's startup (static initialisers), then the command objects
                // an initialiser that reaches something not provided is skipped
                for i in 0..commands::INIT_COUNT {
                    let saved_sp = m.r[4];
                    let r = catch_unwind(AssertUnwindSafe(|| commands::init_one(i, &mut m, &mut run)));
                    m.r[4] = saved_sp;
                    m.reset_stack();
                    let _ = r;
                }
                commands::register_all(&mut m, &mut run);
                // the commands that configure sampling hold the application's configuration object
                let saved_sp = m.r[4];
                let r = catch_unwind(AssertUnwindSafe(|| {
                    let t = run.main_window(&mut m);
                    let cfg = m.ld(t + 0xc870, 8);
                    m.st(0x1_8009_ea28, 8, cfg);
                }));
                m.r[4] = saved_sp;
                m.reset_stack();
                let _ = r;
            }
            let obj = match run.world.objects.get(name) {
                Some(&o) => o,
                None => {
                    let o = m.alloc(0x1000);
                    run.world.objects.insert(name.to_string(), o);
                    o
                }
            };
            m.reset_stack();
            m.r[1] = obj;
            m.r[2] = info;
            commands::run(name, &mut m, &mut run);
            // the status Execute returns: zero, or the error code the script stops with
            let status = m.r[0] as u32 as i32;
            if status < 0 {
                panic_any(Thrown(status));
            }
        }));
        std::panic::set_hook(quiet);
        let result = match outcome {
            Ok(()) => {
                run.copy_back(&m);
                // A command that returns a value and did not store one gives zero.
                let mut result = run.result.take();
                if result.is_none() {
                    result = match crate::script::commands::find(name).map(|(_, c)| c.ret) {
                        Some(crate::script::commands::Ret::Int) => Some(V::Int(0)),
                        Some(crate::script::commands::Ret::Real) => Some(V::Flt(0.0)),
                        Some(crate::script::commands::Ret::Str) => Some(V::Str(String::new())),
                        _ => None,
                    };
                }
                Ok(Some(result))
            }
            Err(payload) => {
                if let Some(t) = payload.downcast_ref::<Thrown>() {
                        {
                        let t = t.0;
                        let vm = run.vm;
                        world.m = Some(m);
                        vm.lifted_world = Some(world);
                        return Err(RunError(t));
                    }
                }
                let why = match payload.downcast_ref::<String>() {
                    Some(s) => s.clone(),
                    None => payload.downcast_ref::<&str>().map(|s| s.to_string()).unwrap_or_default(),
                };
                run.vm.lifted_error = Some(why);
                Err(RunError(ERR_LIFTED))
            }
        };
        let vm = run.vm;
        world.m = Some(m);
        vm.lifted_world = Some(world);
        result
    }
}

/// `ldexp`: x * 2^n, rounded once.
fn scalbn(mut x: f64, mut n: i32) -> f64 {
    if x == 0.0 || !x.is_finite() || n == 0 {
        return x;
    }
    // scale in steps that cannot overflow the power of two; a subnormal result is rounded once
    // by the last multiplication
    while n > 1023 {
        x *= f64::from_bits(0x7fe0_0000_0000_0000);
        n -= 1023;
        if x.is_infinite() {
            return x;
        }
    }
    while n < -1022 {
        // stay normal until the last step so that the rounding happens once
        x *= f64::from_bits(0x0010_0000_0000_0000) * 2.0f64.powi(0);
        n += 1022;
        if x == 0.0 {
            return x;
        }
    }
    x * f64::from_bits(((n + 1023) as u64) << 52)
}

/// `frexp`: the mantissa in [0.5, 1) and the exponent.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // subnormal: scale up first
        let (m, e) = frexp(x * f64::from_bits(0x4350_0000_0000_0000)); // 2^54
        return (m, e - 54);
    }
    (f64::from_bits(bits & 0x800f_ffff_ffff_ffff | 0x3fe0_0000_0000_0000), exp - 1022)
}

fn rcx_of(m: &M) -> u64 {
    m.r[1]
}

/// Broken-down local time of a Unix time, in a C `struct tm`'s order: seconds, minutes, hours, day
/// of month, month (from 0), years since 1900, weekday (Sunday 0), day of year, daylight saving.
/// The machine's own time zone, from the C library.
pub(super) fn localtime(t: i64, utc: bool) -> ([i32; 9], bool) {
    #[repr(C)]
    struct Tm {
        f: [i32; 9],
        // glibc and others add a zone offset and name after the standard fields
        extra: [i64; 4],
    }
    let mut tm = Tm { f: [0; 9], extra: [0; 4] };
    #[cfg(unix)]
    let ok = {
        extern "C" {
            fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
            fn gmtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
        }
        unsafe { !(if utc { gmtime_r(&t, &mut tm) } else { localtime_r(&t, &mut tm) }).is_null() }
    };
    #[cfg(windows)]
    let ok = {
        extern "C" {
            fn _localtime64_s(out: *mut Tm, t: *const i64) -> i32;
        }
        unsafe { _localtime64_s(&mut tm, &t) == 0 }
    };
    #[cfg(not(any(unix, windows)))]
    let ok = false;
    if ok {
        return (tm.f, true);
    }
    // no C library to ask: UTC
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, mo, d) = civil_from_days(days);
    let wday = (days + 4).rem_euclid(7);
    let yday = days - days_from_civil(y, 1, 1);
    ([(secs % 60) as i32, (secs / 60 % 60) as i32, (secs / 3600) as i32, d as i32, mo as i32 - 1, (y - 1900) as i32, wday as i32, yday as i32, 0], true)
}

/// Local time as a Unix time (`mktime`).
pub(super) fn mktime(f: &[i32]) -> i64 {
    #[repr(C)]
    struct Tm {
        f: [i32; 9],
        extra: [i64; 4],
    }
    let mut tm = Tm { f: [0; 9], extra: [0; 4] };
    tm.f.copy_from_slice(&f[..9]);
    tm.f[8] = -1;
    #[cfg(unix)]
    {
        extern "C" {
            fn mktime(tm: *mut Tm) -> i64;
        }
        unsafe { mktime(&mut tm) }
    }
    #[cfg(not(unix))]
    {
        days_from_civil(tm.f[5] as i64 + 1900, tm.f[4] as i64 + 1, tm.f[3] as i64) * 86400 + tm.f[2] as i64 * 3600 + tm.f[1] as i64 * 60 + tm.f[0] as i64
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A host with only the operating system and the C++ runtime behind it: enough to run the data
/// file engine (`CSonFile`) without a script around it.
#[derive(Default)]
pub struct Bare {
    strs: cstring::State,
    win: win::Win,
}

impl Svc for Bare {
    fn import(&mut self, m: &mut M, id: u32) {
        let name = commands::IMPORT_NAMES[id as usize];
        match name {
            "??2@YAPEAX_K@Z" | "??_U@YAPEAX_K@Z" => m.r[0] = m.alloc(m.r[1] as usize),
            "??3@YAXPEAX@Z" | "??_V@YAXPEAX@Z" | "free" => m.free(m.r[1]),
            "?_Orphan_all@_Container_base0@std@@QEAAXXZ" => {}
            other if cstring::call(&mut self.strs, m, other) => {}
            other if self.win.call(m, other) => {}
            other => panic!("the service {other} is not provided"),
        }
    }
}

impl Bare {
    /// Call an exported routine of the data file engine by its decorated name.
    pub fn call(&mut self, m: &mut M, name: &str) {
        if !commands::call_export(name, m, self) {
            panic!("{name} was not translated");
        }
    }
}
