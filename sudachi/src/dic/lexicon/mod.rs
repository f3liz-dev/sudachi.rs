/*
 * Copyright (c) 2021-2024 Works Applications Co., Ltd.
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

use std::cmp;
use std::mem::size_of;

use crate::analysis::stateful_tokenizer::StatefulTokenizer;
use crate::analysis::stateless_tokenizer::DictionaryAccess;
use crate::dic::subset::InfoSubset;
use crate::dic::word_id::WordId;
use nom::{bytes::complete::take, number::complete::le_u32};

use crate::error::{SudachiError, SudachiNomResult};
use crate::prelude::*;

use self::trie::{Trie, TrieEntryIter};
use self::word_id_table::{WordIdTable, WordIdIter};
use self::word_infos::{WordInfo, WordInfos, ChunkedWordInfos};
use self::word_params::WordParams;
use self::xcdat_trie::{XcdatTrie, XcdatPrefixIter, XcdatEntry};

pub mod trie;
pub mod word_id_table;
pub mod word_infos;
pub mod word_params;
pub mod xcdat_trie;

/// The first 4 bits of word_id are used to indicate that from which lexicon
/// the word comes, thus we can only hold 15 lexicons in the same time.
/// 16th is reserved for marking OOVs.
pub const MAX_DICTIONARIES: usize = 15;

/// Dictionary lexicon
///
/// Contains trie, word_id, word_param, word_info
pub struct Lexicon<'a> {
    trie: TrieVariant<'a>,
    word_id_table: WordIdTable<'a>,
    word_params: WordParams<'a>,
    word_infos: WordInfosVariant<'a>,
    lex_id: u8,
}

enum TrieVariant<'a> {
    Yada(Trie<'a>),
    Xcdat(XcdatTrie),
}

enum WordInfosVariant<'a> {
    Flat(WordInfos<'a>),
    Chunked(ChunkedWordInfos),
}

// PhantomData to satisfy the 'a lifetime parameter when Chunked is used
use std::marker::PhantomData;
struct _PhantomLifetime<'a>(PhantomData<&'a ()>);

/// Result of the Lexicon lookup
#[derive(Eq, PartialEq, Debug)]
pub struct LexiconEntry {
    /// Id of the returned word
    pub word_id: WordId,
    /// Byte index of the word end
    pub end: usize,
}

impl LexiconEntry {
    pub fn new(word_id: WordId, end: usize) -> LexiconEntry {
        LexiconEntry { word_id, end }
    }
}

/// Type-erased iterator over lookup results for both trie variants.
pub enum LookupIter<Y, X>
where
    Y: Iterator<Item = LexiconEntry>,
    X: Iterator<Item = LexiconEntry>,
{
    Yada(Y),
    Xcdat(X),
}

impl<Y, X> Iterator for LookupIter<Y, X>
where
    Y: Iterator<Item = LexiconEntry>,
    X: Iterator<Item = LexiconEntry>,
{
    type Item = LexiconEntry;
    fn next(&mut self) -> Option<LexiconEntry> {
        match self {
            LookupIter::Yada(it) => it.next(),
            LookupIter::Xcdat(it) => it.next(),
        }
    }
}

impl<'a> Lexicon<'a> {
    const USER_DICT_COST_PER_MORPH: i32 = -20;

    pub fn parse(
        buf: &[u8],
        original_offset: usize,
        has_synonym_group_ids: bool,
    ) -> SudachiResult<(Lexicon, usize)> {
        let mut offset = original_offset;

        let (_rest, trie_size) = u32_parser_offset(buf, offset)?;
        offset += 4;
        let trie_array = trie_array_parser(buf, offset, trie_size)?;
        let trie = Trie::new(trie_array, trie_size as usize);
        offset += trie.total_size();

        let (_rest, word_id_table_size) = u32_parser_offset(buf, offset)?;
        let word_id_table = WordIdTable::new(buf, word_id_table_size, offset + 4);
        offset += word_id_table.storage_size();

        let (_rest, word_params_size) = u32_parser_offset(buf, offset)?;
        let word_params = WordParams::new(buf, word_params_size, offset + 4);
        offset += word_params.storage_size();

        let word_infos_offset = offset;
        let word_infos = WordInfos::new(buf, offset, word_params.size(), has_synonym_group_ids);

        Ok((Lexicon {
            trie: TrieVariant::Yada(trie),
            word_id_table,
            word_params,
            word_infos: WordInfosVariant::Flat(word_infos),
            lex_id: u8::MAX,
        }, word_infos_offset))
    }

    /// Parse a lexicon from the xcdat-compressed .xdic format sections.
    ///
    /// `xcdat_bytes`: raw xcdat trie bytes
    /// `rank_to_offset`: maps xcdat rank → word_id_table byte offset
    /// `rest`: word_id_table + word_params (uncompressed, as in original .dic)
    /// `chunked_wi`: the ChunkedWordInfos section bytes + word_count + chunk_size
    pub fn parse_xdic(
        xcdat_bytes: &[u8],
        rank_to_offset: &[u32],
        rest: &'a [u8],
        word_count: usize,
        chunked_wi_data: &[u8],
        chunk_size: usize,
        has_synonym_group_ids: bool,
    ) -> SudachiResult<Lexicon<'a>> {
        let trie = XcdatTrie::parse(xcdat_bytes, rank_to_offset.to_vec())?;

        // rest = [4: word_id_table_size][word_id_table][4: word_params_size][word_params]
        let mut offset = 0;
        let word_id_table_size = u32::from_le_bytes(rest[offset..offset+4].try_into()
            .map_err(|_| SudachiError::InvalidDataFormat(0, "xdic rest too short".into()))?) as u32;
        let word_id_table = WordIdTable::new(rest, word_id_table_size, offset + 4);
        offset += word_id_table.storage_size();

        let word_params_size = u32::from_le_bytes(rest[offset..offset+4].try_into()
            .map_err(|_| SudachiError::InvalidDataFormat(0, "xdic rest too short".into()))?) as u32;
        let word_params = WordParams::new(rest, word_params_size, offset + 4);

        let chunked = ChunkedWordInfos::new(chunked_wi_data, word_count, chunk_size, has_synonym_group_ids)?;

        Ok(Lexicon {
            trie: TrieVariant::Xcdat(trie),
            word_id_table,
            word_params,
            word_infos: WordInfosVariant::Chunked(chunked),
            lex_id: u8::MAX,
        })
    }

    /// Assign lexicon id to the current Lexicon
    pub fn set_dic_id(&mut self, id: u8) {
        assert!(id < MAX_DICTIONARIES as u8);
        self.lex_id = id
    }

    #[inline]
    fn word_id(&self, raw_id: u32) -> WordId {
        WordId::new(self.lex_id, raw_id)
    }

    /// Returns raw yada byte offsets for a surface (only valid for Yada trie variant).
    /// Used by the xcdat compressor to build the rank_to_offset table.
    pub fn lookup_raw(&self, input: &[u8], offset: usize) -> Option<u32> {
        match &self.trie {
            TrieVariant::Yada(trie) => trie.common_prefix_iterator(input, offset)
                .find(|e| e.end == input.len())
                .map(|e| e.value),
            TrieVariant::Xcdat(_) => panic!("lookup_raw only valid for Yada trie"),
        }
    }

    /// Returns an iterator of word_id and end of words that matches given input
    #[inline]
    pub fn lookup(
        &'a self,
        input: &'a [u8],
        offset: usize,
    ) -> impl Iterator<Item = LexiconEntry> + 'a {
        debug_assert!(self.lex_id < MAX_DICTIONARIES as u8);
        match &self.trie {
            TrieVariant::Yada(trie) => {
                LookupIter::Yada(
                    trie.common_prefix_iterator(input, offset)
                        .flat_map(move |e| {
                            self.word_id_table
                                .entries(e.value as usize)
                                .map(move |wid| LexiconEntry::new(self.word_id(wid), e.end))
                        })
                )
            }
            TrieVariant::Xcdat(trie) => {
                LookupIter::Xcdat(
                    trie.common_prefix_search(input, offset)
                        .flat_map(move |e| {
                            self.word_id_table
                                .entries(e.value as usize)
                                .map(move |wid| LexiconEntry::new(self.word_id(wid), e.end))
                        })
                )
            }
        }
    }

    /// Returns WordInfo for given word_id
    pub fn get_word_info(&self, word_id: u32, subset: InfoSubset) -> SudachiResult<WordInfo> {
        match &self.word_infos {
            WordInfosVariant::Flat(wi) => wi.get_word_info(word_id, subset),
            WordInfosVariant::Chunked(wi) => wi.get_word_info(word_id, subset),
        }
    }

    /// Returns word_param for given word_id.
    #[inline]
    pub fn get_word_param(&self, word_id: u32) -> (i16, i16, i16) {
        self.word_params.get_params(word_id)
    }

    pub fn update_cost<D: DictionaryAccess>(&mut self, dict: &D) -> SudachiResult<()> {
        let mut tok = StatefulTokenizer::create(dict, false, Mode::C);
        let mut ms = MorphemeList::empty(dict);
        for wid in 0..self.word_params.size() {
            if self.word_params.get_cost(wid) != i16::MIN {
                continue;
            }
            let wi = self.get_word_info(wid, InfoSubset::SURFACE)?;
            tok.reset().push_str(wi.surface());
            tok.do_tokenize()?;
            ms.collect_results(&mut tok)?;
            let internal_cost = ms.get_internal_cost();
            let cost = internal_cost + Lexicon::USER_DICT_COST_PER_MORPH * ms.len() as i32;
            let cost = cmp::min(cost, i16::MAX as i32);
            let cost = cmp::max(cost, i16::MIN as i32);
            self.word_params.set_cost(wid, cost as i16);
        }
        Ok(())
    }

    pub fn size(&self) -> u32 {
        self.word_params.size()
    }

    /// Size of the trie section in bytes (4-byte length prefix + trie data)
    pub fn trie_storage_size(&self) -> usize {
        match &self.trie {
            TrieVariant::Yada(t) => 4 + t.total_size(),
            TrieVariant::Xcdat(_) => 0,
        }
    }

    pub fn word_infos_layout(&self, dict_bytes: &[u8]) -> (usize, usize) {
        let word_count = self.word_params.size() as usize;
        let _ = dict_bytes;
        (word_count, word_count * 4)
    }
}

fn u32_parser_offset(input: &[u8], offset: usize) -> SudachiNomResult<&[u8], u32> {
    nom::sequence::preceded(take(offset), le_u32)(input)
}

fn trie_array_parser(input: &[u8], offset: usize, trie_size: u32) -> SudachiResult<&[u8]> {
    let trie_start = offset;
    let trie_end = offset + (trie_size as usize) * size_of::<u32>();
    if input.len() < trie_start {
        return Err(SudachiError::InvalidRange(trie_start, trie_end));
    }
    if input.len() < trie_end {
        return Err(SudachiError::InvalidRange(trie_start, trie_end));
    }
    let trie_data = &input[trie_start..trie_end];
    Ok(trie_data)
}
