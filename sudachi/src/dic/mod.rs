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

use std::path::Path;

use crate::analysis::stateless_tokenizer::DictionaryAccess;
use character_category::CharacterCategory;
use grammar::Grammar;
use header::Header;
use lexicon::Lexicon;
use lexicon_set::LexiconSet;

use crate::plugin::input_text::InputTextPlugin;
use crate::plugin::oov::OovProviderPlugin;
use crate::plugin::oov::mecab_oov::MeCabOovPlugin;
use crate::plugin::path_rewrite::PathRewritePlugin;
use crate::prelude::*;

#[cfg(feature = "build-dictionary")]
pub mod build;
pub mod category_type;
pub mod character_category;
pub mod connect;
pub mod dictionary;
pub mod grammar;
pub mod header;
pub mod lexicon;
pub mod lexicon_set;
pub mod read;
pub mod storage;
pub mod subset;
pub mod word_id;

const DEFAULT_CHAR_DEF_BYTES: &[u8] = include_bytes!("../../../resources/char.def");
const POS_DEPTH: usize = 6;

/// A dictionary consists of one system_dict and zero or more user_dicts
pub struct LoadedDictionary<'a> {
    pub grammar: Grammar<'a>,
    pub lexicon_set: LexiconSet<'a>,
    pub oov_providers: Vec<Box<dyn OovProviderPlugin + Sync + Send>>,
}

impl<'a> LoadedDictionary<'a> {
    /// Creates a system dictionary from bytes, and preloaded character category
    pub fn from_system_dictionary_and_chardef(
        dictionary_bytes: &'a [u8],
        character_category: CharacterCategory,
    ) -> SudachiResult<LoadedDictionary<'a>> {
        let system_dict = DictionaryLoader::read_system_dictionary(dictionary_bytes)?;

        let mut grammar = system_dict
            .grammar
            .ok_or(SudachiError::InvalidDictionaryGrammar)?;
        grammar.set_character_category(character_category);

        let num_system_pos = grammar.pos_list.len();
        Ok(LoadedDictionary {
            grammar,
            lexicon_set: LexiconSet::new(system_dict.lexicon, num_system_pos),
            oov_providers: vec![],
        })
    }

    /// Creates a system dictionary from bytes, and load a character category from file
    pub fn from_system_dictionary(
        dictionary_bytes: &'a [u8],
        character_category_file: &Path,
    ) -> SudachiResult<LoadedDictionary<'a>> {
        let character_category = CharacterCategory::from_file(character_category_file)?;
        Self::from_system_dictionary_and_chardef(dictionary_bytes, character_category)
    }

    /// Creates a system dictionary from bytes, and load embedded default character category
    pub fn from_system_dictionary_embedded(
        dictionary_bytes: &'a [u8],
    ) -> SudachiResult<LoadedDictionary<'a>> {
        let character_category = CharacterCategory::from_bytes(DEFAULT_CHAR_DEF_BYTES)?;
        Self::from_system_dictionary_and_chardef(dictionary_bytes, character_category)
    }

    /// Load from a .xdic compressed dictionary (xcdat trie + chunk-zstd word infos).
    ///
    /// Format (all lengths are u32 LE):
    ///   [512 header]
    ///   [4: grammar_zstd_len][grammar_zstd]
    ///   [4: xcdat_len][xcdat_bytes]
    ///   [4: rest_len][word_id_table + word_params — uncompressed]
    ///   [4: wi_index_len][wi_index — uncompressed, word_count*4 bytes]
    ///   [4: zdict_len][zdict]
    ///   [4: chunk_count][chunk_count*4: chunk_offsets][compressed_chunks]
    pub fn from_xdic(
        xdic_bytes: &'a [u8],
        character_category: CharacterCategory,
        chunk_size: usize,
    ) -> SudachiResult<LoadedDictionary<'a>> {
        use lexicon::Lexicon;

        let mut pos = 0;

        // header
        let header = Header::parse(&xdic_bytes[..Header::STORAGE_SIZE])?;
        pos += Header::STORAGE_SIZE;

        // grammar (zstd compressed with shared dict — read zdict first, then come back)
        let grammar_zstd_len = u32::from_le_bytes(xdic_bytes[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let grammar_zstd_start = pos;
        pos += grammar_zstd_len;

        // xcdat trie
        let xcdat_len = u32::from_le_bytes(xdic_bytes[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let xcdat_bytes = &xdic_bytes[pos..pos + xcdat_len];
        pos += xcdat_len;

        // rank_to_offset: xcdat rank → word_id_table byte offset
        let rank_to_offset_count = u32::from_le_bytes(xdic_bytes[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let rank_to_offset: Vec<u32> = (0..rank_to_offset_count)
            .map(|i| u32::from_le_bytes(xdic_bytes[pos + i*4..pos + i*4 + 4].try_into().unwrap()))
            .collect();
        pos += rank_to_offset_count * 4;

        // rest (word_id_table + word_params, uncompressed)
        let rest_len = u32::from_le_bytes(xdic_bytes[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let rest = &xdic_bytes[pos..pos + rest_len];
        pos += rest_len;

        // wi_index
        let wi_index_len = u32::from_le_bytes(xdic_bytes[pos..pos+4].try_into().unwrap()) as usize;
        let word_count = wi_index_len / 4;
        pos += 4;
        let wi_index_start = pos;
        pos += wi_index_len;

        // zdict
        let zdict_len = u32::from_le_bytes(xdic_bytes[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        let zdict = &xdic_bytes[pos..pos + zdict_len];

        // now decompress grammar with the zdict
        let grammar_zstd = &xdic_bytes[grammar_zstd_start..grammar_zstd_start + grammar_zstd_len];
        let grammar_raw = zstd::bulk::Decompressor::with_dictionary(zdict)
            .and_then(|mut d| d.decompress(grammar_zstd, 128 * 1024 * 1024))
            .map_err(|_| SudachiError::InvalidDictionaryGrammar)?;
        // grammar_raw is owned; leak it for 'static lifetime (WASM single-threaded, no drop needed)
        let grammar_bytes: &'static [u8] = Box::leak(grammar_raw.into_boxed_slice());
        let mut grammar = Grammar::parse(grammar_bytes, 0)?;
        grammar.set_character_category(character_category);
        let num_system_pos = grammar.pos_list.len();

        // Set up a default MeCabOovPlugin so arbitrary text can be tokenized
        let mut oov = MeCabOovPlugin::default();
        let oov_settings = serde_json::json!({});
        oov.set_up(&oov_settings, &Default::default(), &mut grammar)
            .map_err(|_| SudachiError::InvalidDictionaryGrammar)?;

        // chunked_wi_data: wi_index bytes followed by zdict_len+zdict+chunks
        let lexicon = Lexicon::parse_xdic(
            xcdat_bytes,
            &rank_to_offset,
            rest,
            word_count,
            &xdic_bytes[wi_index_start..],
            chunk_size,
            header.has_synonym_group_ids(),
        )?;

        Ok(LoadedDictionary {
            grammar,
            lexicon_set: LexiconSet::new(lexicon, num_system_pos),
            oov_providers: vec![Box::new(oov)],
        })
    }

    #[cfg(test)]
    pub(crate) fn merge_dictionary(
        mut self,
        other: DictionaryLoader<'a>,
    ) -> SudachiResult<LoadedDictionary<'a>> {
        let npos = self.grammar.pos_list.len();
        let lexicon = other.lexicon;
        let grammar = other.grammar;
        self.lexicon_set.append(lexicon, npos)?;
        if let Some(g) = grammar {
            self.grammar.merge(g)
        }
        Ok(self)
    }
}

impl<'a> DictionaryAccess for LoadedDictionary<'a> {
    fn grammar(&self) -> &Grammar<'a> {
        &self.grammar
    }

    fn lexicon(&self) -> &LexiconSet<'a> {
        &self.lexicon_set
    }

    fn input_text_plugins(&self) -> &[Box<dyn InputTextPlugin + Sync + Send>] {
        &[]
    }

    fn oov_provider_plugins(&self) -> &[Box<dyn OovProviderPlugin + Sync + Send>] {
        &self.oov_providers
    }

    fn path_rewrite_plugins(&self) -> &[Box<dyn PathRewritePlugin + Sync + Send>] {
        &[]
    }
}

/// A single system or user dictionary
pub struct DictionaryLoader<'a> {
    pub header: Header,
    pub grammar: Option<Grammar<'a>>,
    pub lexicon: Lexicon<'a>,
    /// Byte offset where the lexicon section starts in the dictionary bytes
    pub lexicon_offset: usize,
    /// Byte offset where word_infos starts (= after word_id_table + word_params)
    pub word_infos_offset: usize,
}

impl<'a> DictionaryLoader<'a> {
    /// Creates a binary dictionary from bytes
    ///
    /// # Safety
    /// This function is marked unsafe because it does not perform header validation
    pub unsafe fn read_any_dictionary(dictionary_bytes: &[u8]) -> SudachiResult<DictionaryLoader> {
        let header = Header::parse(&dictionary_bytes[..Header::STORAGE_SIZE])?;
        let mut offset = Header::STORAGE_SIZE;

        let grammar = if header.has_grammar() {
            let tmp = Grammar::parse(dictionary_bytes, offset)?;
            offset += tmp.storage_size;
            Some(tmp)
        } else {
            None
        };

        let lexicon_offset = offset;
        let (lexicon, word_infos_offset) = Lexicon::parse(dictionary_bytes, offset, header.has_synonym_group_ids())?;

        Ok(DictionaryLoader {
            header,
            grammar,
            lexicon,
            lexicon_offset,
            word_infos_offset,
        })
    }

    /// Creates a system binary dictionary from bytes
    ///
    /// Returns Err if header version is not match
    pub fn read_system_dictionary(dictionary_bytes: &[u8]) -> SudachiResult<DictionaryLoader> {
        let dict = unsafe { Self::read_any_dictionary(dictionary_bytes) }?;
        match dict.header.version {
            header::HeaderVersion::SystemDict(_) => Ok(dict),
            _ => Err(SudachiError::InvalidHeader(
                header::HeaderError::InvalidSystemDictVersion,
            )),
        }
    }

    /// Creates a user binary dictionary from bytes
    ///
    /// Returns Err if header version is not match
    pub fn read_user_dictionary(dictionary_bytes: &[u8]) -> SudachiResult<DictionaryLoader> {
        let dict = unsafe { Self::read_any_dictionary(dictionary_bytes) }?;
        match dict.header.version {
            header::HeaderVersion::UserDict(_) => Ok(dict),
            _ => Err(SudachiError::InvalidHeader(
                header::HeaderError::InvalidSystemDictVersion,
            )),
        }
    }

    pub fn to_loaded(self) -> Option<LoadedDictionary<'a>> {
        let mut lexicon = self.lexicon;
        lexicon.set_dic_id(0);
        match self.grammar {
            None => None,
            Some(grammar) => {
                let num_system_pos = grammar.pos_list.len();
                Some(LoadedDictionary {
                    grammar,
                    lexicon_set: LexiconSet::new(lexicon, num_system_pos),
                    oov_providers: vec![],
                })
            }
        }
    }
}

#[cfg(test)]
mod xdic_tests {
    use super::*;
    use crate::analysis::stateful_tokenizer::StatefulTokenizer;
    use crate::analysis::Mode;

    #[test]
    fn test_xdic_tokenize() {
        let bytes = std::fs::read("../../demo/system_core.xdic")
            .expect("demo/system_core.xdic not found");
        let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let char_cat = character_category::CharacterCategory::from_bytes(
            include_bytes!("../../../resources/char.def")
        ).unwrap();
        let dict = LoadedDictionary::from_xdic(bytes, char_cat, 65536)
            .expect("from_xdic failed");

        let mut tok = StatefulTokenizer::create(&dict, false, Mode::C);
        let mut morphemes = crate::prelude::MorphemeList::empty(&dict);
        tok.reset().push_str("日本語の形態素解析");
        tok.do_tokenize().expect("tokenize failed");
        morphemes.collect_results(&mut tok).expect("collect failed");

        assert!(morphemes.len() > 0);
        let surfaces: Vec<_> = morphemes.iter().map(|m| m.surface().to_string()).collect();
        println!("surfaces: {:?}", surfaces);
        assert!(surfaces.contains(&"日本語".to_string()));
    }
}
