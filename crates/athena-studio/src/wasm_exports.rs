//! The wasm module's export face (X-30, spec §25): the render faces the §25 views
//! and the live board produce, exposed to the browser glue over the C ABI.
//!
//! No wasm-bindgen is used: the module stays on the workspace's dependency face
//! (`serde` only — §32.2), and the glue owns every DOM / browser concern
//! (`ATH-STUDIO-002`: the data rides the Core API only; the module renders what the
//! API answers). One export per screen face plus the stream reduction and the memory
//! pair; each answers a string whose length the glue reads from
//! [`ATHENA_STUDIO_LAST_LEN`].
//!
//! The memory contract, as the live round trip verified it (node, `athena-studio.wasm`):
//!
//! - **Inputs** ride blocks the host allocates through [`athena_studio_alloc`] and
//!   releases through [`athena_studio_free`] once its copy is written. Both faces use
//!   the module's own runtime allocator (`alloc::alloc` / `alloc::dealloc` at align 1)
//!   — the pair the round trip observed green.
//! - **Answers** live in the module's one retained buffer, allocated once at
//!   [`ANSWER_BYTES`] and never released: every export renders into it, publishes
//!   [`ATHENA_STUDIO_LAST_LEN`], and the face stays valid until the *next* export
//!   answers. The glue copies before its next call; an answer offset is never passed to
//!   [`athena_studio_free`] (the live run observed a render block's *dealloc* aborting
//!   the module, so the answer face does not expose that path at all). One buffer for
//!   the module's whole life keeps the release path exactly what the round trip
//!   verified, and a browser tab's main thread gives the module the single-caller
//!   contract this requires.
//!
//! Secrets / chain-of-thought never cross this face (`ATH-STUDIO-005`,
//! `ATH-STUDIO-007`): the rows the renders consume carry names, stamps and the
//! canonical descriptor only, and every untrusted prose line is escaped at
//! [`crate::views::prost_line`] before it reaches the buffer.

use std::alloc::Layout;
use std::ptr;

/// The answer buffer's fixed alignment (byte-aligned: every answer is a UTF-8 string).
const ANSWER_ALIGN: usize = 1;

/// The retained answer buffer's size. Every §25 screen's render fits comfortably (the
/// largest observed face was ~1 KB per Task row); an answer beyond it is truncated to
/// [`ATHENA_STUDIO_LAST_LEN`]'s honest smaller value rather than written past the block.
const ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// Allocates `len` bytes for the host to write input into; returns the offset.
///
/// # Panics
/// Never for the sizes a screen's payload reaches.
#[unsafe(no_mangle)]
pub extern "C" fn athena_studio_alloc(len: usize) -> *mut u8 {
    let Ok(layout) = Layout::from_size_align(len, ANSWER_ALIGN) else {
        return ptr::null_mut();
    };
    unsafe { std::alloc::alloc(layout) }
}

/// Releases one *input* buffer [`athena_studio_alloc`] produced (the glue is done
/// writing its copy). An answer offset is never released here — answers live in the
/// module's retained buffer and stay valid until the next export answers; passing one
/// leaves the buffer exactly where it is.
///
/// # Safety
/// `ptr` must be a buffer [`athena_studio_alloc`] produced and not yet released, and
/// `len` its allocation length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 || ptr == answer_ptr() {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len, ANSWER_ALIGN) else {
        return;
    };
    unsafe { std::alloc::dealloc(ptr, layout) };
}

/// The last export's answer length, in bytes (0 until the first export answered). The
/// glue reads it and copies that many bytes from the offset the export returned.
///
/// This is a *function*, not an exported static: a `#[no_mangle] pub static mut` leaves
/// the wasm backend as an **immutable** `WebAssembly.Global`, so a JS reader would keep
/// seeing the initial value no matter how often an export updates it (observed live in
/// node). A call always reflects the current state, so the length rides the ABI the
/// module can actually publish per call.
#[unsafe(no_mangle)]
pub extern "C" fn athena_studio_last_len() -> usize {
    ANSWER_LEN.lock().map(|guard| *guard).unwrap_or(0)
}

/// Parses one streamed `data:` chunk into the Event-row JSON array the glue renders
/// (the LiveBoard's reduction face, [`crate::client::parse_stream_chunk`]).
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_parse_chunk(ptr: *const u8, len: usize) -> *const u8 {
    unsafe {
        let chunk = read(ptr, len);
        let rows: Vec<serde_json::Value> = crate::client::parse_stream_chunk(&chunk)
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "event_type": row.event_type,
                    "task_id": row.task_id,
                    "occurred_at": row.occurred_at,
                })
            })
            .collect();
        answer(&serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()))
    }
}

/// Renders the Dashboard from the Tasks API's rows (the `GET /api/v1/tasks` answer the
/// glue fetched, through [`crate::views::dashboard`]).
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_render_dashboard(ptr: *const u8, len: usize) -> *const u8 {
    unsafe {
        let body = read(ptr, len);
        let tasks: Vec<crate::client::TaskRow> = serde_json::from_str(&body).unwrap_or_default();
        answer(&crate::views::dashboard(&tasks))
    }
}

/// Renders the Task detail face (the `GET /api/v1/tasks/{id}` answer through
/// [`crate::views::task_view`]).
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_render_task(ptr: *const u8, len: usize) -> *const u8 {
    unsafe {
        let body = read(ptr, len);
        match serde_json::from_str::<crate::client::TaskRow>(&body) {
            Ok(task) => answer(&crate::views::task_view(&task)),
            Err(_) => answer(&detail_absent()),
        }
    }
}

/// Renders the approvals screen plus its actionable plans
/// ([`crate::actions::approval_screen`]).
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_render_approvals(ptr: *const u8, len: usize) -> *const u8 {
    unsafe {
        let body = read(ptr, len);
        let rows: Vec<crate::client::ApprovalRow> = serde_json::from_str(&body).unwrap_or_default();
        answer(&crate::actions::approval_screen(&rows))
    }
}

/// Applies one streamed chunk to the Task rows the glue holds and answers the
/// re-rendered live board ([`crate::live::LiveBoard`], [`crate::live::render`]) — the
/// module is stateless by design (renders in, render out), so the board state stays the
/// glue's.
///
/// The glue sends `{"tasks": [...], "chunk": "data: ..."}`.
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_render_live(ptr: *const u8, len: usize) -> *const u8 {
    unsafe {
        let body = read(ptr, len);
        let Ok(face) = serde_json::from_str::<serde_json::Value>(&body) else {
            return answer(&absent_board());
        };
        let tasks = face
            .get("tasks")
            .cloned()
            .map(serde_json::from_value::<Vec<crate::client::TaskRow>>)
            .map(Result::unwrap_or_default)
            .unwrap_or_default();
        let chunk = face
            .get("chunk")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let mut board = crate::live::LiveBoard::from_tasks(tasks);
        board.apply_chunk(chunk);
        answer(&crate::live::render(&board))
    }
}

/// Renders the event-history section (the names and stamps only).
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn athena_studio_render_events(ptr: *const u8, len: usize) -> *const u8 {
    unsafe {
        let body = read(ptr, len);
        let rows: Vec<crate::client::EventRow> = serde_json::from_str(&body).unwrap_or_default();
        answer(&crate::views::event_view(&rows))
    }
}

/// Reads the caller's buffer into an owned String (lossy: a non-UTF-8 body renders as
/// its replacement face, never as a fault the glue cannot read).
///
/// # Safety
/// The caller must pass a readable buffer.
unsafe fn read(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// The retained answer buffer, built once on first use (the module's linear memory keeps
/// it for the module's whole life; no export releases it).
static ANSWER: std::sync::Mutex<Option<usize>> = std::sync::Mutex::new(None);

/// The retained buffer's offset, allocating [`ANSWER_BYTES`] on first use.
fn answer_ptr() -> *mut u8 {
    let Ok(mut guard) = ANSWER.lock() else {
        return ptr::null_mut();
    };
    if guard.is_none() {
        let Ok(layout) = Layout::from_size_align(ANSWER_BYTES, ANSWER_ALIGN) else {
            return ptr::null_mut();
        };
        *guard = Some(unsafe { std::alloc::alloc(layout) } as usize);
    }
    guard.unwrap() as *mut u8
}

/// The last answer's length (the non-exported fact `athena_studio_last_len` serves).
static ANSWER_LEN: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

/// Writes one answer into the retained buffer, records its length and hands the offset
/// back (valid until the next export answers; never released, never freed).
fn answer(json: &str) -> *const u8 {
    let base = answer_ptr();
    if base.is_null() {
        return ptr::null();
    }
    let len = json.len().min(ANSWER_BYTES);
    unsafe {
        ptr::copy_nonoverlapping(json.as_ptr().cast(), base, len);
    }
    if let Ok(mut guard) = ANSWER_LEN.lock() {
        *guard = len;
    }
    base
}

/// A detail face the payload could not shape (the glue renders the empty face; the
/// row's absence is never invented into a fabricated Task).
fn detail_absent() -> String {
    "<section class=\"absent\"><p>the answer could not be shaped</p></section>\n".into()
}

/// The live board's absent face (a malformed input is an empty board, never an
/// invented row).
fn absent_board() -> String {
    crate::live::render(&crate::live::LiveBoard::default())
}
