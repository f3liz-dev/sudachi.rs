/*
 * Copyright (c) 2021 Works Applications Co., Ltd.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::sync::Mutex;
use std::iter::FusedIterator;

use crate::dic::lexicon_set::LexiconSet;
use crate::dic::read::u32_parser;
use crate::dic::read::word_info::WordInfoParser;
use crate::dic::subset::InfoSubset;
use crate::dic::word_id::WordId;
use crate::error::SudachiError;
use crate::prelude::*;

pub struct WordInfos<'a> {
    bytes: &'a [u8],
    offset: usize,
    _word_size: u32,
    has_synonym_group_ids: bool,
}

impl<'a> WordInfos<'a> {
    pub fn new(
        bytes: &'a [u8],
        offset: usize,
        _word_size: u32,
        has_synonym_group_ids: bool,
    ) -> WordInfos<'a> {
        WordInfos {
            bytes,
            offset,
            _word_size,
            has_synonym_group_ids,
        }
    }

    fn word_id_to_offset(&self, word_id: u32) -> SudachiResult<usize> {
        Ok(u32_parser(&self.bytes[self.offset + (4 * word_id as usize)..])?.1 as usize)
    }

    fn parse_word_info(&self, word_id: u32, subset: InfoSubset) -> SudachiResult<WordInfoData> {
        let index = self.word_id_to_offset(word_id)?;
        let parser = WordInfoParser::subset(subset);
        parser.parse(&self.bytes[index..])
    }

    pub fn get_word_info(&self, word_id: u32, mut subset: InfoSubset) -> SudachiResult<WordInfo> {
        if !self.has_synonym_group_ids {
            subset -= InfoSubset::SYNONYM_GROUP_ID;
        }

        let mut word_info = self.parse_word_info(word_id, subset)?;

        // consult dictionary form
        let dfwi = word_info.dictionary_form_word_id;
        if (dfwi >= 0) && (dfwi != word_id as i32) {
            let inner = self.parse_word_info(dfwi as u32, InfoSubset::SURFACE)?;
            word_info.dictionary_form = inner.surface;
        };

        Ok(word_info.into())
    }
}

/// Chunk-based decompressing WordInfos for .xdic format.
///
/// Layout of the data passed in:
///   [word_count * 4 bytes: offset index (absolute offsets into decompressed records)]
///   [zdict_len: u32][zdict bytes]
///   [chunk_count: u32][chunk_count * 4: chunk byte offsets][compressed chunks]
///
/// Each compressed chunk entry: [u32 compressed_len][compressed bytes]
pub struct ChunkedWordInfos {
    /// Offset index: word_id → absolute byte offset in decompressed records space
    wi_index: Vec<u32>,
    zdict: Vec<u8>,
    chunk_offsets: Vec<u32>,
    compressed_data: Vec<u8>,
    chunk_size: usize,
    has_synonym_group_ids: bool,
    /// LRU-1 cache: (chunk_index, decompressed_bytes)
    cache: Mutex<Option<(usize, Vec<u8>)>>,
}

impl ChunkedWordInfos {
    /// Parse from the raw bytes section of a .xdic file.
    /// `word_count` is the number of words (= wi_index entries).
    /// `chunk_size` is the uncompressed chunk size used during compression.
    pub fn new(
        data: &[u8],
        word_count: usize,
        chunk_size: usize,
        has_synonym_group_ids: bool,
    ) -> SudachiResult<Self> {
        let mut pos = 0;

        // offset index
        let index_bytes = word_count * 4;
        let wi_index: Vec<u32> = (0..word_count)
            .map(|i| u32::from_le_bytes(data[pos + i*4..pos + i*4 + 4].try_into().unwrap()))
            .collect();
        pos += index_bytes;

        // zdict
        let zdict_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let zdict = data[pos..pos + zdict_len].to_vec();
        pos += zdict_len;

        // chunk table
        let chunk_count = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let chunk_offsets: Vec<u32> = (0..chunk_count)
            .map(|i| u32::from_le_bytes(data[pos + i*4..pos + i*4 + 4].try_into().unwrap()))
            .collect();
        pos += chunk_count * 4;

        let compressed_data = data[pos..].to_vec();

        Ok(ChunkedWordInfos {
            wi_index,
            zdict,
            chunk_offsets,
            compressed_data,
            chunk_size,
            has_synonym_group_ids,
            cache: Mutex::new(None),
        })
    }

    fn decompress_chunk(&self, chunk_idx: usize) -> SudachiResult<Vec<u8>> {
        let start = self.chunk_offsets[chunk_idx] as usize;
        let clen = u32::from_le_bytes(
            self.compressed_data[start..start+4].try_into().unwrap()
        ) as usize;
        let compressed = &self.compressed_data[start + 4..start + 4 + clen];
        let mut dec = zstd::bulk::Decompressor::with_dictionary(&self.zdict)
            .map_err(|e| SudachiError::InvalidDataFormat(0, format!("zstd dict: {}", e)))?;
        dec.decompress(compressed, self.chunk_size * 2)
            .map_err(|e| SudachiError::InvalidDataFormat(
                0, format!("zstd decompress chunk {}: {}", chunk_idx, e)
            ))
    }

    fn get_record_bytes(&self, abs_offset: usize) -> SudachiResult<Vec<u8>> {
        let chunk_idx = abs_offset / self.chunk_size;
        let chunk_off = abs_offset % self.chunk_size;

        // Check cache
        {
            let cache = self.cache.lock().unwrap();
            if let Some((ci, ref bytes)) = *cache {
                if ci == chunk_idx {
                    return Ok(bytes[chunk_off..].to_vec());
                }
            }
        }

        let decompressed = self.decompress_chunk(chunk_idx)?;
        let result = decompressed[chunk_off..].to_vec();
        *self.cache.lock().unwrap() = Some((chunk_idx, decompressed));
        Ok(result)
    }

    fn parse_word_info(&self, word_id: u32, subset: InfoSubset) -> SudachiResult<WordInfoData> {
        let abs_offset = self.wi_index[word_id as usize] as usize;
        let record_bytes = self.get_record_bytes(abs_offset)?;
        let parser = WordInfoParser::subset(subset);
        parser.parse(&record_bytes)
    }

    pub fn get_word_info(&self, word_id: u32, mut subset: InfoSubset) -> SudachiResult<WordInfo> {
        if !self.has_synonym_group_ids {
            subset -= InfoSubset::SYNONYM_GROUP_ID;
        }
        let mut word_info = self.parse_word_info(word_id, subset)?;
        let dfwi = word_info.dictionary_form_word_id;
        if dfwi >= 0 && dfwi != word_id as i32 {
            let inner = self.parse_word_info(dfwi as u32, InfoSubset::SURFACE)?;
            word_info.dictionary_form = inner.surface;
        }
        Ok(word_info.into())
    }
}

/// Internal storage of the WordInfo.
/// It is not accessible by default, but a WordInfo can be created from it:
/// `let wi: WordInfo = data.into();`
///
/// String fields CAN be empty, in this case the value of the surface field should be used instead
#[derive(Clone, Debug, Default)]
pub struct WordInfoData {
    pub surface: String,
    pub head_word_length: u16,
    pub pos_id: u16,
    pub normalized_form: String,
    pub dictionary_form_word_id: i32,
    pub dictionary_form: String,
    pub reading_form: String,
    pub a_unit_split: Vec<WordId>,
    pub b_unit_split: Vec<WordId>,
    pub word_structure: Vec<WordId>,
    pub synonym_group_ids: Vec<u32>,
}

/// WordInfo API.
///
/// Internal data is not accessible by default, but can be extracted as
/// `let data: WordInfoData = info.into()`.
/// Note: this will consume WordInfo.
#[derive(Clone, Default)]
#[repr(transparent)]
pub struct WordInfo {
    data: WordInfoData,
}

impl WordInfo {
    pub fn surface(&self) -> &str {
        &self.data.surface
    }

    pub fn head_word_length(&self) -> usize {
        self.data.head_word_length as usize
    }

    pub fn pos_id(&self) -> u16 {
        self.data.pos_id
    }

    pub fn normalized_form(&self) -> &str {
        if self.data.normalized_form.is_empty() {
            self.surface()
        } else {
            &self.data.normalized_form
        }
    }

    pub fn dictionary_form_word_id(&self) -> i32 {
        self.data.dictionary_form_word_id
    }

    pub fn dictionary_form(&self) -> &str {
        if self.data.dictionary_form.is_empty() {
            self.surface()
        } else {
            &self.data.dictionary_form
        }
    }

    pub fn reading_form(&self) -> &str {
        if self.data.reading_form.is_empty() {
            self.surface()
        } else {
            &self.data.reading_form
        }
    }

    pub fn a_unit_split(&self) -> &[WordId] {
        &self.data.a_unit_split
    }

    pub fn b_unit_split(&self) -> &[WordId] {
        &self.data.b_unit_split
    }

    pub fn word_structure(&self) -> &[WordId] {
        &self.data.word_structure
    }

    pub fn synonym_group_ids(&self) -> &[u32] {
        &self.data.synonym_group_ids
    }

    pub fn borrow_data(&self) -> &WordInfoData {
        &self.data
    }
}

impl From<WordInfoData> for WordInfo {
    fn from(data: WordInfoData) -> Self {
        WordInfo { data }
    }
}

impl From<WordInfo> for WordInfoData {
    fn from(info: WordInfo) -> Self {
        info.data
    }
}

struct SplitIter<'a> {
    index: usize,
    split: &'a [WordId],
    lexicon: &'a LexiconSet<'a>,
}

impl Iterator for SplitIter<'_> {
    type Item = SudachiResult<WordInfo>;

    fn next(&mut self) -> Option<Self::Item> {
        let idx = self.index;
        if idx >= self.split.len() {
            None
        } else {
            self.index += 1;
            Some(self.lexicon.get_word_info(self.split[idx]))
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.split.len() - self.index;
        (rem, Some(rem))
    }
}

impl FusedIterator for SplitIter<'_> {}
