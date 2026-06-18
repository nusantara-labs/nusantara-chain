pub const LAMPORTS_PER_NUSA: u64 = const_parse_u64(env!("NUSA_TOKEN_LAMPORTS_PER_NUSA"));

pub fn lamports_to_nusa(lamports: u64) -> f64 {
    lamports as f64 / LAMPORTS_PER_NUSA as f64
}

/// Convert a NUSA amount expressed as f64 to lamports.
///
/// This is a convenience/display function only.  f64 has 53 bits of mantissa,
/// so values above 2^53 NUSA (~9 quadrillion) lose precision.  NaN, infinite,
/// and negative inputs return 0.
pub fn nusa_to_lamports(nusa: f64) -> u64 {
    if !nusa.is_finite() || nusa < 0.0 {
        return 0;
    }
    (nusa * LAMPORTS_PER_NUSA as f64) as u64
}

pub const fn const_parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut result: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(b >= b'0' && b <= b'9', "const_parse_u64: non-digit character in input");
        result = result * 10 + (b - b'0') as u64;
        i += 1;
    }
    result
}

pub const fn const_parse_u8(s: &str) -> u8 {
    const_parse_u64(s) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamports_per_nusa_is_one_billion() {
        assert_eq!(LAMPORTS_PER_NUSA, 1_000_000_000);
    }

    #[test]
    fn conversion_roundtrip() {
        let nusa = 1.5;
        let lamports = nusa_to_lamports(nusa);
        assert_eq!(lamports, 1_500_000_000);
        let back = lamports_to_nusa(lamports);
        assert!((back - nusa).abs() < f64::EPSILON);
    }
}
