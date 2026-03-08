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
        *unsafe { self.data.get_unchecked(index) }
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
/// The converter stores the matrix as zstd-compressed 64×64 blocks
/// with a trained zstd dictionary for better small-block compression.
/// On load, the full matrix is decompressed into a `Vec<i16>`.
///
/// Binary format:
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
    _phantom: std::marker::PhantomData<&'a ()>,
    data: Vec<i16>,
    num_left: usize,
    num_right: usize,
}

#[cfg(feature = "marisa-trie")]
impl<'a> ConnectionMatrix<'a> {
    /// Block size for compression (64×64 cells per block).
    pub const BLOCK_SIZE: usize = 64;

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

        let mut data = vec![0i16; size];
        for i in 0..size {
            let pos = offset + i * 2;
            data[i] = i16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap());
        }

        Ok(ConnectionMatrix {
            _phantom: std::marker::PhantomData,
            data,
            num_left,
            num_right,
        })
    }

    /// Read a block-compressed connection matrix from the dictionary.
    pub fn from_compressed(buf: &[u8], offset: usize) -> SudachiResult<(ConnectionMatrix<'a>, usize)> {
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

        // Read block index
        let index_size = num_blocks * 8; // (offset: u32, size: u32) per block
        if pos + index_size > buf.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("compressed conn index"));
        }
        let mut block_index = Vec::with_capacity(num_blocks);
        for _ in 0..num_blocks {
            let blk_offset = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
            let blk_size = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
            block_index.push((blk_offset, blk_size));
            pos += 8;
        }

        let data_start = pos;

        // Prepare FrameDecoder with dictionary
        let mut frame_decoder = ruzstd::decoding::FrameDecoder::new();
        frame_decoder.add_dict(dict).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("connection matrix add_dict failed: {:?}", e),
            )
        })?;

        // Decompress all blocks into a flat array
        let total_size = num_left * num_right;
        let mut data = vec![0i16; total_size];

        let bs = Self::BLOCK_SIZE;
        let num_row_blocks = (num_right + bs - 1) / bs;
        let num_col_blocks = (num_left + bs - 1) / bs;

        let mut max_data_end = data_start;

        for (blk_idx, &(blk_offset, blk_size)) in block_index.iter().enumerate() {
            let abs_offset = data_start + blk_offset;
            let abs_end = abs_offset + blk_size;
            if abs_end > buf.len() {
                return Err(SudachiError::InvalidDictionaryGrammar.with_context("compressed block data"));
            }
            if abs_end > max_data_end {
                max_data_end = abs_end;
            }

            let compressed = &buf[abs_offset..abs_end];
            let decompressed = {
                use std::io::Read;
                let mut cursor = std::io::Cursor::new(compressed);
                frame_decoder.reset(&mut cursor).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("block {} zstd init failed: {:?}", blk_idx, e),
                    )
                })?;
                frame_decoder.decode_blocks(
                    &mut cursor,
                    ruzstd::decoding::BlockDecodingStrategy::All,
                ).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("block {} zstd decode failed: {:?}", blk_idx, e),
                    )
                })?;
                let mut buf = Vec::new();
                frame_decoder.read_to_end(&mut buf).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("block {} zstd collect failed: {}", blk_idx, e),
                    )
                })?;
                buf
            };

            // Determine block position in the matrix
            let row_block = blk_idx / num_col_blocks;
            let col_block = blk_idx % num_col_blocks;
            let row_start = row_block * bs;
            let col_start = col_block * bs;

            // Copy decompressed i16 values into the flat array
            // Block layout matches matrix layout: row-major within the block,
            // where "row" = right_id, "col" = left_id
            let mut src = 0;
            for r in row_start..std::cmp::min(row_start + bs, num_right) {
                for c in col_start..std::cmp::min(col_start + bs, num_left) {
                    if src + 1 < decompressed.len() {
                        let val = i16::from_le_bytes(
                            decompressed[src..src + 2].try_into().unwrap(),
                        );
                        data[r * num_left + c] = val;
                    }
                    src += 2;
                }
            }
        }

        let consumed = max_data_end - offset;
        Ok((
            ConnectionMatrix {
                _phantom: std::marker::PhantomData,
                data,
                num_left,
                num_right,
            },
            consumed,
        ))
    }

    #[inline(always)]
    fn index(&self, left: u16, right: u16) -> usize {
        let uleft = left as usize;
        let uright = right as usize;
        debug_assert!(uleft < self.num_left);
        debug_assert!(uright < self.num_right);
        uright * self.num_left + uleft
    }

    #[inline(always)]
    pub fn cost(&self, left: u16, right: u16) -> i16 {
        let index = self.index(left, right);
        *unsafe { self.data.get_unchecked(index) }
    }

    pub fn update(&mut self, left: u16, right: u16, value: i16) {
        let index = self.index(left, right);
        self.data[index] = value;
    }

    pub fn num_left(&self) -> usize {
        self.num_left
    }

    pub fn num_right(&self) -> usize {
        self.num_right
    }
}
