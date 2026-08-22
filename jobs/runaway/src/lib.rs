//! A job that loops forever.
//!
//! Exists so the CPU ceiling can be demonstrated against something that
//! genuinely tries to run away, rather than inferred from a job that happens to
//! be slow. A volunteer's machine has to survive this: hostile or merely buggy,
//! the outcome must be the same.

#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let mut buf: Vec<u8> = Vec::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr as u32
}

/// Never returns. The host must stop it.
#[no_mangle]
pub extern "C" fn tick(_ptr: u32, _len: u32) -> u64 {
    let mut spin: u64 = 0;
    loop {
        // read_volatile keeps the optimiser from noticing this loop has no
        // effect and deleting it, which would turn the test into a no-op.
        spin = unsafe { core::ptr::read_volatile(&spin) }.wrapping_add(1);
    }
}
