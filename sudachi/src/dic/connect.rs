/*
 *  Copyright (c) 2021 Works Applications Co., Ltd.
 *
 *  Licensed under the Apache License, Version 2.0 (the "License");
 *  you may not use this file except in compliance with the License.
 *  You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 *   Unless required by applicable law or agreed to in writing, software
 *  distributed under the License is distributed on an "AS IS" BASIS,
 *  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *  See the License for the specific language governing permissions and
 *  limitations under the License.
 */

use crate::error::{SudachiError, SudachiResult};

#[cfg(not(feature = "marisa-trie"))]
use crate::util::cow_array::CowArray;

/// The flat (uncompressed) connection matrix, used when `marisa-trie` is NOT enabled.
#[cfg(not(feature = "marisa-trie"))]
pub struct ConnectionMatrix<'a> {
    data: CowArray<'a, i16>,
    num_left: usize,
    num_right: usize,
}

#[cfg(not(feature = "marisa-trie"))]
impl<'a> ConnectionMatrix<'a> {
    pub fn from_offset_size(
        data: &'a [u8],
        offset: usize,
        num_left: usize,
        num_right: usize,
    ) -> SudachiResult<ConnectionMatrix<'a>> {
        let size = num_left * num_right;

        let end = offset + size;
        if end > data.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("connection matrix"));
        }

        Ok(ConnectionMatrix {
            data: CowArray::from_bytes(data, offset, size),
            num_left,
            num_right,
        })
    }

    #[inline(always)]
    fn index(&self, left: u16, right: u16) -> usize {
        let uleft = left as usize;
        let uright = right as usize;
        debug_assert!(uleft < self.num_left);
        debug_assert!(uright < self.num_right);
        let index = uright * self.num_left + uleft;
        debug_assert!(index < self.data.len());
        index
    }

    #[inline(always)]
    pub fn cost(&self, left: u16, right: u16) -> i16 {
        let index = self.index(left, right);
        self.data.get(index).copied().unwrap_or(0)
    }

    pub fn update(&mut self, left: u16, right: u16, value: i16) {
        let index = self.index(left, right);
        self.data.set(index, value);
    }

    pub fn num_left(&self) -> usize {
        self.num_left
    }

    pub fn num_right(&self) -> usize {
        self.num_right
    }
}

/// Block-compressed connection matrix for the `marisa-trie` feature.
///
/// Stores the matrix as bit-packed 256×256 blocks. Each block stores
/// a minimum value and a bit-width, then packs `(value - min)` for every
/// cell using exactly `bit_width` bits. This provides O(1) random access
/// without any decompression cache or mutex.
///
/// Binary format (after the 4-byte magic in Grammar):
/// ```text
/// [num_left: u16][num_right: u16]
/// [num_blocks: u32]
/// [block_offsets: u32 × num_blocks]       — byte offset from data_start
/// [block_data: ...]
/// ```
///
/// Each block:
/// ```text
/// [min_val: i16][bit_width: u8][packed_data: ...]
/// ```
#[cfg(feature = "marisa-trie")]
pub struct ConnectionMatrix<'a> {
    buf: &'a [u8],
    num_left: usize,
    num_right: usize,
    num_col_blocks: usize,
    block_offsets_start: usize,
    data_start: usize,
}

#[cfg(feature = "marisa-trie")]
impl<'a> ConnectionMatrix<'a> {
    /// Block size for bit-packing (256×256 cells per block).
    pub const BLOCK_SIZE: usize = 256;

    /// Bit-packed connection matrix magic: "MCBP"
    pub const BITPACKED_MAGIC: u32 = 0x4D43_4250;

    /// Read the flat (uncompressed) connection matrix — used when loading
    /// dictionaries that have NOT been converted (e.g., dictionaries built
    /// from source via `DictBuilder`).
    pub fn from_offset_size(
        buf: &'a [u8],
        offset: usize,
        num_left: usize,
        num_right: usize,
    ) -> SudachiResult<ConnectionMatrix<'a>> {
        let size = num_left * num_right;
        let byte_size = size * 2;
        if offset + byte_size > buf.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("connection matrix"));
        }

        // For uncompressed matrices, store as a single "block" with the
        // raw i16 data accessible via special-case (num_col_blocks == 0).
        Ok(ConnectionMatrix {
            buf,
            num_left,
            num_right,
            num_col_blocks: 0,
            block_offsets_start: offset, // repurposed: raw data start
            data_start: offset,
        })
    }

    /// Read a bit-packed connection matrix from the dictionary.
    ///
    /// No data is decoded eagerly — values are extracted on demand via `cost()`.
    pub fn from_bitpacked(
        buf: &'a [u8],
        offset: usize,
    ) -> SudachiResult<(ConnectionMatrix<'a>, usize)> {
        let mut pos = offset;

        if pos + 4 > buf.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("bitpacked conn header"));
        }
        let num_left = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap()) as usize;
        let num_right = u16::from_le_bytes(buf[pos + 2..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        let num_blocks = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        let block_offsets_start = pos;
        pos += num_blocks * 4; // u32 per block
        let data_start = pos;

        // Compute total size by finding max(block_offset + block_size) across all blocks
        let num_col_blocks = (num_left + Self::BLOCK_SIZE - 1) / Self::BLOCK_SIZE;

        // Find the end of block data
        let mut max_data_end = data_start;
        for bi in 0..num_blocks {
            let bo_off = block_offsets_start + bi * 4;
            let block_offset = u32::from_le_bytes(buf[bo_off..bo_off + 4].try_into().unwrap()) as usize;
            let abs_start = data_start + block_offset;

            // Parse block header to compute block data size
            let bit_width = buf[abs_start + 2];
            let rb = bi / num_col_blocks;
            let cb = bi % num_col_blocks;
            let actual_rows = std::cmp::min(Self::BLOCK_SIZE, num_right.saturating_sub(rb * Self::BLOCK_SIZE));
            let actual_cols = std::cmp::min(Self::BLOCK_SIZE, num_left.saturating_sub(cb * Self::BLOCK_SIZE));
            let total_bits = actual_rows * actual_cols * bit_width as usize;
            let packed_bytes = (total_bits + 7) / 8;
            let block_end = abs_start + 3 + packed_bytes; // 2 (min_val) + 1 (bit_width) + packed
            if block_end > max_data_end {
                max_data_end = block_end;
            }
        }

        let consumed = max_data_end - offset;
        Ok((
            ConnectionMatrix {
                buf,
                num_left,
                num_right,
                num_col_blocks,
                block_offsets_start,
                data_start,
            },
            consumed,
        ))
    }

    #[inline(always)]
    pub fn cost(&self, left: u16, right: u16) -> i16 {
        let uleft = left as usize;
        let uright = right as usize;
        debug_assert!(uleft < self.num_left);
        debug_assert!(uright < self.num_right);

        // Uncompressed fallback (num_col_blocks == 0)
        if self.num_col_blocks == 0 {
            let index = uright * self.num_left + uleft;
            let pos = self.data_start + index * 2;
            return i16::from_le_bytes(self.buf[pos..pos + 2].try_into().unwrap());
        }

        let col_block = uleft / Self::BLOCK_SIZE;
        let row_block = uright / Self::BLOCK_SIZE;
        let local_col = uleft % Self::BLOCK_SIZE;
        let local_row = uright % Self::BLOCK_SIZE;

        let actual_cols = std::cmp::min(
            Self::BLOCK_SIZE,
            self.num_left - col_block * Self::BLOCK_SIZE,
        );

        let block_idx = row_block * self.num_col_blocks + col_block;
        let bo_off = self.block_offsets_start + block_idx * 4;
        let block_offset = u32::from_le_bytes(
            self.buf[bo_off..bo_off + 4].try_into().unwrap(),
        ) as usize;
        let abs_start = self.data_start + block_offset;

        let min_val = i16::from_le_bytes(self.buf[abs_start..abs_start + 2].try_into().unwrap());
        let bit_width = self.buf[abs_start + 2];

        if bit_width == 0 {
            return min_val;
        }

        let cell_index = local_row * actual_cols + local_col;
        let bit_offset = cell_index * bit_width as usize;
        let packed_start = abs_start + 3;
        let extracted = crate::dic::compact::extract_bits(
            &self.buf[packed_start..],
            bit_offset,
            bit_width,
        );
        min_val.wrapping_add(extracted as i16)
    }

    pub fn update(&mut self, _left: u16, _right: u16, _value: i16) {
        // Bit-packed matrices are read-only; update is a no-op.
        // This method exists for API compatibility.
    }

    pub fn num_left(&self) -> usize {
        self.num_left
    }

    pub fn num_right(&self) -> usize {
        self.num_right
    }
}
