/// Pure-Rust reader for xcdat `trie_8_type` binary files.
///
/// Binary layout (little-endian, produced by `xcdat_build -t 8`):
///
/// trie:
///   u64  m_num_keys
///   code_table
///   bit_vector  m_terms
///   bc_vector_8 m_bcvec
///   tail_vector m_tvec
///
/// immutable_vector<T>:  u64 size, then size*sizeof(T) bytes
/// bit_vector:           u64 m_size, u64 m_num_ones,
///                       immutable_vector<u64> m_bits,
///                       immutable_vector<u64> m_rank_hints,
///                       immutable_vector<u64> m_select_hints
/// code_table:           u64 m_max_length, [u8;512] m_table, immutable_vector<u8> m_alphabet
/// compact_vector:       u64 m_size, u64 m_bits, u64 m_mask, immutable_vector<u64> m_chunks
/// bc_vector_8:          u32 m_num_levels, u64 m_num_frees,
///                       [immutable_vector<u8>; 8] m_bytes,
///                       [bit_vector; 7] m_nexts,
///                       compact_vector m_links,
///                       bit_vector m_leaves
/// tail_vector:          immutable_vector<char(u8)> m_chars, bit_vector m_terms

use crate::prelude::SudachiResult;
use crate::error::SudachiError;

// ── low-level reader ──────────────────────────────────────────────────────────

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self { Reader { data, pos: 0 } }

    fn read_u8(&mut self) -> u8 {
        let v = self.data[self.pos];
        self.pos += 1;
        v
    }

    fn read_u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.data[self.pos..self.pos+4].try_into().unwrap());
        self.pos += 4;
        v
    }

    fn read_u64(&mut self) -> u64 {
        let v = u64::from_le_bytes(self.data[self.pos..self.pos+8].try_into().unwrap());
        self.pos += 8;
        v
    }

    fn read_bytes(&mut self, n: usize) -> &'a [u8] {
        let s = &self.data[self.pos..self.pos+n];
        self.pos += n;
        s
    }

    fn read_vec_u8(&mut self) -> Vec<u8> {
        let n = self.read_u64() as usize;
        self.read_bytes(n).to_vec()
    }

    fn read_vec_u64(&mut self) -> Vec<u64> {
        let n = self.read_u64() as usize;
        let bytes = self.read_bytes(n * 8);
        (0..n).map(|i| u64::from_le_bytes(bytes[i*8..i*8+8].try_into().unwrap())).collect()
    }
}

// ── bit_vector ────────────────────────────────────────────────────────────────

struct BitVector {
    size: u64,
    num_ones: u64,
    bits: Vec<u64>,
    rank_hints: Vec<u64>,
    // select_hints not needed for prefix search
}

impl BitVector {
    fn read(r: &mut Reader) -> Self {
        let size = r.read_u64();
        let num_ones = r.read_u64();
        let bits = r.read_vec_u64();
        let rank_hints = r.read_vec_u64();
        let _select_hints = r.read_vec_u64();
        BitVector { size, num_ones, bits, rank_hints }
    }

    #[inline]
    fn get(&self, i: u64) -> bool {
        self.bits[(i / 64) as usize] & (1u64 << (i % 64)) != 0
    }

    /// Number of 1s in [0..i)
    #[inline]
    fn rank(&self, i: u64) -> u64 {
        if i == self.size { return self.num_ones; }
        let wi = (i / 64) as usize;
        let wj = i % 64;
        self.rank_for_word(wi) + if wj != 0 { (self.bits[wi] << (64 - wj)).count_ones() as u64 } else { 0 }
    }

    fn rank_for_word(&self, wi: usize) -> u64 {
        const BLOCK: usize = 8;
        let bi = wi / BLOCK;
        let bj = wi % BLOCK;
        self.rank_hints[bi * 2] + self.rank_in_block(bi, bj)
    }

    fn rank_in_block(&self, bi: usize, bj: usize) -> u64 {
        (self.rank_hints[bi * 2 + 1] >> ((7 - bj) * 9)) & 0x1FF
    }
}

// ── compact_vector ────────────────────────────────────────────────────────────

struct CompactVector {
    size: u64,
    bits: u64,
    mask: u64,
    chunks: Vec<u64>,
}

impl CompactVector {
    fn read(r: &mut Reader) -> Self {
        let size = r.read_u64();
        let bits = r.read_u64();
        let mask = r.read_u64();
        let chunks = r.read_vec_u64();
        CompactVector { size, bits, mask, chunks }
    }

    #[inline]
    fn get(&self, i: u64) -> u64 {
        let bit_pos = i * self.bits;
        let quo = (bit_pos / 64) as usize;
        let rem = bit_pos % 64;
        if rem + self.bits <= 64 {
            (self.chunks[quo] >> rem) & self.mask
        } else {
            ((self.chunks[quo] >> rem) | (self.chunks[quo + 1] << (64 - rem))) & self.mask
        }
    }
}

// ── bc_vector_8 ───────────────────────────────────────────────────────────────

struct BcVector8 {
    num_levels: u32,
    bytes: [Vec<u8>; 8],
    nexts: [BitVector; 7],
    links: CompactVector,
    leaves: BitVector,
}

impl BcVector8 {
    fn read(r: &mut Reader) -> Self {
        let num_levels = r.read_u32();
        let _num_frees = r.read_u64();
        let bytes: [Vec<u8>; 8] = std::array::from_fn(|_| r.read_vec_u8());
        let nexts: [BitVector; 7] = std::array::from_fn(|_| BitVector::read(r));
        let links = CompactVector::read(r);
        let leaves = BitVector::read(r);
        BcVector8 { num_levels, bytes, nexts, links, leaves }
    }

    #[inline]
    fn access(&self, i: u64) -> u64 {
        let mut j = 0usize;
        let mut idx = i;
        let mut x = self.bytes[0][idx as usize] as u64;
        while j < self.num_levels as usize && self.nexts[j].get(idx) {
            idx = self.nexts[j].rank(idx);
            j += 1;
            x |= (self.bytes[j][idx as usize] as u64) << (j * 8);
        }
        x
    }

    #[inline]
    pub fn base(&self, i: u64) -> u64 { self.access(i * 2) ^ i }
    #[inline]
    pub fn check(&self, i: u64) -> u64 { self.access(i * 2 + 1) ^ i }
    #[inline]
    pub fn is_leaf(&self, i: u64) -> bool { self.leaves.get(i) }
    #[inline]
    pub fn link(&self, i: u64) -> u64 {
        self.bytes[0][(i * 2) as usize] as u64 | (self.links.get(self.leaves.rank(i)) << 8)
    }
}

// ── tail_vector ───────────────────────────────────────────────────────────────

struct TailVector {
    chars: Vec<u8>,
    terms: BitVector,
}

impl TailVector {
    fn read(r: &mut Reader) -> Self {
        let chars = r.read_vec_u8();
        let terms = BitVector::read(r);
        TailVector { chars, terms }
    }

    fn bin_mode(&self) -> bool { self.terms.size != 0 }

    /// Returns Some(matched_key_bytes) if TAIL[tpos..] is a prefix of key[kpos..]
    fn prefix_match(&self, key: &[u8], kpos: usize, tpos: u64) -> Option<usize> {
        if tpos == 0 { return Some(0); }
        if kpos >= key.len() { return None; }
        let mut ki = kpos;
        let mut ti = tpos;
        if self.bin_mode() {
            loop {
                if key[ki] != self.chars[ti as usize] { return None; }
                ki += 1;
                if self.terms.get(ti) { return Some(ki - kpos); }
                ti += 1;
                if ki >= key.len() { return Some(ki - kpos); }
            }
        } else {
            loop {
                if self.chars[ti as usize] == 0 { return Some(ki - kpos); }
                if key[ki] != self.chars[ti as usize] { return None; }
                ki += 1;
                ti += 1;
                if ki >= key.len() { return Some(ki - kpos); }
            }
        }
    }
}

// ── code_table ────────────────────────────────────────────────────────────────

struct CodeTable {
    table: [u8; 512],
}

impl CodeTable {
    fn read(r: &mut Reader) -> Self {
        let _max_length = r.read_u64();
        let mut table = [0u8; 512];
        for b in table.iter_mut() { *b = r.read_u8(); }
        let _alphabet = r.read_vec_u8();
        CodeTable { table }
    }

    #[inline]
    fn get_code(&self, ch: u8) -> u64 { self.table[ch as usize] as u64 }
}

// ── XcdatTrie ─────────────────────────────────────────────────────────────────

pub struct XcdatTrie {
    _num_keys: u64,
    table: CodeTable,
    terms: BitVector,
    bcvec: BcVector8,
    tvec: TailVector,
    /// Maps xcdat rank → word_id_table byte offset
    rank_to_offset: Vec<u32>,
}

impl XcdatTrie {
    pub fn parse(data: &[u8], rank_to_offset: Vec<u32>) -> SudachiResult<Self> {
        let mut r = Reader::new(data);
        let _type_id = r.read_u32();
        let num_keys = r.read_u64();
        let table = CodeTable::read(&mut r);
        let terms = BitVector::read(&mut r);
        let bcvec = BcVector8::read(&mut r);
        let tvec = TailVector::read(&mut r);
        Ok(XcdatTrie { _num_keys: num_keys, table, terms, bcvec, tvec, rank_to_offset })
    }

    /// Translate xcdat rank to word_id_table byte offset
    #[inline]
    pub fn rank_to_offset(&self, rank: u64) -> u32 {
        self.rank_to_offset[rank as usize]
    }

    /// Common-prefix search: yields (trie_id, end_byte_pos) for every prefix of
    /// `input[offset..]` that is stored in the trie.
    pub fn common_prefix_search<'a>(
        &'a self,
        input: &'a [u8],
        offset: usize,
    ) -> XcdatPrefixIter<'a> {
        XcdatPrefixIter {
            trie: self,
            input,
            kpos: offset,
            npos: 0,
            is_beg: true,
            is_end: false,
        }
    }

    #[inline]
    fn npos_to_offset(&self, npos: u64) -> u64 {
        let rank = self.terms.rank(npos);
        self.rank_to_offset[rank as usize] as u64
    }
}

pub struct XcdatPrefixIter<'a> {
    trie: &'a XcdatTrie,
    input: &'a [u8],
    kpos: usize,
    npos: u64,
    is_beg: bool,
    is_end: bool,
}

pub struct XcdatEntry {
    pub value: u64,  // trie id (= word_id_table index)
    pub end: usize,  // byte position in input
}

impl<'a> Iterator for XcdatPrefixIter<'a> {
    type Item = XcdatEntry;

    fn next(&mut self) -> Option<XcdatEntry> {
        if self.is_end { return None; }

        let t = self.trie;

        // On first call, check if root is a term (empty-string match)
        if self.is_beg {
            self.is_beg = false;
            if t.terms.get(self.npos) {
                return Some(XcdatEntry { value: t.npos_to_offset(self.npos), end: self.kpos });
            }
        }

        loop {
            if t.bcvec.is_leaf(self.npos) {
                self.is_end = true;
                let tpos = t.bcvec.link(self.npos);
                let matched = t.tvec.prefix_match(self.input, self.kpos, tpos)?;
                self.kpos += matched;
                return Some(XcdatEntry { value: t.npos_to_offset(self.npos), end: self.kpos });
            }

            if self.kpos >= self.input.len() { self.is_end = true; return None; }

            let cpos = t.bcvec.base(self.npos) ^ t.table.get_code(self.input[self.kpos]);
            if t.bcvec.check(cpos) != self.npos { self.is_end = true; return None; }

            self.kpos += 1;
            self.npos = cpos;

            if !t.bcvec.is_leaf(self.npos) && t.terms.get(self.npos) {
                return Some(XcdatEntry { value: t.npos_to_offset(self.npos), end: self.kpos });
            }
        }
    }
}
