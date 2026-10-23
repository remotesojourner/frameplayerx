//! The server's list of shares (`NetrShareEnum`, MS-SRVS 3.1.4.8) over the
//! `srvsvc` pipe, in DCE/RPC's classic NDR encoding.
//!
//! smb-rs speaks only NDR64 on its RPC pipes, which Windows takes and
//! Samba (most NAS boxes) refuses with a bind NAK; Windows takes this
//! encoding too. Only what listing shares needs is here: a bind, one
//! request and its answer, each in a single fragment.

use crate::error::{Error, Result};

/// Packet types (MS-RPCE / C706 12.6.4).
const REQUEST: u8 = 0;
const RESPONSE: u8 = 2;
const FAULT: u8 = 3;
const BIND: u8 = 11;
const BIND_ACK: u8 = 12;
const BIND_NAK: u8 = 13;
/// First and last fragment.
const WHOLE: u8 = 0x03;
/// Little-endian integers, ASCII, IEEE floats.
const DREP: [u8; 4] = [0x10, 0, 0, 0];
const HEADER: usize = 16;
/// The largest fragment either side sends; the server cuts its answer to
/// the smaller of this and its own limit.
pub(crate) const MAX_FRAG: u16 = 0xFFFF;
/// `NetrShareEnum`.
const OPNUM_SHARE_ENUM: u16 = 15;
/// `SRVSVC` 3.0 and the NDR transfer syntax 2.0.
const SRVSVC: ([u8; 16], u32) = (
    guid(
        0x4b32_4fc8,
        0x1670,
        0x01d3,
        [0x12, 0x78, 0x5a, 0x47, 0xbf, 0x6e, 0xe1, 0x88],
    ),
    3,
);
const NDR: ([u8; 16], u32) = (
    guid(
        0x8a88_5d04,
        0x1ceb,
        0x11c9,
        [0x9f, 0xe8, 0x08, 0x00, 0x2b, 0x10, 0x48, 0x60],
    ),
    2,
);
/// `STYPE_DISKTREE` in a share's type (its low two bits).
const DISK: u32 = 0;

/// A GUID as it goes on the wire: the first three fields little-endian.
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

/// A PDU: the common header, then `body`.
fn pdu(kind: u8, call_id: u32, body: &[u8]) -> Vec<u8> {
    let mut out = vec![5, 0, kind, WHOLE];
    out.extend_from_slice(&DREP);
    out.extend_from_slice(&((HEADER + body.len()) as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // no authentication
    out.extend_from_slice(&call_id.to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Binds context 0 to SRVSVC in NDR.
pub(crate) fn bind(call_id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&MAX_FRAG.to_le_bytes()); // max_xmit_frag
    b.extend_from_slice(&MAX_FRAG.to_le_bytes()); // max_recv_frag
    b.extend_from_slice(&0u32.to_le_bytes()); // new association group
    b.extend_from_slice(&[1, 0, 0, 0]); // one context element
    b.extend_from_slice(&0u16.to_le_bytes()); // context 0
    b.extend_from_slice(&[1, 0]); // one transfer syntax
    for (uuid, version) in [SRVSVC, NDR] {
        b.extend_from_slice(&uuid);
        b.extend_from_slice(&version.to_le_bytes());
    }
    pdu(BIND, call_id, &b)
}

/// Checks a bind's answer accepted the context.
pub(crate) fn bind_accepted(reply: &[u8]) -> Result<()> {
    let (kind, body) = split(reply)?;
    match kind {
        BIND_ACK => {}
        BIND_NAK => {
            return Err(Error::Smb(
                "the server refused the share list (bind NAK)".into(),
            ));
        }
        k => return Err(Error::Smb(format!("unexpected answer {k} to a bind"))),
    }
    // max_xmit, max_recv, association group; the secondary address
    // (length-prefixed), padded to 4 from the start of the PDU; results.
    let mut r = Reader::new(body, HEADER);
    r.skip(8)?;
    let addr = r.u16()? as usize;
    r.skip(addr)?;
    r.align(4)?;
    let results = r.u8()?;
    r.skip(3)?;
    if results == 0 || r.u16()? != 0 {
        return Err(Error::Smb(
            "the server refused the share list's encoding".into(),
        ));
    }
    Ok(())
}

/// `NetrShareEnum(\\server, level 1)` on context 0.
pub(crate) fn share_enum(call_id: u32, server: &str) -> Vec<u8> {
    let mut s = Writer::default();
    // ServerName: a unique pointer to a conformant varying string.
    s.u32(0x0002_0000);
    s.string(&format!("\\\\{server}"));
    // InfoStruct: level 1, the union's switch, a pointer to an empty
    // container (no entries, no buffer).
    s.u32(1);
    s.u32(1);
    s.u32(0x0002_0004);
    s.u32(0);
    s.u32(0);
    // PreferedMaximumLength: everything.
    s.u32(u32::MAX);
    // ResumeHandle: a unique pointer to 0.
    s.u32(0x0002_0008);
    s.u32(0);
    let stub = s.0;
    let mut b = Vec::new();
    b.extend_from_slice(&(stub.len() as u32).to_le_bytes()); // allocation hint
    b.extend_from_slice(&0u16.to_le_bytes()); // context 0
    b.extend_from_slice(&OPNUM_SHARE_ENUM.to_le_bytes());
    b.extend_from_slice(&stub);
    pdu(REQUEST, call_id, &b)
}

/// One share as the server lists it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Share {
    pub name: String,
    /// `STYPE_*`: 0 disk, 1 printer, 2 device, 3 IPC; high bits for hidden
    /// and temporary shares.
    pub kind: u32,
}

impl Share {
    /// A folder share (not a printer, device or the IPC pipe).
    pub(crate) fn is_disk(&self) -> bool {
        self.kind & 3 == DISK
    }
}

/// The shares in `NetrShareEnum`'s answer.
pub(crate) fn shares(reply: &[u8]) -> Result<Vec<Share>> {
    let (kind, body) = split(reply)?;
    let mut r = Reader::new(body, 0);
    match kind {
        RESPONSE => {}
        FAULT => {
            r.skip(8)?;
            return Err(Error::Smb(format!(
                "share list failed (fault {:#x})",
                r.u32()?
            )));
        }
        k => {
            return Err(Error::Smb(format!(
                "unexpected answer {k} to the share list"
            )));
        }
    }
    // Allocation hint, context, cancel count, reserved; then the stub,
    // aligned from here.
    r.skip(8)?;
    let mut r = Reader::new(&body[8..], 0);
    let _level = r.u32()?;
    let _switch = r.u32()?;
    if r.u32()? == 0 {
        return Ok(Vec::new());
    }
    let count = r.u32()? as usize;
    if r.u32()? == 0 {
        return Ok(Vec::new());
    }
    let max = r.u32()? as usize;
    if count > max || count > 10_000 {
        return Err(Error::Smb("a malformed share list".into()));
    }
    // The entries' fixed parts (name pointer, type, remark pointer), then
    // the strings they point to, in that order.
    let mut fixed = Vec::with_capacity(count);
    for _ in 0..count {
        fixed.push((r.u32()?, r.u32()?, r.u32()?));
    }
    let mut out = Vec::with_capacity(count);
    for (name_ptr, kind, remark_ptr) in fixed {
        let name = if name_ptr != 0 {
            r.string()?
        } else {
            String::new()
        };
        if remark_ptr != 0 {
            r.string()?;
        }
        out.push(Share { name, kind });
    }
    // Total entries, the resume handle, then the status.
    let _total = r.u32()?;
    if r.u32()? != 0 {
        r.u32()?;
    }
    let status = r.u32()?;
    if status != 0 {
        return Err(Error::Smb(format!(
            "share list failed (status {status:#x})"
        )));
    }
    Ok(out)
}

/// A PDU's type and body; one whole fragment of the size it says.
fn split(reply: &[u8]) -> Result<(u8, &[u8])> {
    if reply.len() < HEADER || reply[0] != 5 {
        return Err(Error::Smb("not a DCE/RPC answer".into()));
    }
    if reply[3] & WHOLE != WHOLE {
        return Err(Error::Smb(
            "the share list came in pieces (too many shares)".into(),
        ));
    }
    if reply[4] & 0xf0 != 0x10 {
        return Err(Error::Smb("a big-endian DCE/RPC answer".into()));
    }
    let len = u16::from_le_bytes([reply[8], reply[9]]) as usize;
    let body = reply
        .get(HEADER..len)
        .ok_or_else(|| Error::Smb("a short DCE/RPC answer".into()))?;
    Ok((reply[2], body))
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    /// A conformant varying UTF-16 string with its terminating NUL, padded
    /// to 4.
    fn string(&mut self, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
        self.u32(units.len() as u32);
        self.u32(0);
        self.u32(units.len() as u32);
        for u in units {
            self.0.extend_from_slice(&u.to_le_bytes());
        }
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
    }
}

/// Reads little-endian NDR; `base` is where `data` starts in what its
/// alignment counts from.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    base: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], base: usize) -> Reader<'a> {
        Reader { data, at: 0, base }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let out = self
            .data
            .get(self.at..self.at + n)
            .ok_or_else(|| Error::Smb("a short share list".into()))?;
        self.at += n;
        Ok(out)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    fn align(&mut self, to: usize) -> Result<()> {
        let pad = (to - (self.base + self.at) % to) % to;
        self.skip(pad)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        self.align(4)?;
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A conformant varying UTF-16 string, without its NUL.
    fn string(&mut self) -> Result<String> {
        let _max = self.u32()?;
        let _offset = self.u32()?;
        let n = self.u32()? as usize;
        if n > 4096 {
            return Err(Error::Smb("a malformed share name".into()));
        }
        let units: Vec<u16> = self
            .take(n * 2)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let text = String::from_utf16_lossy(&units);
        Ok(text.trim_end_matches('\0').to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Samba 4's answers (dperson/samba, shares `movies` and `tv`).
    const SAMBA_BIND_ACK: &str = "05000c03100000004400000001000000b810b810f05300000d005c504950455c73727673766300000100000000000000045d888aeb1cc9119fe808002b10486002000000";
    const SAMBA_SHARES: &str = "05000203100000001001000002000000f80000000000000001000000010000000c0002000300000010000200030000001400020000000000180002001c00020000000000200002002400020003000080280002000700000000000000070000006d006f00760069006500730000000000010000000000000001000000000000000300000000000000030000007400760000000000010000000000000001000000000000000500000000000000050000004900500043002400000000001b000000000000001b0000004900500043002000530065007200760069006300650020002800530061006d00620061002000530065007200760065007200290000000000030000002c0002000000000000000000";

    #[test]
    fn samba_takes_the_bind_and_lists_its_shares() {
        bind_accepted(&hex(SAMBA_BIND_ACK)).unwrap();
        let shares = shares(&hex(SAMBA_SHARES)).unwrap();
        let named: Vec<(&str, bool)> = shares
            .iter()
            .map(|s| (s.name.as_str(), s.is_disk()))
            .collect();
        assert_eq!(named, [("movies", true), ("tv", true), ("IPC$", false)]);
    }

    #[test]
    fn a_refused_bind_or_a_fault_is_an_error() {
        let nak = pdu(BIND_NAK, 1, &[0, 0, 0]);
        assert!(bind_accepted(&nak).is_err());
        let mut fault = vec![0u8; 8];
        fault.extend_from_slice(&5u32.to_le_bytes()); // access denied
        let err = shares(&pdu(FAULT, 2, &fault)).unwrap_err();
        assert!(err.to_string().contains("0x5"), "{err}");
        // An answer in pieces isn't read as a whole one.
        let mut first = hex(SAMBA_SHARES);
        first[3] = 0x01;
        assert!(shares(&first).is_err());
    }

    #[test]
    fn requests_are_whole_fragments_in_ndr() {
        let b = bind(1);
        assert_eq!(&b[..4], &[5, 0, BIND, WHOLE]);
        assert_eq!(u16::from_le_bytes([b[8], b[9]]) as usize, b.len());
        // SRVSVC 3.0 over NDR 2.0, as Samba and Windows take it.
        assert!(b.windows(16).any(|w| w == SRVSVC.0));
        assert!(b.windows(16).any(|w| w == NDR.0));
        let r = share_enum(2, "nas");
        assert_eq!(r[2], REQUEST);
        assert_eq!(u16::from_le_bytes([r[8], r[9]]) as usize, r.len());
        assert_eq!(u16::from_le_bytes([r[22], r[23]]), OPNUM_SHARE_ENUM);
        let name: Vec<u8> = "\\\\nas\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(r.windows(name.len()).any(|w| w == name), "server name");
        // The stub's integers stay 4-aligned after the name.
        assert!((r.len() - 24).is_multiple_of(4));
    }
}
