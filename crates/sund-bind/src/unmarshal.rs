//! Schema-driven JSON unmarshal: reactor hooks + entry points.
//!
//! Ported from `ndec/impl/bind.c` §2-§5.

use crate::arena::Arena;
use crate::type_info::{Field, Kind, TypeInfo};
use sund_core::kernel;
use sund_core::reactor::Reactor;
use sund_core::types::*;

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// Bind-specific error codes (start at 32 to avoid collision with kernel codes).
pub const ERR_BIND_TYPE_MISMATCH: i32 = 32;
pub const ERR_BIND_NUMBER_RANGE: i32 = 33;
pub const ERR_BIND_UNKNOWN_FIELD: i32 = 34;
pub const ERR_BIND_OOM: i32 = 35;

// ---------------------------------------------------------------------------
// Options / Error
// ---------------------------------------------------------------------------

/// Options for `unmarshal_ex`.
#[derive(Clone, Debug, Default)]
pub struct UnmarshalOpts {
    /// If `true`, unknown JSON fields cause an error instead of being skipped.
    pub strict: bool,
}

/// Detailed error from unmarshal.
#[derive(Clone, Debug, Default)]
pub struct UnmarshalError {
    /// Positive kernel/bind error code, 0 on success.
    pub code: i32,
    /// JSON byte offset (0 if unknown).
    pub pos: usize,
    /// Human-readable message (static; do not free).
    pub message: &'static str,
}

// ---------------------------------------------------------------------------
// Internal bind state
// ---------------------------------------------------------------------------

/// Frame kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameKind {
    Struct,
    Array,
}

/// A bind frame tracks one JSON container in progress.
struct BindFrame {
    kind: FrameKind,

    // FR_STRUCT fields
    obj: *mut u8,
    type_info: Option<&'static TypeInfo>,
    pending: Option<&'static Field>,

    // FR_ARRAY fields
    scratch: Vec<u8>,
    count: usize,
    cap: usize,
    elem_size: usize,
    elem_kind: Option<Kind>,
    elem_type: Option<&'static TypeInfo>,
    dst_ptr_addr: *mut u8,
    dst_len_addr: *mut usize,
}

impl BindFrame {
    fn new_struct(obj: *mut u8, type_info: Option<&'static TypeInfo>) -> Self {
        Self {
            kind: FrameKind::Struct,
            obj,
            type_info,
            pending: None,
            scratch: Vec::new(),
            count: 0,
            cap: 0,
            elem_size: 0,
            elem_kind: None,
            elem_type: None,
            dst_ptr_addr: std::ptr::null_mut(),
            dst_len_addr: std::ptr::null_mut(),
        }
    }

    fn new_array(
        elem_kind: Kind,
        elem_type: Option<&'static TypeInfo>,
        elem_size: usize,
        dst_ptr_addr: *mut u8,
        dst_len_addr: *mut usize,
    ) -> Self {
        Self {
            kind: FrameKind::Array,
            obj: std::ptr::null_mut(),
            type_info: None,
            pending: None,
            scratch: Vec::new(),
            count: 0,
            cap: 0,
            elem_size,
            elem_kind: Some(elem_kind),
            elem_type,
            dst_ptr_addr,
            dst_len_addr,
        }
    }
}

/// Bind state: the reactor's user-data.
struct BindState {
    stack: Vec<BindFrame>,
    arena: *mut Arena,
    strict: bool,
    err_code: i32,
    err_message: &'static str,
}

/// Where the next value should be written.
struct BindSlot {
    dst: *mut u8,
    kind: Kind,
    type_info: Option<&'static TypeInfo>,
}

impl BindState {
    fn top(&mut self) -> &mut BindFrame {
        self.stack.last_mut().expect("empty bind stack")
    }

    fn fail(&mut self, code: i32, msg: &'static str) -> i32 {
        if self.err_code == 0 {
            self.err_code = code;
            self.err_message = msg;
        }
        -2 // negative reactor return => kernel abort
    }

    fn arena(&mut self) -> &mut Arena {
        unsafe { &mut *self.arena }
    }

    /// Resolve where the next incoming value should be written.
    fn slot_for_value(&mut self) -> Result<BindSlot, i32> {
        let top = self.stack.last_mut().unwrap();
        match top.kind {
            FrameKind::Struct => {
                let field = match top.pending {
                    Some(f) => f,
                    None => return Err(self.fail(ERR_BIND_TYPE_MISMATCH, "no pending field")),
                };
                let dst = unsafe { top.obj.add(field.offset) };
                Ok(BindSlot {
                    dst,
                    kind: field.kind,
                    type_info: field.elem_type,
                })
            }
            FrameKind::Array => {
                let elem_size = top.elem_size;
                if top.count >= top.cap {
                    let new_cap = if top.cap == 0 { 16 } else { top.cap * 2 };
                    top.scratch.resize(new_cap * elem_size, 0);
                    top.cap = new_cap;
                }
                let offset = top.count * elem_size;
                // Zero-init the new element.
                for b in &mut top.scratch[offset..offset + elem_size] {
                    *b = 0;
                }
                let dst = top.scratch.as_mut_ptr().wrapping_add(offset);
                let kind = top.elem_kind.unwrap_or(Kind::Bool);
                let type_info = top.elem_type;
                Ok(BindSlot {
                    dst,
                    kind,
                    type_info,
                })
            }
        }
    }

    /// Advance the current frame one value forward.
    fn advance(&mut self) {
        let top = self.stack.last_mut().unwrap();
        match top.kind {
            FrameKind::Struct => {
                top.pending = None;
            }
            FrameKind::Array => {
                top.count += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scalar writers
// ---------------------------------------------------------------------------

unsafe fn write_number(bs: &mut BindState, dst: *mut u8, kind: Kind, raw: &[u8]) -> i32 {
    // NUL-terminate for parsing.
    let mut buf = [0u8; 64];
    if raw.len() >= buf.len() {
        return bs.fail(ERR_BIND_NUMBER_RANGE, "number too long");
    }
    buf[..raw.len()].copy_from_slice(raw);
    let s = std::str::from_utf8(&buf[..raw.len()]).unwrap_or("");

    match kind {
        Kind::Int8 | Kind::Int16 | Kind::Int32 | Kind::Int64 => {
            let v: i64 = match s.parse() {
                Ok(v) => v,
                Err(_) => return bs.fail(ERR_BIND_NUMBER_RANGE, "number out of range"),
            };
            match kind {
                Kind::Int8 => {
                    if v < i8::MIN as i64 || v > i8::MAX as i64 {
                        return bs.fail(ERR_BIND_NUMBER_RANGE, "int8 overflow");
                    }
                    *(dst as *mut i8) = v as i8;
                }
                Kind::Int16 => {
                    if v < i16::MIN as i64 || v > i16::MAX as i64 {
                        return bs.fail(ERR_BIND_NUMBER_RANGE, "int16 overflow");
                    }
                    *(dst as *mut i16) = v as i16;
                }
                Kind::Int32 => {
                    if v < i32::MIN as i64 || v > i32::MAX as i64 {
                        return bs.fail(ERR_BIND_NUMBER_RANGE, "int32 overflow");
                    }
                    *(dst as *mut i32) = v as i32;
                }
                Kind::Int64 => {
                    *(dst as *mut i64) = v;
                }
                _ => unreachable!(),
            }
        }
        Kind::Uint8 | Kind::Uint16 | Kind::Uint32 | Kind::Uint64 => {
            if s.starts_with('-') {
                return bs.fail(ERR_BIND_NUMBER_RANGE, "negative value for unsigned");
            }
            let v: u64 = match s.parse() {
                Ok(v) => v,
                Err(_) => return bs.fail(ERR_BIND_NUMBER_RANGE, "number out of range"),
            };
            match kind {
                Kind::Uint8 => {
                    if v > u8::MAX as u64 {
                        return bs.fail(ERR_BIND_NUMBER_RANGE, "uint8 overflow");
                    }
                    *(dst as *mut u8) = v as u8;
                }
                Kind::Uint16 => {
                    if v > u16::MAX as u64 {
                        return bs.fail(ERR_BIND_NUMBER_RANGE, "uint16 overflow");
                    }
                    *(dst as *mut u16) = v as u16;
                }
                Kind::Uint32 => {
                    if v > u32::MAX as u64 {
                        return bs.fail(ERR_BIND_NUMBER_RANGE, "uint32 overflow");
                    }
                    *(dst as *mut u32) = v as u32;
                }
                Kind::Uint64 => {
                    *(dst as *mut u64) = v;
                }
                _ => unreachable!(),
            }
        }
        Kind::Float32 | Kind::Float64 => {
            let v: f64 = match s.parse() {
                Ok(v) => v,
                Err(_) => return bs.fail(ERR_BIND_NUMBER_RANGE, "float parse error"),
            };
            match kind {
                Kind::Float32 => {
                    *(dst as *mut f32) = v as f32;
                }
                Kind::Float64 => {
                    *(dst as *mut f64) = v;
                }
                _ => unreachable!(),
            }
        }
        _ => return bs.fail(ERR_BIND_TYPE_MISMATCH, "expected number kind"),
    }
    0
}

// ---------------------------------------------------------------------------
// Reactor implementation
// ---------------------------------------------------------------------------

impl Reactor for BindState {
    fn begin_object(&mut self) -> i32 {
        let slot = match self.slot_for_value() {
            Ok(s) => s,
            Err(e) => return e,
        };

        let (obj, type_info) = match slot.kind {
            Kind::Struct => {
                let ti = match slot.type_info {
                    Some(t) => t,
                    None => return self.fail(ERR_BIND_TYPE_MISMATCH, "missing struct type"),
                };
                unsafe {
                    std::ptr::write_bytes(slot.dst, 0, ti.size);
                }
                (slot.dst, ti)
            }
            Kind::StructPtr => {
                let ti = match slot.type_info {
                    Some(t) => t,
                    None => return self.fail(ERR_BIND_TYPE_MISMATCH, "missing struct type"),
                };
                let allocated = match self.arena().alloc(ti.size) {
                    Some(p) => p,
                    None => return self.fail(ERR_BIND_OOM, "arena OOM"),
                };
                unsafe {
                    std::ptr::write_bytes(allocated, 0, ti.size);
                    *(slot.dst as *mut *mut u8) = allocated;
                }
                (allocated, ti)
            }
            _ => return self.fail(ERR_BIND_TYPE_MISMATCH, "expected object"),
        };

        self.advance();
        self.stack.push(BindFrame::new_struct(obj, Some(type_info)));
        PROCEED
    }

    fn end_object(&mut self) -> i32 {
        if self.top().kind != FrameKind::Struct {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "unexpected end_object");
        }
        self.stack.pop();
        PROCEED
    }

    fn object_field(&mut self, key: StrInfo<'_>) -> i32 {
        let top = self.stack.last_mut().unwrap();
        if top.kind != FrameKind::Struct {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "field outside struct");
        }

        let key_bytes = unsafe { std::slice::from_raw_parts(key.raw.ptr, key.raw.len as usize) };

        if let Some(type_info) = top.type_info {
            for field in type_info.fields {
                if field.name.as_bytes() == key_bytes {
                    top.pending = Some(field);
                    return PROCEED;
                }
            }
        }

        if self.strict {
            return self.fail(ERR_BIND_UNKNOWN_FIELD, "unknown field");
        }
        SKIP
    }

    fn begin_array(&mut self) -> i32 {
        let top = self.stack.last_mut().unwrap();
        if top.kind != FrameKind::Struct {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "unexpected array");
        }
        let field = match top.pending {
            Some(f) => f,
            None => return self.fail(ERR_BIND_TYPE_MISMATCH, "unexpected array (no pending)"),
        };
        if field.kind != Kind::Array {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "expected array");
        }
        let elem_kind = match field.elem_kind {
            Some(k) => k,
            None => return self.fail(ERR_BIND_TYPE_MISMATCH, "no elem_kind"),
        };
        let elem_size = match elem_kind.elem_size(field.elem_type) {
            Some(s) => s,
            None => return self.fail(ERR_BIND_TYPE_MISMATCH, "unsupported array element kind"),
        };

        let parent_obj = top.obj;
        let dst_ptr_addr = unsafe { parent_obj.add(field.offset) };
        let dst_len_addr = unsafe { parent_obj.add(field.len_offset) as *mut usize };
        top.pending = None;

        self.stack.push(BindFrame::new_array(
            elem_kind,
            field.elem_type,
            elem_size,
            dst_ptr_addr,
            dst_len_addr,
        ));
        PROCEED
    }

    fn end_array(&mut self) -> i32 {
        let top = self.stack.last_mut().unwrap();
        if top.kind != FrameKind::Array {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "unexpected end_array");
        }

        let count = top.count;
        let elem_size = top.elem_size;
        let total = count * elem_size;
        let dst_ptr_addr = top.dst_ptr_addr;
        let dst_len_addr = top.dst_len_addr;

        let items = if total > 0 {
            let p = match self.arena().alloc(total) {
                Some(p) => p,
                None => return self.fail(ERR_BIND_OOM, "arena OOM"),
            };
            let top = self.stack.last().unwrap();
            unsafe {
                std::ptr::copy_nonoverlapping(top.scratch.as_ptr(), p, total);
            }
            p
        } else {
            std::ptr::null_mut()
        };

        unsafe {
            *(dst_ptr_addr as *mut *mut u8) = items;
            *dst_len_addr = count;
        }

        self.stack.pop();
        PROCEED
    }

    fn array_elem(&mut self) -> i32 {
        PROCEED
    }

    fn scalar_null(&mut self) -> i32 {
        let slot = match self.slot_for_value() {
            Ok(s) => s,
            Err(e) => return e,
        };
        match slot.kind {
            Kind::String | Kind::StructPtr => unsafe {
                *(slot.dst as *mut *mut u8) = std::ptr::null_mut();
            },
            Kind::Struct => {
                if let Some(ti) = slot.type_info {
                    unsafe {
                        std::ptr::write_bytes(slot.dst, 0, ti.size);
                    }
                }
            }
            _ => {
                if let Some(sz) = slot.kind.elem_size(slot.type_info) {
                    if sz > 0 {
                        unsafe {
                            std::ptr::write_bytes(slot.dst, 0, sz);
                        }
                    }
                }
            }
        }
        self.advance();
        PROCEED
    }

    fn scalar_bool(&mut self, value: bool) -> i32 {
        let slot = match self.slot_for_value() {
            Ok(s) => s,
            Err(e) => return e,
        };
        if slot.kind != Kind::Bool {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "expected bool");
        }
        unsafe {
            *(slot.dst as *mut bool) = value;
        }
        self.advance();
        PROCEED
    }

    fn scalar_number(&mut self, raw: RawStr<'_>) -> i32 {
        let slot = match self.slot_for_value() {
            Ok(s) => s,
            Err(e) => return e,
        };
        let bytes = unsafe { std::slice::from_raw_parts(raw.ptr, raw.len as usize) };
        let rc = unsafe { write_number(self, slot.dst, slot.kind, bytes) };
        if rc < 0 {
            return -2;
        }
        self.advance();
        PROCEED
    }

    fn scalar_string(&mut self, s: StrInfo<'_>) -> i32 {
        let slot = match self.slot_for_value() {
            Ok(s) => s,
            Err(e) => return e,
        };
        let bytes = unsafe { std::slice::from_raw_parts(s.raw.ptr, s.raw.len as usize) };

        if slot.kind == Kind::String {
            let p = match self.arena().memdup_z(bytes) {
                Some(p) => p,
                None => return self.fail(ERR_BIND_OOM, "arena OOM"),
            };
            unsafe {
                *(slot.dst as *mut *mut u8) = p;
            }
        } else {
            return self.fail(ERR_BIND_TYPE_MISMATCH, "expected string kind");
        }
        self.advance();
        PROCEED
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Unmarshal JSON data into `out` according to the schema in `type_info`.
///
/// Returns `Ok(())` on success, or `Err(code)` on failure.
pub fn unmarshal(
    type_info: &'static TypeInfo,
    out: *mut u8,
    data: &[u8],
    arena: &mut Arena,
) -> Result<(), i32> {
    unmarshal_ex(type_info, out, data, arena, None, None)
}

/// Extended unmarshal with options and error detail.
pub fn unmarshal_ex(
    type_info: &'static TypeInfo,
    out: *mut u8,
    data: &[u8],
    arena: &mut Arena,
    opts: Option<&UnmarshalOpts>,
    err: Option<&mut UnmarshalError>,
) -> Result<(), i32> {
    if let Some(e) = err.as_ref() {
        let _ = e; // just checking it exists
    }

    // Bootstrap: synthetic root frame.
    let root_field = Field {
        name: "",
        kind: Kind::Struct,
        offset: 0,
        len_offset: 0,
        elem_kind: None,
        elem_type: Some(type_info),
    };

    // Leak the root_field so we can get a 'static reference.
    // This is safe because unmarshal is synchronous and root_field lives
    // on the stack for the duration.
    let root_field_ref: &'static Field = unsafe { &*(&root_field as *const Field) };

    let mut bs = BindState {
        stack: vec![{
            let mut f = BindFrame::new_struct(out, None);
            f.pending = Some(root_field_ref);
            f
        }],
        arena: arena as *mut Arena,
        strict: opts.map_or(false, |o| o.strict),
        err_code: 0,
        err_message: "",
    };

    let mut ctx = Ctx::new();
    ctx.set_input_slice(data, true);

    unsafe { kernel::parse(&mut ctx, &mut bs) };

    // Finalize.
    let kernel_exit = ctx.exit_code;
    let kernel_pos = ctx.error_pos;

    if kernel_exit == ExitCode::Ok as i32 && bs.err_code == 0 {
        if let Some(e) = err {
            e.code = 0;
            e.pos = 0;
            e.message = "";
        }
        return Ok(());
    }

    let code = if bs.err_code != 0 {
        bs.err_code
    } else {
        kernel_exit
    };
    if let Some(e) = err {
        e.code = code;
        e.pos = kernel_pos as usize;
        e.message = bs.err_message;
    }
    Err(code)
}
