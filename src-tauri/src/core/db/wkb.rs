//! PostGIS hex EWKB (the text output of `geometry` and `geography`) → WKT for the grid, as
//! `ST_AsText` prints it. The SRID is not shown; saving an edit keeps the column's SRID.
//!
//! The input is checked in a cheap first pass that reads only headers and counts and skips the
//! coordinates, so a value that is not EWKB costs almost nothing and is shown as plain text.
//! The second pass writes WKT and stops at the preview cap, so a polygon of megabytes formats
//! only the part that the grid shows.

use std::fmt::Write as _;

const MAX_DEPTH: u32 = 32;

const Z_FLAG: u32 = 0x8000_0000;
const M_FLAG: u32 = 0x4000_0000;
const SRID_FLAG: u32 = 0x2000_0000;

/// `POINT(1 2)` for PostGIS hex EWKB, `None` when `hex` is not valid EWKB.
/// With a cap, the text is cut after `cap` bytes and ends with `…`.
pub fn hex_ewkb_to_wkt(hex: &str, cap: Option<usize>) -> Option<String> {
    let h = hex.as_bytes();
    // Byte order (1) + type (4) is the smallest header.
    if h.len() < 10
        || !h.len().is_multiple_of(2)
        || !(h.starts_with(b"00") || h.starts_with(b"01"))
        || !h.iter().all(u8::is_ascii_hexdigit)
    {
        return None;
    }
    let mut check = Reader { hex: h, pos: 0 };
    geometry(&mut check, None, true, 0).ok()?;
    if check.pos != h.len() / 2 {
        return None;
    }
    let mut out = Out {
        s: String::new(),
        cap,
        after_dims: false,
    };
    match geometry(&mut Reader { hex: h, pos: 0 }, Some(&mut out), true, 0) {
        Ok(()) => Some(out.s),
        Err(Stop::Cap) => {
            // WKT is ASCII, so every index is a char boundary.
            out.s.truncate(cap.unwrap_or(out.s.len()));
            out.s.push('…');
            Some(out.s)
        }
        Err(Stop::Invalid) => None,
    }
}

enum Stop {
    Invalid,
    Cap,
}

struct Out {
    s: String,
    cap: Option<usize>,
    /// A ` Z`/` M`/` ZM` tag was just written: an opening `(` needs a space (`POINT Z (1 2 3)`).
    after_dims: bool,
}

impl Out {
    fn push(&mut self, s: &str) -> Result<(), Stop> {
        if std::mem::take(&mut self.after_dims) && s.starts_with('(') {
            self.s.push(' ');
        }
        self.s.push_str(s);
        self.check()
    }

    fn num(&mut self, f: f64) -> Result<(), Stop> {
        let _ = write!(self.s, "{f}");
        self.check()
    }

    fn check(&self) -> Result<(), Stop> {
        match self.cap {
            Some(c) if self.s.len() > c => Err(Stop::Cap),
            _ => Ok(()),
        }
    }
}

struct Reader<'a> {
    hex: &'a [u8],
    /// Byte offset (half the hex offset).
    pos: usize,
}

impl Reader<'_> {
    fn len(&self) -> usize {
        self.hex.len() / 2
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Stop> {
        if self.len() - self.pos < N {
            return Err(Stop::Invalid);
        }
        let mut b = [0u8; N];
        for (i, out) in b.iter_mut().enumerate() {
            let at = (self.pos + i) * 2;
            *out = nibble(self.hex[at]) << 4 | nibble(self.hex[at + 1]);
        }
        self.pos += N;
        Ok(b)
    }

    fn u32(&mut self, le: bool) -> Result<u32, Stop> {
        let b = self.bytes::<4>()?;
        Ok(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn f64(&mut self, le: bool) -> Result<f64, Stop> {
        let b = self.bytes::<8>()?;
        Ok(if le {
            f64::from_le_bytes(b)
        } else {
            f64::from_be_bytes(b)
        })
    }

    fn skip(&mut self, n: u32, dims: usize) -> Result<(), Stop> {
        let size = (n as usize).checked_mul(dims * 8).ok_or(Stop::Invalid)?;
        if self.len() - self.pos < size {
            return Err(Stop::Invalid);
        }
        self.pos += size;
        Ok(())
    }
}

/// Input is already checked to be ASCII hex digits.
fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

const fn name(code: u32) -> Option<&'static str> {
    Some(match code {
        1 => "POINT",
        2 => "LINESTRING",
        3 => "POLYGON",
        4 => "MULTIPOINT",
        5 => "MULTILINESTRING",
        6 => "MULTIPOLYGON",
        7 => "GEOMETRYCOLLECTION",
        8 => "CIRCULARSTRING",
        9 => "COMPOUNDCURVE",
        10 => "CURVEPOLYGON",
        11 => "MULTICURVE",
        12 => "MULTISURFACE",
        15 => "POLYHEDRALSURFACE",
        16 => "TIN",
        17 => "TRIANGLE",
        _ => return None,
    })
}

/// One geometry with its header. `out` is `None` in the check pass. `tagged` is false for
/// members of a multi-geometry, whose WKT has no type name (`MULTIPOINT((1 2),(3 4))`).
fn geometry(
    r: &mut Reader,
    mut out: Option<&mut Out>,
    tagged: bool,
    depth: u32,
) -> Result<(), Stop> {
    if depth > MAX_DEPTH {
        return Err(Stop::Invalid);
    }
    let le = match r.bytes::<1>()?[0] {
        0 => false,
        1 => true,
        _ => return Err(Stop::Invalid),
    };
    let raw = r.u32(le)?;
    let (mut z, mut m) = (raw & Z_FLAG != 0, raw & M_FLAG != 0);
    let mut code = raw & 0x0FFF_FFFF;
    // ISO WKB: 1000s are Z, 2000s M, 3000s ZM.
    match code / 1000 {
        0 => {}
        1 => z = true,
        2 => m = true,
        3 => (z, m) = (true, true),
        _ => return Err(Stop::Invalid),
    }
    code %= 1000;
    let type_name = name(code).ok_or(Stop::Invalid)?;
    if raw & SRID_FLAG != 0 {
        r.u32(le)?;
    }
    let dims = 2 + z as usize + m as usize;
    if tagged && let Some(o) = out.as_deref_mut() {
        o.push(type_name)?;
        if z || m {
            o.push(match (z, m) {
                (true, false) => " Z",
                (false, true) => " M",
                _ => " ZM",
            })?;
            o.after_dims = true;
        }
    }
    match code {
        1 => point(r, out, le, dims, tagged),
        2 | 8 => {
            let n = r.u32(le)?;
            coords(r, out, le, dims, n, tagged)
        }
        3 | 17 => polygon(r, out, le, dims, tagged),
        _ => {
            let n = r.u32(le)?;
            if n == 0 {
                return empty(out, tagged);
            }
            if let Some(o) = out.as_deref_mut() {
                o.push("(")?;
            }
            for i in 0..n {
                if i > 0
                    && let Some(o) = out.as_deref_mut()
                {
                    o.push(",")?;
                }
                // Only multi-geometries of one plain member type leave out its name; curve
                // collections name members that are not the plain type (`CIRCULARSTRING(…)`).
                let member_tagged = match code {
                    4..=6 | 15 | 16 => false,
                    9..=11 => peek_code(r)? != 2,
                    12 => peek_code(r)? != 3,
                    _ => true,
                };
                geometry(r, out.as_deref_mut(), member_tagged, depth + 1)?;
            }
            match out {
                Some(o) => o.push(")"),
                None => Ok(()),
            }
        }
    }
}

/// Type code of the next member without moving the reader.
fn peek_code(r: &mut Reader) -> Result<u32, Stop> {
    let start = r.pos;
    let le = r.bytes::<1>()?[0] == 1;
    let code = (r.u32(le)? & 0x0FFF_FFFF) % 1000;
    r.pos = start;
    Ok(code)
}

fn empty(out: Option<&mut Out>, tagged: bool) -> Result<(), Stop> {
    match out {
        Some(o) => o.push(if tagged { " EMPTY" } else { "EMPTY" }),
        None => Ok(()),
    }
}

/// A point whose coordinates are all NaN is `POINT EMPTY` in WKB.
fn point(
    r: &mut Reader,
    out: Option<&mut Out>,
    le: bool,
    dims: usize,
    tagged: bool,
) -> Result<(), Stop> {
    let Some(o) = out else {
        return r.skip(1, dims);
    };
    let mut c = [0f64; 4];
    for v in c.iter_mut().take(dims) {
        *v = r.f64(le)?;
    }
    if c[..dims].iter().all(|v| v.is_nan()) {
        return empty(Some(o), tagged);
    }
    o.push("(")?;
    write_coord(o, &c[..dims])?;
    o.push(")")
}

fn polygon(
    r: &mut Reader,
    mut out: Option<&mut Out>,
    le: bool,
    dims: usize,
    tagged: bool,
) -> Result<(), Stop> {
    let rings = r.u32(le)?;
    if rings == 0 {
        return empty(out, tagged);
    }
    if let Some(o) = out.as_deref_mut() {
        o.push("(")?;
    }
    for i in 0..rings {
        if i > 0
            && let Some(o) = out.as_deref_mut()
        {
            o.push(",")?;
        }
        let n = r.u32(le)?;
        coords(r, out.as_deref_mut(), le, dims, n, false)?;
    }
    match out {
        Some(o) => o.push(")"),
        None => Ok(()),
    }
}

/// `(x y,x y,…)`, or `EMPTY` for no points.
fn coords(
    r: &mut Reader,
    out: Option<&mut Out>,
    le: bool,
    dims: usize,
    n: u32,
    tagged: bool,
) -> Result<(), Stop> {
    let Some(o) = out else {
        return r.skip(n, dims);
    };
    if n == 0 {
        return empty(Some(o), tagged);
    }
    o.push("(")?;
    let mut c = [0f64; 4];
    for i in 0..n {
        if i > 0 {
            o.push(",")?;
        }
        for v in c.iter_mut().take(dims) {
            *v = r.f64(le)?;
        }
        write_coord(o, &c[..dims])?;
    }
    o.push(")")
}

fn write_coord(o: &mut Out, c: &[f64]) -> Result<(), Stop> {
    for (i, v) in c.iter().enumerate() {
        if i > 0 {
            o.push(" ")?;
        }
        o.num(*v)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hex EWKB as PostGIS 3.5 prints it, and its `ST_AsText`.
    const CASES: &[(&str, &str)] = &[
        (
            "0101000020E61000003333333333B35A40CDCCCCCCCCCC18C0",
            "POINT(106.8 -6.2)",
        ),
        ("0101000000000000000000F87F000000000000F87F", "POINT EMPTY"),
        (
            "01020000800200000000000000000000000000000000000000000000000000F03F000000000000F03F000000000000F03F0000000000000040",
            "LINESTRING Z (0 0 1,1 1 2)",
        ),
        (
            "01030000000200000005000000000000000000000000000000000000000000000000001040000000000000000000000000000010400000000000001040000000000000000000000000000010400000000000000000000000000000000004000000000000000000F03F000000000000F03F0000000000000040000000000000F03F00000000000000400000000000000040000000000000F03F000000000000F03F",
            "POLYGON((0 0,4 0,4 4,0 4,0 0),(1 1,2 1,2 2,1 1))",
        ),
        (
            "0104000040020000000101000040000000000000F03F000000000000004000000000000008400101000040000000000000104000000000000014400000000000001840",
            "MULTIPOINT M ((1 2 3),(4 5 6))",
        ),
        (
            "0106000020110F0000020000000103000000010000000400000000000000000000000000000000000000000000000000F03F0000000000000000000000000000F03F000000000000F03F000000000000000000000000000000000103000000010000000400000000000000000014400000000000001440000000000000184000000000000014400000000000001840000000000000184000000000000014400000000000001440",
            "MULTIPOLYGON(((0 0,1 0,1 1,0 0)),((5 5,6 5,6 6,5 5)))",
        ),
        (
            "0107000000020000000101000000000000000000F03F0000000000000040010200000000000000",
            "GEOMETRYCOLLECTION(POINT(1 2),LINESTRING EMPTY)",
        ),
        (
            "01090000000200000001020000000200000000000000000000000000000000000000000000000000F03F000000000000F03F010800000003000000000000000000F03F000000000000F03F000000000000004000000000000000400000000000000840000000000000F03F",
            "COMPOUNDCURVE((0 0,1 1),CIRCULARSTRING(1 1,2 2,3 1))",
        ),
        (
            "01010000C0000000000000F03F000000000000004000000000000008400000000000001040",
            "POINT ZM (1 2 3 4)",
        ),
        // Big-endian (XDR) WKB.
        ("00000000013FF00000000000004000000000000000", "POINT(1 2)"),
    ];

    #[test]
    fn postgis_values_become_wkt() {
        for (hex, wkt) in CASES {
            assert_eq!(hex_ewkb_to_wkt(hex, None).as_deref(), Some(*wkt), "{hex}");
            let lower = hex.to_ascii_lowercase();
            assert_eq!(
                hex_ewkb_to_wkt(&lower, None).as_deref(),
                Some(*wkt),
                "{lower}"
            );
        }
    }

    #[test]
    fn values_that_are_not_ewkb_are_refused() {
        let point = CASES[0].0;
        for s in [
            "",
            "active",
            "0101",
            "0101000000",                                 // header without coordinates
            &point[..point.len() - 2],                    // one byte short
            &format!("{point}00"),                        // trailing byte
            "0163000000000000000000F03F",                 // unknown type code
            "0201000000000000000000F03F0000000000000040", // bad byte order
            "010100000G000000000000F03F0000000000000040", // not hex
            // A linestring that claims 4 billion points must not be trusted or allocated.
            "0102000000FFFFFFFF000000000000F03F",
        ] {
            assert_eq!(hex_ewkb_to_wkt(s, None), None, "{s}");
        }
    }

    #[test]
    fn deep_nesting_is_refused_without_overflowing_the_stack() {
        let mut hex = "010700000001000000".repeat(10_000);
        hex.push_str("010700000000000000");
        assert_eq!(hex_ewkb_to_wkt(&hex, None), None);
    }

    #[test]
    fn a_cap_cuts_large_geometry_and_export_keeps_it_whole() {
        // LINESTRING of 200 000 points (about 6.4 MB of hex).
        let n = 200_000u32;
        let mut hex = String::from("0102000000");
        hex.push_str(&format!("{:08X}", n.swap_bytes()));
        // (1.5 2.5) little-endian.
        hex.push_str(&"000000000000F83F0000000000000440".repeat(n as usize));
        let cut = hex_ewkb_to_wkt(&hex, Some(64)).unwrap();
        let expected = format!("LINESTRING({}", "1.5 2.5,".repeat(8));
        assert_eq!(cut, format!("{}…", &expected[..64]));
        let full = hex_ewkb_to_wkt(&hex, None).unwrap();
        assert!(full.starts_with("LINESTRING(1.5 2.5,1.5 2.5"));
        assert_eq!(full.matches(',').count() as u32, n - 1);
    }
}
