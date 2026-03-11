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
/// Stores the matrix as zstd-compressed 256×256 blocks with a trained
/// dictionary. Blocks may use row-delta encoding (MCZD magic) for ~3x
/// better compression.
///
/// Instead of decompressing the entire matrix on load (~68 MB for core dic),
/// blocks are decompressed lazily: a "row-stripe" cache holds all decoded
/// column-blocks for the current row-block (~3 MB). This reduces memory
/// from ~151 MB to ~72 MB for a typical dictionary.
///
/// Binary format (after the 4-byte magic in Grammar):
/// ```text
/// [num_left: u16][num_right: u16]
/// [num_blocks: u32]
/// [dict_size: u32]
/// [dictionary: u8 × dict_size]
/// [block_index: (offset: u32, size: u32) × num_blocks]
/// [compressed_block_data: ...]
/// ```
#[cfg(feature = "marisa-trie")]
pub struct ConnectionMatrix<'a> {
    buf: &'a [u8],
    num_left: usize,
    num_right: usize,
    num_col_blocks: usize,
    block_index_start: usize,
    data_start: usize,
    delta_encoded: bool,
    cache: std::sync::Mutex<StripeCache>,
}

/// Cache holding one fully-decoded row-stripe (all columns for BLOCK_SIZE rows).
#[cfg(feature = "marisa-trie")]
struct StripeCache {
    row_block: usize,
    /// Flat array: stripe[local_row * num_left + col] = cost value
    data: Vec<i16>,
    decoder: ruzstd::decoding::FrameDecoder,
}

#[cfg(feature = "marisa-trie")]
impl<'a> ConnectionMatrix<'a> {
    /// Block size for compression (256×256 cells per block).
    pub const BLOCK_SIZE: usize = 256;

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

        // For uncompressed matrices, eagerly decode into a flat stripe covering
        // the entire matrix (single "row-block" spanning all rows).
        let mut data = vec![0i16; size];
        for i in 0..size {
            let pos = offset + i * 2;
            data[i] = i16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap());
        }

        // Store as a single-stripe covering everything, with num_col_blocks=0
        // sentinel so `cost()` can use the fast-path directly.
        let cache = StripeCache {
            row_block: 0,
            data,
            decoder: ruzstd::decoding::FrameDecoder::new(),
        };

        Ok(ConnectionMatrix {
            buf,
            num_left,
            num_right,
            num_col_blocks: 0,
            block_index_start: 0,
            data_start: 0,
            delta_encoded: false,
            cache: std::sync::Mutex::new(cache),
        })
    }

    /// Read a block-compressed connection matrix from the dictionary.
    ///
    /// Supports both MCZB (raw blocks) and MCZD (row-delta encoded blocks).
    /// Does NOT decompress any blocks — decompression happens lazily in `cost()`.
    pub fn from_compressed(
        buf: &'a [u8],
        offset: usize,
        delta_encoded: bool,
    ) -> SudachiResult<(ConnectionMatrix<'a>, usize)> {
        let mut pos = offset;

        if pos + 4 > buf.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("compressed conn header"));
        }
        let num_left = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap()) as usize;
        let num_right = u16::from_le_bytes(buf[pos + 2..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        let num_blocks = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // Read zstd dictionary
        let dict_size = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let dict_bytes = &buf[pos..pos + dict_size];
        pos += dict_size;

        let dict = ruzstd::decoding::Dictionary::decode_dict(dict_bytes)
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("connection matrix dict decode failed: {:?}", e),
                )
            })?;

        let block_index_start = pos;

        // Skip block index to find data_start and total consumed bytes
        let index_size = num_blocks * 8;
        if pos + index_size > buf.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("compressed conn index"));
        }
        pos += index_size;
        let data_start = pos;

        // Find the end of compressed data
        let mut max_data_end = data_start;
        for i in 0..num_blocks {
            let idx_off = block_index_start + i * 8;
            let blk_offset = u32::from_le_bytes(buf[idx_off..idx_off + 4].try_into().unwrap()) as usize;
            let blk_size = u32::from_le_bytes(buf[idx_off + 4..idx_off + 8].try_into().unwrap()) as usize;
            let abs_end = data_start + blk_offset + blk_size;
            if abs_end > max_data_end {
                max_data_end = abs_end;
            }
        }

        let num_col_blocks = (num_left + Self::BLOCK_SIZE - 1) / Self::BLOCK_SIZE;

        // Prepare decoder with dictionary (no decompression yet)
        let mut decoder = ruzstd::decoding::FrameDecoder::new();
        decoder.add_dict(dict).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("connection matrix add_dict failed: {:?}", e),
            )
        })?;

        let cache = StripeCache {
            row_block: usize::MAX, // sentinel: no stripe loaded yet
            data: vec![0i16; Self::BLOCK_SIZE * num_left],
            decoder,
        };

        let consumed = max_data_end - offset;
        Ok((
            ConnectionMatrix {
                buf,
                num_left,
                num_right,
                num_col_blocks,
                block_index_start,
                data_start,
                delta_encoded,
                cache: std::sync::Mutex::new(cache),
            },
            consumed,
        ))
    }

    /// Decompress all column-blocks for the given row-block into the stripe cache.
    fn load_row_stripe(
        cache: &mut StripeCache,
        buf: &[u8],
        row_block: usize,
        num_left: usize,
        num_right: usize,
        num_col_blocks: usize,
        block_index_start: usize,
        data_start: usize,
        delta_encoded: bool,
    ) {
        let bs = Self::BLOCK_SIZE;
        let row_start = row_block * bs;
        let row_end = std::cmp::min(row_start + bs, num_right);
        let actual_rows = row_end - row_start;

        // Zero out the stripe
        for v in cache.data[..actual_rows * num_left].iter_mut() {
            *v = 0;
        }

        // Decompress each column-block in this row
        for cb in 0..num_col_blocks {
            let blk_idx = row_block * num_col_blocks + cb;
            let idx_off = block_index_start + blk_idx * 8;
            let blk_offset = u32::from_le_bytes(buf[idx_off..idx_off + 4].try_into().unwrap()) as usize;
            let blk_size = u32::from_le_bytes(buf[idx_off + 4..idx_off + 8].try_into().unwrap()) as usize;

            let abs_offset = data_start + blk_offset;
            let compressed = &buf[abs_offset..abs_offset + blk_size];

            let decompressed = {
                use std::io::Read;
                let mut cursor = std::io::Cursor::new(compressed);
                cache.decoder.reset(&mut cursor).unwrap_or_else(|e| {
                    panic!("connection matrix block {} zstd reset failed: {:?}", blk_idx, e);
                });
                cache.decoder.decode_blocks(
                    &mut cursor,
                    ruzstd::decoding::BlockDecodingStrategy::All,
                ).unwrap_or_else(|e| {
                    panic!("connection matrix block {} zstd decode failed: {:?}", blk_idx, e);
                });
                let mut out = Vec::new();
                cache.decoder.read_to_end(&mut out).unwrap_or_else(|e| {
                    panic!("connection matrix block {} zstd collect failed: {}", blk_idx, e);
                });
                out
            };

            let col_start = cb * bs;
            let col_end = std::cmp::min(col_start + bs, num_left);

            // Copy decompressed i16 values into the stripe, applying inverse delta if needed
            let mut src = 0;
            for local_r in 0..actual_rows {
                let mut prev: i16 = 0;
                for c in col_start..col_end {
                    if src + 2 <= decompressed.len() {
                        let raw = i16::from_le_bytes(
                            decompressed[src..src + 2].try_into().unwrap(),
                        );
                        let val = if delta_encoded {
                            if c == col_start {
                                raw // first column: stored as-is
                            } else {
                                prev.wrapping_add(raw) // subsequent: cumulative sum
                            }
                        } else {
                            raw
                        };
                        cache.data[local_r * num_left + c] = val;
                        prev = val;
                    }
                    src += 2;
                }
            }
        }

        cache.row_block = row_block;
    }

    #[inline(always)]
    pub fn cost(&self, left: u16, right: u16) -> i16 {
        let uleft = left as usize;
        let uright = right as usize;
        debug_assert!(uleft < self.num_left);
        debug_assert!(uright < self.num_right);

        // For uncompressed matrices (num_col_blocks == 0), the cache holds everything
        if self.num_col_blocks == 0 {
            let index = uright * self.num_left + uleft;
            let cache = self.cache.lock().unwrap();
            return cache.data.get(index).copied().unwrap_or(0);
        }

        let row_block = uright / Self::BLOCK_SIZE;
        let local_row = uright % Self::BLOCK_SIZE;

        let mut cache = self.cache.lock().unwrap();
        if cache.row_block != row_block {
            Self::load_row_stripe(
                &mut cache,
                self.buf,
                row_block,
                self.num_left,
                self.num_right,
                self.num_col_blocks,
                self.block_index_start,
                self.data_start,
                self.delta_encoded,
            );
        }

        let index = local_row * self.num_left + uleft;
        cache.data.get(index).copied().unwrap_or(0)
    }

    pub fn update(&mut self, left: u16, right: u16, value: i16) {
        let uleft = left as usize;
        let uright = right as usize;
        let row_block = uright / Self::BLOCK_SIZE;
        let local_row = uright % Self::BLOCK_SIZE;

        let mut cache = self.cache.lock().unwrap();
        if self.num_col_blocks != 0 && cache.row_block != row_block {
            Self::load_row_stripe(
                &mut cache,
                self.buf,
                row_block,
                self.num_left,
                self.num_right,
                self.num_col_blocks,
                self.block_index_start,
                self.data_start,
                self.delta_encoded,
            );
        }

        let index = if self.num_col_blocks == 0 {
            uright * self.num_left + uleft
        } else {
            local_row * self.num_left + uleft
        };
        if index < cache.data.len() {
            cache.data[index] = value;
        }
    }

    pub fn num_left(&self) -> usize {
        self.num_left
    }

    pub fn num_right(&self) -> usize {
        self.num_right
    }
}
