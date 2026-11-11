//! DEFLATE (RFC 1951) compression and decompression with zlib framing (RFC 1950).
//!
//! The compressor runs LZ77 with hash chains and lazy matching, then encodes each
//! block in whichever of stored / fixed-Huffman / dynamic-Huffman form is smallest.
//! Dynamic codes are optimal length-limited Huffman codes (package-merge).

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// Order in which code-length code lengths are transmitted.
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 64;
const NICE_LEN: usize = 128;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// LZ77 tokens per block; each block gets its own Huffman codes.
const BLOCK_TOKENS: usize = 1 << 15;

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// zlib stream: 2-byte header, raw DEFLATE data, Adler-32 of the input.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9C];
    out.extend(compress(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Inverse of [`zlib_compress`]; accepts any valid zlib stream without a preset dictionary.
pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 6 {
        return Err("zlib stream too short".into());
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0F != 8 || !(u16::from(cmf) << 8 | u16::from(flg)).is_multiple_of(31) || flg & 0x20 != 0 {
        return Err("invalid zlib header".into());
    }
    let out = inflate(&data[2..data.len() - 4])?;
    let expect = u32::from_be_bytes(data[data.len() - 4..].try_into().unwrap());
    if adler32(&out) != expect {
        return Err("zlib checksum mismatch".into());
    }
    Ok(out)
}

// ------------------------------------------------------------------ bit output

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    #[inline]
    fn put(&mut self, v: u32, bits: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn align(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}

// ------------------------------------------------------------------ LZ77

/// A literal (`dist == 0`, `len` = byte) or a back-reference.
#[derive(Clone, Copy)]
struct Token {
    len: u16,
    dist: u16,
}

const NONE: u32 = u32::MAX;

struct Matcher<'a> {
    data: &'a [u8],
    head: Vec<u32>,
    prev: Vec<u32>,
}

impl Matcher<'_> {
    #[inline]
    fn hash(&self, i: usize) -> usize {
        let d = self.data;
        let v = u32::from(d[i]) | u32::from(d[i + 1]) << 8 | u32::from(d[i + 2]) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    #[inline]
    fn insert(&mut self, i: usize) {
        if i + MIN_MATCH <= self.data.len() {
            let h = self.hash(i);
            self.prev[i % WINDOW] = self.head[h];
            self.head[h] = i as u32;
        }
    }

    /// Longest earlier match for position `i` (not yet inserted): (len, dist).
    fn find(&self, i: usize) -> (usize, usize) {
        let d = self.data;
        if i + MIN_MATCH > d.len() {
            return (0, 0);
        }
        let max_len = (d.len() - i).min(MAX_MATCH);
        let (mut best_len, mut best_dist) = (MIN_MATCH - 1, 0);
        let mut cand = self.head[self.hash(i)];
        let mut chain = MAX_CHAIN;
        while cand != NONE && chain > 0 {
            let c = cand as usize;
            let dist = i - c;
            if dist > WINDOW {
                break;
            }
            if d[c + best_len] == d[i + best_len] {
                let len = d[c..c + max_len].iter().zip(&d[i..i + max_len]).take_while(|(a, b)| a == b).count();
                if len > best_len {
                    best_len = len;
                    best_dist = dist;
                    if len >= max_len.min(NICE_LEN) {
                        break;
                    }
                }
            }
            let next = self.prev[c % WINDOW];
            if next == NONE || next as usize >= c {
                break; // stale entry from a newer position
            }
            cand = next;
            chain -= 1;
        }
        if best_len >= MIN_MATCH { (best_len, best_dist) } else { (0, 0) }
    }
}

fn lz77(data: &[u8]) -> Vec<Token> {
    let mut m = Matcher { data, head: vec![NONE; 1 << HASH_BITS], prev: vec![NONE; WINDOW] };
    let mut toks = Vec::with_capacity(data.len() / 3 + 16);
    let lit = |b: u8| Token { len: u16::from(b), dist: 0 };
    let mut i = 0;
    while i < data.len() {
        let (len, dist) = m.find(i);
        m.insert(i);
        if len == 0 {
            toks.push(lit(data[i]));
            i += 1;
            continue;
        }
        // Lazy matching: emit a literal if the next position has a longer match.
        if len < NICE_LEN && m.find(i + 1).0 > len {
            toks.push(lit(data[i]));
            i += 1;
            continue;
        }
        toks.push(Token { len: len as u16, dist: dist as u16 });
        for j in i + 1..i + len {
            m.insert(j);
        }
        i += len;
    }
    toks
}

#[inline]
fn len_code(len: usize) -> usize {
    LEN_BASE.partition_point(|&b| b as usize <= len) - 1
}

#[inline]
fn dist_code(dist: usize) -> usize {
    DIST_BASE.partition_point(|&b| b as usize <= dist) - 1
}

// ------------------------------------------------------------------ Huffman codes

/// Optimal code lengths with no code longer than `limit` bits (package-merge).
fn huffman_lengths(freq: &[u32], limit: u32) -> Vec<u8> {
    let mut lens = vec![0u8; freq.len()];
    let mut leaves: Vec<(u64, u16)> =
        freq.iter().enumerate().filter(|&(_, &f)| f > 0).map(|(i, &f)| (u64::from(f), i as u16)).collect();
    match leaves.len() {
        0 => return lens,
        1 => {
            lens[leaves[0].1 as usize] = 1;
            return lens;
        }
        _ => {}
    }
    leaves.sort_unstable();
    let leaf_items: Vec<(u64, Vec<u16>)> = leaves.iter().map(|&(w, s)| (w, vec![s])).collect();
    let mut list = leaf_items.clone();
    for _ in 1..limit {
        let packages: Vec<(u64, Vec<u16>)> = list
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[a, b]| (a.0 + b.0, [a.1.as_slice(), b.1.as_slice()].concat()))
            .collect();
        let mut merged = Vec::with_capacity(leaf_items.len() + packages.len());
        let (mut a, mut b) = (0, 0);
        while a < leaf_items.len() || b < packages.len() {
            if b >= packages.len() || (a < leaf_items.len() && leaf_items[a].0 <= packages[b].0) {
                merged.push(leaf_items[a].clone());
                a += 1;
            } else {
                merged.push(packages[b].clone());
                b += 1;
            }
        }
        list = merged;
    }
    for item in &list[..2 * leaves.len() - 2] {
        for &s in &item.1 {
            lens[s as usize] += 1;
        }
    }
    lens
}

/// Canonical codes for `lens`, bit-reversed for LSB-first output.
fn canonical_codes(lens: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            c.reverse_bits() >> (16 - l)
        })
        .collect()
}

fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let mut ll = vec![8u8; 288];
    ll[144..256].fill(9);
    ll[256..280].fill(7);
    (ll, vec![5u8; 30])
}

/// Ensures at least two symbols have nonzero frequency, so codes are complete.
fn at_least_two(freq: &mut [u32]) {
    let used = freq.iter().filter(|&&f| f > 0).count();
    for f in freq.iter_mut().take(2) {
        if used < 2 && *f == 0 {
            *f = 1;
        }
    }
}

/// Run-length encodes the code lengths with symbols 16/17/18: (symbol, extra value).
fn rle_lengths(lens: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let run = lens[i..].iter().take_while(|&&x| x == v).count();
        let mut r = run;
        if v == 0 {
            while r >= 11 {
                let k = r.min(138);
                out.push((18, (k - 11) as u8));
                r -= k;
            }
            if r >= 3 {
                out.push((17, (r - 3) as u8));
                r = 0;
            }
        } else {
            out.push((v, 0));
            r -= 1;
            while r >= 3 {
                let k = r.min(6);
                out.push((16, (k - 3) as u8));
                r -= k;
            }
        }
        out.extend(std::iter::repeat_n((v, 0), r));
        i += run;
    }
    out
}

const CL_EXTRA: [u32; 19] = {
    let mut e = [0u32; 19];
    e[16] = 2;
    e[17] = 3;
    e[18] = 7;
    e
};

struct DynamicHeader {
    ll: Vec<u8>,
    dist: Vec<u8>,
    hlit: usize,
    hdist: usize,
    cl: Vec<u8>,
    hclen: usize,
    rle: Vec<(u8, u8)>,
}

impl DynamicHeader {
    fn new(lf: &[u32; 286], df: &[u32; 30]) -> DynamicHeader {
        let (mut lf, mut df) = (*lf, *df);
        at_least_two(&mut lf);
        at_least_two(&mut df);
        let ll = huffman_lengths(&lf, 15);
        let dist = huffman_lengths(&df, 15);
        let hlit = ll.iter().rposition(|&l| l > 0).map_or(0, |p| p + 1).max(257);
        let hdist = dist.iter().rposition(|&l| l > 0).map_or(0, |p| p + 1).max(1);
        let rle = rle_lengths(&[&ll[..hlit], &dist[..hdist]].concat());
        let mut cf = [0u32; 19];
        for &(s, _) in &rle {
            cf[s as usize] += 1;
        }
        at_least_two(&mut cf);
        let cl = huffman_lengths(&cf, 7);
        let hclen = CL_ORDER.iter().rposition(|&s| cl[s] > 0).map_or(0, |p| p + 1).max(4);
        DynamicHeader { ll, dist, hlit, hdist, cl, hclen, rle }
    }

    fn bits(&self) -> u64 {
        let rle: u64 =
            self.rle.iter().map(|&(s, _)| u64::from(u32::from(self.cl[s as usize]) + CL_EXTRA[s as usize])).sum();
        14 + 3 * self.hclen as u64 + rle
    }

    fn write(&self, bw: &mut BitWriter) {
        bw.put((self.hlit - 257) as u32, 5);
        bw.put((self.hdist - 1) as u32, 5);
        bw.put((self.hclen - 4) as u32, 4);
        for &s in &CL_ORDER[..self.hclen] {
            bw.put(u32::from(self.cl[s]), 3);
        }
        let codes = canonical_codes(&self.cl);
        for &(s, extra) in &self.rle {
            bw.put(u32::from(codes[s as usize]), u32::from(self.cl[s as usize]));
            if CL_EXTRA[s as usize] > 0 {
                bw.put(u32::from(extra), CL_EXTRA[s as usize]);
            }
        }
    }
}

fn payload_bits(lf: &[u32; 286], df: &[u32; 30], ll: &[u8], dl: &[u8]) -> u64 {
    let mut bits = 0u64;
    for (s, &f) in lf.iter().enumerate() {
        let extra = if s > 256 { u64::from(LEN_EXTRA[s - 257]) } else { 0 };
        bits += u64::from(f) * (u64::from(ll[s]) + extra);
    }
    for (s, &f) in df.iter().enumerate() {
        bits += u64::from(f) * (u64::from(dl[s]) + u64::from(DIST_EXTRA[s]));
    }
    bits
}

fn write_tokens(bw: &mut BitWriter, toks: &[Token], ll: &[u8], dl: &[u8]) {
    let (lc, dc) = (canonical_codes(ll), canonical_codes(dl));
    for t in toks {
        if t.dist == 0 {
            bw.put(u32::from(lc[t.len as usize]), u32::from(ll[t.len as usize]));
        } else {
            let (len, dist) = (t.len as usize, t.dist as usize);
            let c = len_code(len);
            bw.put(u32::from(lc[257 + c]), u32::from(ll[257 + c]));
            bw.put((len - LEN_BASE[c] as usize) as u32, u32::from(LEN_EXTRA[c]));
            let d = dist_code(dist);
            bw.put(u32::from(dc[d]), u32::from(dl[d]));
            bw.put((dist - DIST_BASE[d] as usize) as u32, u32::from(DIST_EXTRA[d]));
        }
    }
    bw.put(u32::from(lc[256]), u32::from(ll[256]));
}

fn write_stored(bw: &mut BitWriter, raw: &[u8], last: bool) {
    let chunks: Vec<&[u8]> = if raw.is_empty() { vec![raw] } else { raw.chunks(65535).collect() };
    for (i, c) in chunks.iter().enumerate() {
        bw.put(u32::from(last && i + 1 == chunks.len()), 1);
        bw.put(0, 2);
        bw.align();
        let len = c.len() as u16;
        bw.out.extend_from_slice(&len.to_le_bytes());
        bw.out.extend_from_slice(&(!len).to_le_bytes());
        bw.out.extend_from_slice(c);
    }
}

fn write_block(bw: &mut BitWriter, toks: &[Token], raw: &[u8], last: bool) {
    let mut lf = [0u32; 286];
    let mut df = [0u32; 30];
    for t in toks {
        if t.dist == 0 {
            lf[t.len as usize] += 1;
        } else {
            lf[257 + len_code(t.len as usize)] += 1;
            df[dist_code(t.dist as usize)] += 1;
        }
    }
    lf[256] = 1;

    let dynamic = DynamicHeader::new(&lf, &df);
    let dyn_bits = 3 + dynamic.bits() + payload_bits(&lf, &df, &dynamic.ll, &dynamic.dist);
    let (fll, fdl) = fixed_lengths();
    let fixed_bits = 3 + payload_bits(&lf, &df, &fll, &fdl);
    let stored_bits = (raw.len() as u64 + 5 * raw.len().div_ceil(65535).max(1) as u64) * 8 + 7;

    if stored_bits <= dyn_bits.min(fixed_bits) {
        write_stored(bw, raw, last);
    } else if fixed_bits <= dyn_bits {
        bw.put(u32::from(last), 1);
        bw.put(1, 2);
        write_tokens(bw, toks, &fll, &fdl);
    } else {
        bw.put(u32::from(last), 1);
        bw.put(2, 2);
        dynamic.write(bw);
        write_tokens(bw, toks, &dynamic.ll, &dynamic.dist);
    }
}

/// Raw DEFLATE stream.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let toks = lz77(data);
    let mut bw = BitWriter { out: Vec::with_capacity(data.len() / 2 + 64), acc: 0, n: 0 };
    if toks.is_empty() {
        write_stored(&mut bw, &[], true);
        return bw.out;
    }
    let blocks: Vec<&[Token]> = toks.chunks(BLOCK_TOKENS).collect();
    let mut pos = 0;
    for (i, b) in blocks.iter().enumerate() {
        let size: usize = b.iter().map(|t| if t.dist == 0 { 1 } else { t.len as usize }).sum();
        write_block(&mut bw, b, &data[pos..pos + size], i + 1 == blocks.len());
        pos += size;
    }
    bw.align();
    bw.out
}

// ------------------------------------------------------------------ decompression

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl BitReader<'_> {
    #[inline]
    fn bits(&mut self, need: u32) -> Result<u32, String> {
        while self.n < need {
            let b = *self.data.get(self.pos).ok_or("unexpected end of deflate stream")?;
            self.pos += 1;
            self.acc |= u32::from(b) << self.n;
            self.n += 8;
        }
        let v = self.acc & ((1u64 << need) - 1) as u32;
        self.acc >>= need;
        self.n -= need;
        Ok(v)
    }
}

/// Canonical Huffman decoding table (as in zlib's `puff`).
struct Decoder {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Decoder {
    fn new(lens: &[u8]) -> Decoder {
        let mut count = [0u16; 16];
        for &l in lens {
            count[l as usize] += 1;
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbol = vec![0u16; lens.len()];
        for (s, &l) in lens.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        count[0] = 0;
        Decoder { count, symbol }
    }

    fn decode(&self, br: &mut BitReader<'_>) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = i32::from(self.count[len]);
            if code - count < first {
                return Ok(self.symbol[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid Huffman code".into())
    }
}

pub fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut br = BitReader { data, pos: 0, acc: 0, n: 0 };
    let mut out = Vec::new();
    loop {
        let last = br.bits(1)?;
        match br.bits(2)? {
            0 => {
                br.acc = 0;
                br.n = 0;
                let p = br.pos;
                let hdr = data.get(p..p + 4).ok_or("truncated stored block")?;
                let len = u16::from_le_bytes([hdr[0], hdr[1]]);
                if len != !u16::from_le_bytes([hdr[2], hdr[3]]) {
                    return Err("stored block length mismatch".into());
                }
                let body = data.get(p + 4..p + 4 + len as usize).ok_or("truncated stored block")?;
                out.extend_from_slice(body);
                br.pos = p + 4 + len as usize;
            }
            1 => {
                let (ll, dl) = fixed_lengths();
                inflate_block(&mut br, &mut out, &Decoder::new(&ll), &Decoder::new(&dl))?;
            }
            2 => {
                let hlit = br.bits(5)? as usize + 257;
                let hdist = br.bits(5)? as usize + 1;
                let hclen = br.bits(4)? as usize + 4;
                let mut cl = [0u8; 19];
                for &s in &CL_ORDER[..hclen] {
                    cl[s] = br.bits(3)? as u8;
                }
                let cld = Decoder::new(&cl);
                let mut lens = Vec::with_capacity(hlit + hdist);
                while lens.len() < hlit + hdist {
                    let (v, n) = match cld.decode(&mut br)? {
                        s @ 0..=15 => (s as u8, 1),
                        16 => (*lens.last().ok_or("repeat with no previous length")?, 3 + br.bits(2)?),
                        17 => (0, 3 + br.bits(3)?),
                        _ => (0, 11 + br.bits(7)?),
                    };
                    lens.extend(std::iter::repeat_n(v, n as usize));
                }
                if lens.len() != hlit + hdist {
                    return Err("code lengths overflow".into());
                }
                inflate_block(&mut br, &mut out, &Decoder::new(&lens[..hlit]), &Decoder::new(&lens[hlit..]))?;
            }
            _ => return Err("invalid block type".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

fn inflate_block(br: &mut BitReader<'_>, out: &mut Vec<u8>, ll: &Decoder, dl: &Decoder) -> Result<(), String> {
    loop {
        let s = ll.decode(br)? as usize;
        match s {
            0..=255 => out.push(s as u8),
            256 => return Ok(()),
            _ => {
                let c = s - 257;
                if c >= 29 {
                    return Err("invalid length code".into());
                }
                let len = LEN_BASE[c] as usize + br.bits(u32::from(LEN_EXTRA[c]))? as usize;
                let d = dl.decode(br)? as usize;
                if d >= 30 {
                    return Err("invalid distance code".into());
                }
                let dist = DIST_BASE[d] as usize + br.bits(u32::from(DIST_EXTRA[d]))? as usize;
                if dist > out.len() {
                    return Err("distance too far back".into());
                }
                let start = out.len() - dist;
                for i in 0..len {
                    out.push(out[start + i]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn roundtrip(data: &[u8]) -> usize {
        let z = zlib_compress(data);
        assert_eq!(zlib_decompress(&z).unwrap(), data);
        z.len()
    }

    #[test]
    fn roundtrips() {
        roundtrip(b"");
        roundtrip(b"a");
        roundtrip(b"abcabcabcabcabcabcabc");
        // Long runs (distance 1, max-length matches).
        assert!(roundtrip(&vec![7u8; 200_000]) < 1000);
        // Incompressible data must fall back to stored blocks (tiny overhead).
        let mut rng = Rng::new(1, 2);
        let noise: Vec<u8> = (0..100_000).map(|_| (rng.f32() * 256.0) as u8).collect();
        assert!(roundtrip(&noise) < 100_000 + 100);
        // Skewed alphabet: dynamic Huffman should beat 8 bits per symbol.
        let skewed: Vec<u8> = (0..100_000).map(|_| (rng.f32() * rng.f32() * 16.0) as u8).collect();
        assert!(roundtrip(&skewed) < 60_000);
        // Text-like data with repeats beyond one block.
        let text: Vec<u8> =
            (0..300_000u32).map(|i| b"the quick brown fox "[(i * 7 % 20) as usize] ^ u8::from(i % 3 == 0)).collect();
        roundtrip(&text);
    }

    #[test]
    fn length_limited_huffman() {
        // Fibonacci frequencies force unlimited Huffman codes deeper than the limit.
        let mut f = vec![1u32, 1];
        while f.len() < 30 {
            let n = f[f.len() - 1] + f[f.len() - 2];
            f.push(n);
        }
        let lens = huffman_lengths(&f, 15);
        assert!(lens.iter().all(|&l| (1..=15).contains(&l)));
        let kraft: f64 = lens.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
        assert!((kraft - 1.0).abs() < 1e-12, "code must be complete, kraft {kraft}");
    }
}
