//! Validation of pointers that user code hands to the kernel. Pure, host-testable.

/// True when `[ptr, ptr + len)` lies entirely inside the user window `[start, end)`. A zero length is
/// fine as long as the pointer is inside the window or exactly at its end.
pub fn check(start: u64, end: u64, ptr: u64, len: u64) -> bool {
    match ptr.checked_add(len) {
        Some(stop) => ptr >= start && stop <= end,
        None => false,
    }
}

/// True when `[ptr, ptr + len)` lies inside the user writable memory range `[start + code_len, end)`.
/// Protects read-only user code pages from being overwritten by kernel writes (e.g. `read` or `copy_to_user`).
pub fn check_write(start: u64, end: u64, ptr: u64, len: u64, code_len: u64) -> bool {
    let write_start = match start.checked_add(code_len) {
        Some(s) => s,
        None => return false,
    };
    check(write_start, end, ptr, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 0x4000_0000;
    const E: u64 = 0x4001_0000;
    const CODE_SZ: u64 = 4096;

    #[test]
    fn rejects_overflow() {
        assert!(!check(S, E, u64::MAX - 1, 8));
        assert!(!check(S, E, S, u64::MAX));
    }

    #[test]
    fn rejects_kernel_range() {
        assert!(!check(S, E, 0, 1));
        assert!(!check(S, E, 0x10_0000, 16));
        assert!(!check(S, E, 0xFFFF_8000_0000_0000, 1));
        assert!(!check(S, E, S - 1, 1));
    }

    #[test]
    fn rejects_straddle() {
        assert!(!check(S, E, S - 4, 8));
        assert!(!check(S, E, E - 4, 8));
    }

    #[test]
    fn accepts_zero_len_inside() {
        assert!(check(S, E, S, 0));
        assert!(check(S, E, E, 0));
        assert!(!check(S, E, E + 1, 0));
        assert!(!check(S, E, 0, 0));
    }

    #[test]
    fn accepts_exact_window() {
        assert!(check(S, E, S, E - S));
        assert!(!check(S, E, S, E - S + 1));
    }

    #[test]
    fn empty_window_accepts_nothing() {
        assert!(!check(0, 0, 1, 1));
    }

    #[test]
    fn check_write_protects_code_page() {
        // Pointer inside code page must be rejected for writing
        assert!(!check_write(S, E, S, 16, CODE_SZ));
        assert!(!check_write(S, E, S + CODE_SZ - 4, 8, CODE_SZ));
        // Pointer right at write_start must be accepted
        assert!(check_write(S, E, S + CODE_SZ, 16, CODE_SZ));
        // Pointer in data window must be accepted
        assert!(check_write(S, E, S + CODE_SZ + 1024, 64, CODE_SZ));
        // Pointer exceeding end must be rejected
        assert!(!check_write(S, E, E - 8, 16, CODE_SZ));
    }
}
