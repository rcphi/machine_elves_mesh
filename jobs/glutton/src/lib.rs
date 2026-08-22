//! A job that tries to eat all the memory it can.
//!
//! Fuel limits instructions and says nothing about memory, so a job can obey
//! its CPU ceiling perfectly while exhausting the host's RAM. This exists so
//! that the second ceiling is demonstrated against something genuinely greedy
//! rather than assumed to work.

#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let mut buf: Vec<u8> = Vec::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr as u32
}

#[no_mangle]
pub extern "C" fn tick(_ptr: u32, _len: u32) -> u64 {
    // Grow in large steps: the aim is to cross the ceiling quickly, well before
    // the fuel runs out, so that the two limits are told apart.
    let mut held: Vec<Vec<u8>> = Vec::new();
    loop {
        let mut chunk = vec![0u8; 4 * 1024 * 1024];
        // Touch it, or the allocation may never become real pages.
        chunk[0] = 1;
        held.push(chunk);
    }
}
