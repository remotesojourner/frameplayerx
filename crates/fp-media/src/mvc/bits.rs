//! Reading and writing H.264 RBSPs bit by bit, and the emulation
//! prevention bytes that keep start codes out of NAL units.

use super::Bad;

/// A NAL unit's RBSP: the unit with its emulation prevention bytes (the 03
/// of every 00 00 03) taken out.
pub(crate) fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Appends `rbsp` to `out` as a NAL unit: an 03 goes in wherever two zero
/// bytes would be followed by a byte up to 03, and after a final zero byte.
pub(crate) fn escape_into(rbsp: &[u8], out: &mut Vec<u8>) {
    out.reserve(rbsp.len() + rbsp.len() / 64 + 1);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    if zeros > 0 {
        out.push(3);
    }
}

/// Where the rbsp_stop_one_bit of an RBSP is, in bits from its start.
pub(crate) fn stop_bit(rbsp: &[u8]) -> Option<usize> {
    let i = rbsp.iter().rposition(|&b| b != 0)?;
    Some(i * 8 + 7 - rbsp[i].trailing_zeros() as usize)
}

/// Reads an RBSP from a bit position.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8], pos: usize) -> Reader<'a> {
        Reader { data, pos }
    }

    /// Bits read so far, from the start of the data.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn u(&mut self, n: u32) -> Result<u32, Bad> {
        debug_assert!(n <= 32);
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self
                .data
                .get(self.pos / 8)
                .ok_or(Bad::Broken("cut short"))?;
            v = v << 1 | u32::from(byte >> (7 - self.pos % 8) & 1);
            self.pos += 1;
        }
        Ok(v)
    }

    pub(crate) fn flag(&mut self) -> Result<bool, Bad> {
        Ok(self.u(1)? == 1)
    }

    /// An unsigned Exp-Golomb code, ue(v).
    pub(crate) fn ue(&mut self) -> Result<u32, Bad> {
        let mut zeros = 0;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(Bad::Broken("Exp-Golomb code too long"));
            }
        }
        let v = (1u64 << zeros) - 1 + u64::from(self.u(zeros)?);
        u32::try_from(v).map_err(|_| Bad::Broken("Exp-Golomb code too long"))
    }

    /// A signed Exp-Golomb code, se(v).
    pub(crate) fn se(&mut self) -> Result<i32, Bad> {
        let k = i64::from(self.ue()?);
        Ok((if k & 1 == 1 { (k + 1) / 2 } else { -(k / 2) }) as i32)
    }
}

/// Builds an RBSP bit by bit.
#[derive(Default)]
pub(crate) struct Writer {
    out: Vec<u8>,
    /// Bits not yet a whole byte, in the low `n` bits.
    acc: u32,
    n: u32,
}

impl Writer {
    pub(crate) fn new() -> Writer {
        Writer::default()
    }

    /// Bits written.
    pub(crate) fn len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    /// The low `n` bits of `v`.
    pub(crate) fn u(&mut self, n: u32, v: u32) {
        debug_assert!(n <= 32 && (n == 32 || v >> n == 0), "{v} in {n} bits");
        for i in (0..n).rev() {
            self.acc = self.acc << 1 | (v >> i & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    pub(crate) fn flag(&mut self, b: bool) {
        self.u(1, u32::from(b));
    }

    pub(crate) fn ue(&mut self, v: u32) {
        let x = u64::from(v) + 1;
        let len = 64 - x.leading_zeros();
        self.u(len - 1, 0);
        if len > 32 {
            self.u(1, 1);
            self.u(32, x as u32);
        } else {
            self.u(len, x as u32);
        }
    }

    pub(crate) fn se(&mut self, v: i32) {
        let v = i64::from(v);
        self.ue((if v > 0 { 2 * v - 1 } else { -2 * v }) as u32);
    }

    /// Bits `from..to` of `src`.
    pub(crate) fn copy(&mut self, src: &[u8], from: usize, to: usize) {
        let mut at = from;
        while at < to && !at.is_multiple_of(8) {
            self.u(1, u32::from(src[at / 8] >> (7 - at % 8) & 1));
            at += 1;
        }
        let whole = (to - at.min(to)) / 8;
        if whole > 0 {
            let bytes = &src[at / 8..at / 8 + whole];
            if self.n == 0 {
                self.out.extend_from_slice(bytes);
            } else {
                for &b in bytes {
                    self.u(8, u32::from(b));
                }
            }
            at += whole * 8;
        }
        while at < to {
            self.u(1, u32::from(src[at / 8] >> (7 - at % 8) & 1));
            at += 1;
        }
    }

    /// Everything written by `other`.
    pub(crate) fn append(&mut self, other: &Writer) {
        self.copy(&other.out, 0, other.out.len() * 8);
        self.u(other.n, other.acc);
    }

    /// Ones up to the next byte (cabac_alignment_one_bit).
    pub(crate) fn align_ones(&mut self) {
        while self.n != 0 {
            self.u(1, 1);
        }
    }

    /// Whole bytes, after the writer is byte aligned.
    pub(crate) fn extend(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.n, 0);
        self.out.extend_from_slice(bytes);
    }

    /// rbsp_trailing_bits: a one, then zeros up to the next byte.
    pub(crate) fn trailing(&mut self) {
        self.u(1, 1);
        while self.n != 0 {
            self.u(1, 0);
        }
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        debug_assert_eq!(self.n, 0, "not byte aligned");
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_codes_round_trip() {
        let mut w = Writer::new();
        let unsigned = [0, 1, 2, 3, 7, 254, 255, 65535, u32::MAX - 1];
        let signed = [0, 1, -1, 2, -2, 1000, -1000, i32::MAX, -i32::MAX];
        for &v in &unsigned {
            w.ue(v);
        }
        for &v in &signed {
            w.se(v);
        }
        w.u(5, 0b10110);
        w.trailing();
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes, 0);
        for &v in &unsigned {
            assert_eq!(r.ue().unwrap(), v);
        }
        for &v in &signed {
            assert_eq!(r.se().unwrap(), v);
        }
        assert_eq!(r.u(5).unwrap(), 0b10110);
        assert_eq!(stop_bit(&bytes), Some(r.pos()));
    }

    #[test]
    fn known_codes() {
        // ue: 0 -> 1, 1 -> 010, 2 -> 011, 3 -> 00100; se(-1) -> 011.
        let mut w = Writer::new();
        w.ue(0);
        w.ue(1);
        w.ue(2);
        w.ue(3);
        w.se(-1);
        assert_eq!(w.len(), 1 + 3 + 3 + 5 + 3);
        w.trailing();
        assert_eq!(w.into_bytes(), [0b1010_0110, 0b0100_0111]);
    }

    #[test]
    fn copies_bits_at_any_phase() {
        let src: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(37) ^ 0x5a).collect();
        for from in [0, 3, 8, 13] {
            for lead in 0..8 {
                let mut w = Writer::new();
                w.u(lead, 0);
                w.copy(&src, from, 300);
                let n = w.len();
                w.trailing();
                let out = w.into_bytes();
                let mut a = Reader::new(&out, lead as usize);
                let mut b = Reader::new(&src, from);
                assert_eq!(n, lead as usize + 300 - from);
                for _ in from..300 {
                    assert_eq!(a.u(1).unwrap(), b.u(1).unwrap(), "from {from} lead {lead}");
                }
            }
        }
    }

    #[test]
    fn emulation_prevention_round_trips() {
        let cases: [&[u8]; 6] = [
            &[0, 0, 0, 0, 1],
            &[1, 0, 0, 2, 0, 0, 3, 0, 0, 4],
            &[0, 0],
            &[5, 0, 0, 0, 0],
            &[0, 0, 3, 0, 0, 3],
            &[0x25, 0x88, 0x80],
        ];
        for rbsp in cases {
            let mut nal = Vec::new();
            escape_into(rbsp, &mut nal);
            assert!(
                !nal.windows(3).any(|w| w[0] == 0 && w[1] == 0 && w[2] <= 2),
                "start code in {nal:?}"
            );
            assert_ne!(nal.last(), Some(&0));
            assert_eq!(unescape(&nal), rbsp);
        }
        let mut nal = Vec::new();
        escape_into(&[0, 0, 1], &mut nal);
        assert_eq!(nal, [0, 0, 3, 1]);
    }
}
