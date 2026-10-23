//! An FFmpeg `AVIOContext` that reads from a [`ByteSource`].

use crate::{Error, Result};
use fp_core::ByteSource;
use fp_ffmpeg_sys as ff;
use std::ffi::{c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const BUFFER_SIZE: usize = 256 * 1024;
const SEEK_SET: c_int = 0;
const SEEK_CUR: c_int = 1;
const SEEK_END: c_int = 2;

struct State {
    src: Arc<dyn ByteSource>,
    pos: u64,
    interrupt: Arc<AtomicBool>,
}

/// Owns an `AVIOContext` and the Rust state behind it.
pub struct IoContext {
    ctx: *mut ff::AVIOContext,
    state: *mut State,
}

// SAFETY: the context is only used by one thread at a time (the demuxer that
// owns it); the ByteSource behind it is Send + Sync.
unsafe impl Send for IoContext {}

impl IoContext {
    pub fn new(src: Arc<dyn ByteSource>, interrupt: Arc<AtomicBool>) -> Result<IoContext> {
        let state = Box::into_raw(Box::new(State {
            src,
            pos: 0,
            interrupt,
        }));
        // SAFETY: buffer ownership passes to the AVIOContext, freed in Drop.
        unsafe {
            let buf = ff::av_malloc(BUFFER_SIZE) as *mut u8;
            if buf.is_null() {
                drop(Box::from_raw(state));
                return Err(Error::Unsupported("out of memory".into()));
            }
            let ctx = ff::avio_alloc_context(
                buf,
                BUFFER_SIZE as c_int,
                0,
                state as *mut c_void,
                Some(read_cb),
                None,
                Some(seek_cb),
            );
            if ctx.is_null() {
                ff::av_free(buf as *mut c_void);
                drop(Box::from_raw(state));
                return Err(Error::Unsupported("avio_alloc_context failed".into()));
            }
            Ok(IoContext { ctx, state })
        }
    }

    pub fn as_ptr(&self) -> *mut ff::AVIOContext {
        self.ctx
    }

    /// Hands the context to FFmpeg (a demuxer's `io_open`); it comes back
    /// to [`IoContext::free_raw`] through `io_close2`.
    pub(crate) fn into_raw(self) -> *mut ff::AVIOContext {
        let ctx = self.ctx;
        std::mem::forget(self);
        ctx
    }

    /// Whether `ctx` was made here (and not by FFmpeg).
    ///
    /// # Safety
    /// `ctx` must be a valid `AVIOContext`.
    pub(crate) unsafe fn is_ours(ctx: *mut ff::AVIOContext) -> bool {
        // SAFETY: valid per the contract.
        unsafe { (*ctx).read_packet }.is_some_and(|f| {
            std::ptr::fn_addr_eq(
                f,
                read_cb as unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int,
            )
        })
    }

    /// Frees a context from [`IoContext::into_raw`].
    ///
    /// # Safety
    /// `ctx` must come from `into_raw` and not be used afterwards.
    pub(crate) unsafe fn free_raw(ctx: *mut ff::AVIOContext) {
        // SAFETY: into_raw's context, whose opaque is our State.
        let state = unsafe { (*ctx).opaque } as *mut State;
        drop(IoContext { ctx, state });
    }
}

impl Drop for IoContext {
    fn drop(&mut self) {
        // SAFETY: ctx and state were created in new() and are freed once.
        unsafe {
            if !self.ctx.is_null() {
                ff::av_freep(&mut (*self.ctx).buffer as *mut *mut u8 as *mut c_void);
                ff::avio_context_free(&mut self.ctx);
            }
            drop(Box::from_raw(self.state));
        }
    }
}

unsafe extern "C" fn read_cb(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    // SAFETY: opaque is the State pointer passed to avio_alloc_context.
    let st = unsafe { &mut *(opaque as *mut State) };
    if st.interrupt.load(Ordering::Relaxed) {
        return ff::AVERROR_EXIT_;
    }
    // SAFETY: FFmpeg gives us a writable buffer of `size` bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size.max(0) as usize) };
    match st.src.read_at(st.pos, out) {
        Ok(0) => ff::AVERROR_EOF_,
        Ok(n) => {
            st.pos += n as u64;
            n as c_int
        }
        Err(e) => {
            log::warn!("read {} at {}: {e}", st.src.describe(), st.pos);
            ff::averror(5) // EIO
        }
    }
}

unsafe extern "C" fn seek_cb(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    // SAFETY: opaque is our State pointer.
    let st = unsafe { &mut *(opaque as *mut State) };
    let size = st.src.size();
    if whence & ff::AVSEEK_SIZE as c_int != 0 {
        return size.map(|s| s as i64).unwrap_or(-1);
    }
    let base = match whence & !(ff::AVSEEK_FORCE as c_int) {
        SEEK_SET => 0i64,
        SEEK_CUR => st.pos as i64,
        SEEK_END => match size {
            Some(s) => s as i64,
            None => return -1,
        },
        _ => return -1,
    };
    let target = base.saturating_add(offset);
    if target < 0 {
        return -1;
    }
    st.pos = target as u64;
    target
}
