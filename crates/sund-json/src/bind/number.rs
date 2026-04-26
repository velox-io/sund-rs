//! Fast JSON number parser (Clinger's fast path).

/// Precomputed 10^0..10^22 (exact in binary64).
static POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// Integer powers of 10 for the extended fast path.
static IPOW10: [u64; 16] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
];

fn parse_digit(c: u8, acc: &mut u64) -> bool {
    let d = c.wrapping_sub(b'0');
    if d > 9 {
        return false;
    }
    *acc = acc.wrapping_mul(10).wrapping_add(d as u64);
    true
}

/// Slow path: parse via `str::parse::<f64>()`.
fn parse_double_fallback(src: &[u8]) -> f64 {
    // NUL-terminate in a small buffer.
    let mut buf = [0u8; 64];
    let n = src.len().min(63);
    buf[..n].copy_from_slice(&src[..n]);
    let s = std::str::from_utf8(&buf[..n]).unwrap_or("0");
    s.parse::<f64>().unwrap_or(0.0)
}

/// Parse a validated JSON number span `src` into a `f64`.
///
/// Uses Clinger's fast path for mantissas with ≤19 significant digits and
/// exponents in [-22, 22]. Falls back to `str::parse` for edge cases.
///
/// Returns `Ok(value)` on success, `Err(())` if parsing failed entirely.
pub fn parse_double(src: &[u8]) -> Result<f64, ()> {
    if src.is_empty() {
        return Err(());
    }

    let mut p = 0usize;
    let end = src.len();

    let neg = if src[p] == b'-' {
        p += 1;
        true
    } else {
        false
    };

    // Integer part.
    let mut i: u64 = 0;
    let int_start = p;
    while p < end && parse_digit(src[p], &mut i) {
        p += 1;
    }
    let int_digits = p - int_start;
    if int_digits == 0 {
        return Err(());
    }

    // Fractional part.
    let mut exponent: i64 = 0;
    let mut frac_digits: u32 = 0;
    if p < end && src[p] == b'.' {
        p += 1;
        let frac_start = p;
        while p < end && parse_digit(src[p], &mut i) {
            p += 1;
        }
        frac_digits = (p - frac_start) as u32;
        if frac_digits == 0 {
            return Err(());
        }
        exponent = -(frac_digits as i64);
    }

    let total_digits = int_digits as u32 + frac_digits;

    // Exponent part.
    if p < end && (src[p] == b'e' || src[p] == b'E') {
        p += 1;
        let exp_neg = if p < end && src[p] == b'-' {
            p += 1;
            true
        } else if p < end && src[p] == b'+' {
            p += 1;
            false
        } else {
            false
        };
        let mut exp: u64 = 0;
        let exp_start = p;
        while p < end && parse_digit(src[p], &mut exp) {
            p += 1;
        }
        if p == exp_start {
            return Err(());
        }
        if exp_neg {
            exponent -= exp as i64;
        } else {
            exponent += exp as i64;
        }
    }

    // Clinger fast path.
    if total_digits <= 19 && i < (1u64 << 53) {
        if exponent >= -22 && exponent <= 22 {
            let mut d = i as f64;
            if exponent >= 0 {
                d *= POW10[exponent as usize];
            } else {
                d /= POW10[(-exponent) as usize];
            }
            if neg {
                d = -d;
            }
            return Ok(d);
        }

        // Extended fast path: 22 < exp <= 37.
        if exponent > 22 && exponent <= 37 {
            let extra = (exponent - 22) as usize;
            let shifted = i.checked_mul(IPOW10[extra]);
            if let Some(shifted) = shifted {
                if shifted < (1u64 << 53) && shifted / IPOW10[extra] == i {
                    let mut d = shifted as f64;
                    d *= POW10[22];
                    if neg {
                        d = -d;
                    }
                    return Ok(d);
                }
            }
        }
    }

    let mut d = parse_double_fallback(src);
    if neg && d > 0.0 {
        d = -d;
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integers() {
        assert_eq!(parse_double(b"0"), Ok(0.0));
        assert_eq!(parse_double(b"42"), Ok(42.0));
        assert_eq!(parse_double(b"-7"), Ok(-7.0));
    }

    #[test]
    fn test_fractions() {
        assert_eq!(parse_double(b"3.14"), Ok(3.14));
        assert_eq!(parse_double(b"-0.5"), Ok(-0.5));
    }

    #[test]
    fn test_exponents() {
        assert_eq!(parse_double(b"1e10"), Ok(1e10));
        assert_eq!(parse_double(b"2.5e-3"), Ok(2.5e-3));
    }

    #[test]
    fn test_empty() {
        assert!(parse_double(b"").is_err());
    }
}
